//! Low-level wheel input observation for manual long screenshots.
//!
//! The hook callback only updates atomics. Capturing, matching and UI work must
//! stay on the long-screenshot worker so Windows never removes a slow hook.

use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(windows)]
use std::thread::JoinHandle;

/// A coherent cumulative view of wheel input observed by the monitor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScrollInputSnapshot {
    /// Increases once for every vertical or horizontal wheel message.
    pub sequence: u64,
    pub vertical_events: u64,
    pub horizontal_events: u64,
    /// Sum of the signed high words from `WM_MOUSEWHEEL` messages.
    pub vertical_delta: i64,
    /// Sum of the signed high words from `WM_MOUSEHWHEEL` messages.
    pub horizontal_delta: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollInputError {
    operation: &'static str,
    message: String,
}

impl ScrollInputError {
    fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    #[cfg(windows)]
    fn last_os_error(operation: &'static str) -> Self {
        Self::new(operation, std::io::Error::last_os_error().to_string())
    }
}

impl Display for ScrollInputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for ScrollInputError {}

#[derive(Debug, Clone, Copy)]
enum ScrollAxis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Default)]
struct SharedState {
    // Even values are stable snapshots; odd values mean the callback is
    // updating the counters. This small seqlock avoids a callback-side mutex.
    version: AtomicU64,
    vertical_events: AtomicU64,
    horizontal_events: AtomicU64,
    vertical_delta: AtomicI64,
    horizontal_delta: AtomicI64,
    running: AtomicBool,
}

impl SharedState {
    fn record(&self, axis: ScrollAxis, delta: i32) {
        self.version.fetch_add(1, Ordering::AcqRel);
        match axis {
            ScrollAxis::Vertical => {
                self.vertical_events.fetch_add(1, Ordering::Relaxed);
                self.vertical_delta
                    .fetch_add(i64::from(delta), Ordering::Relaxed);
            }
            ScrollAxis::Horizontal => {
                self.horizontal_events.fetch_add(1, Ordering::Relaxed);
                self.horizontal_delta
                    .fetch_add(i64::from(delta), Ordering::Relaxed);
            }
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> ScrollInputSnapshot {
        loop {
            let before = self.version.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let snapshot = ScrollInputSnapshot {
                sequence: before / 2,
                vertical_events: self.vertical_events.load(Ordering::Relaxed),
                horizontal_events: self.horizontal_events.load(Ordering::Relaxed),
                vertical_delta: self.vertical_delta.load(Ordering::Relaxed),
                horizontal_delta: self.horizontal_delta.load(Ordering::Relaxed),
            };
            if self.version.load(Ordering::Acquire) == before {
                return snapshot;
            }
        }
    }
}

/// Owns the low-level mouse hook and its dedicated Windows message thread.
///
/// `stop` is idempotent. Dropping a live monitor stops and joins the thread,
/// which unhooks before the thread-local callback state is released.
pub struct ScrollInputMonitor {
    shared: Arc<SharedState>,
    #[cfg(windows)]
    thread_id: Option<u32>,
    #[cfg(windows)]
    worker: Option<JoinHandle<Result<(), ScrollInputError>>>,
}

impl ScrollInputMonitor {
    #[cfg(not(windows))]
    const fn is_supported() -> bool {
        cfg!(windows)
    }

    pub fn start() -> Result<Self, ScrollInputError> {
        #[cfg(windows)]
        {
            return Self::start_windows();
        }

        #[cfg(not(windows))]
        {
            Ok(Self {
                shared: Arc::new(SharedState::default()),
            })
        }
    }

    pub fn snapshot(&self) -> ScrollInputSnapshot {
        self.shared.snapshot()
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }

    #[cfg(windows)]
    fn start_windows() -> Result<Self, ScrollInputError> {
        use std::sync::mpsc;

        let shared = Arc::new(SharedState::default());
        let worker_shared = Arc::clone(&shared);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("petaldesk-scroll-input".to_string())
            .spawn(move || run_hook_thread(worker_shared, ready_tx))
            .map_err(|error| ScrollInputError::new("spawn wheel hook thread", error.to_string()))?;

        match ready_rx.recv() {
            Ok(HookStartup::Ready(thread_id)) => Ok(Self {
                shared,
                thread_id: Some(thread_id),
                worker: Some(worker),
            }),
            Ok(HookStartup::Failed(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                let thread_error = match worker.join() {
                    Ok(Err(error)) => return Err(error),
                    Err(_) => "wheel hook thread panicked during startup".to_string(),
                    Ok(Ok(())) => error.to_string(),
                };
                Err(ScrollInputError::new(
                    "start wheel hook thread",
                    thread_error,
                ))
            }
        }
    }

    pub fn stop(&mut self) -> Result<(), ScrollInputError> {
        #[cfg(windows)]
        {
            return self.stop_windows();
        }

        #[cfg(not(windows))]
        {
            self.shared.running.store(false, Ordering::Release);
            Ok(())
        }
    }

    #[cfg(windows)]
    fn stop_windows(&mut self) -> Result<(), ScrollInputError> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};

        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        self.shared.running.store(false, Ordering::Release);
        let thread_id = self.thread_id.take().unwrap_or_default();
        if thread_id != 0 {
            // This is only a fast wake-up. The message loop also polls the
            // running flag, so a failed post can never make `join` unbounded.
            unsafe {
                PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
            }
        }

        let worker_result = worker.join().map_err(|_| {
            ScrollInputError::new("stop wheel hook thread", "wheel hook thread panicked")
        })?;
        worker_result
    }
}

impl Drop for ScrollInputMonitor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(windows)]
enum HookStartup {
    Ready(u32),
    Failed(ScrollInputError),
}

#[cfg(windows)]
thread_local! {
    static HOOK_STATE: std::cell::RefCell<Option<Arc<SharedState>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(windows)]
struct ThreadStateGuard;

#[cfg(windows)]
impl ThreadStateGuard {
    fn install(shared: Arc<SharedState>) -> Self {
        HOOK_STATE.with(|slot| *slot.borrow_mut() = Some(shared));
        Self
    }
}

#[cfg(windows)]
impl Drop for ThreadStateGuard {
    fn drop(&mut self) {
        let _ = HOOK_STATE.try_with(|slot| {
            if let Ok(mut state) = slot.try_borrow_mut() {
                state.take();
            }
        });
    }
}

#[cfg(windows)]
struct RunningGuard(Arc<SharedState>);

#[cfg(windows)]
impl RunningGuard {
    fn new(shared: Arc<SharedState>) -> Self {
        shared.running.store(true, Ordering::Release);
        Self(shared)
    }
}

#[cfg(windows)]
impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

#[cfg(windows)]
struct HookGuard(windows_sys::Win32::UI::WindowsAndMessaging::HHOOK);

#[cfg(windows)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(self.0);
        }
    }
}

#[cfg(windows)]
fn run_hook_thread(
    shared: Arc<SharedState>,
    ready: std::sync::mpsc::SyncSender<HookStartup>,
) -> Result<(), ScrollInputError> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, SetWindowsHookExW, TranslateMessage, MSG, PM_NOREMOVE,
        PM_REMOVE, WH_MOUSE_LL, WM_QUIT,
    };

    let thread_id = unsafe { GetCurrentThreadId() };
    let mut message = MSG::default();
    // A thread message cannot be posted until its message queue exists.
    unsafe {
        PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
    }

    let _state_guard = ThreadStateGuard::install(Arc::clone(&shared));
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module.is_null() {
        let error = ScrollInputError::last_os_error("resolve wheel hook module");
        let _ = ready.send(HookStartup::Failed(error.clone()));
        return Err(error);
    }
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), module, 0) };
    if hook.is_null() {
        let error = ScrollInputError::last_os_error("install low-level wheel hook");
        let _ = ready.send(HookStartup::Failed(error.clone()));
        return Err(error);
    }
    let _hook_guard = HookGuard(hook);
    let _running_guard = RunningGuard::new(Arc::clone(&shared));
    if ready.send(HookStartup::Ready(thread_id)).is_err() {
        return Ok(());
    }

    while shared.running.load(Ordering::Acquire) {
        let has_message =
            unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
        if has_message == 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        if message.message == WM_QUIT {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn low_level_mouse_proc(
    code: i32,
    message: windows_sys::Win32::Foundation::WPARAM,
    data: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, MSLLHOOKSTRUCT, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    };

    if code >= 0 && data != 0 {
        let axis = match message as u32 {
            WM_MOUSEWHEEL => Some(ScrollAxis::Vertical),
            WM_MOUSEHWHEEL => Some(ScrollAxis::Horizontal),
            _ => None,
        };
        if let Some(axis) = axis {
            let mouse = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
            let delta = wheel_delta(mouse.mouseData);
            if delta != 0 {
                let _ = HOOK_STATE.try_with(|slot| {
                    if let Ok(state) = slot.try_borrow() {
                        if let Some(state) = state.as_ref() {
                            state.record(axis, delta);
                        }
                    }
                });
            }
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

#[cfg(any(windows, test))]
fn wheel_delta(mouse_data: u32) -> i32 {
    i32::from((mouse_data >> 16) as u16 as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_tracks_both_axes_and_signed_deltas() {
        let state = SharedState::default();
        let initial = state.snapshot();
        state.record(ScrollAxis::Vertical, -120);
        state.record(ScrollAxis::Vertical, -30);
        state.record(ScrollAxis::Horizontal, 120);

        let snapshot = state.snapshot();
        assert_ne!(snapshot.sequence, initial.sequence);
        assert_eq!(snapshot.sequence, 3);
        assert_eq!(snapshot.vertical_events, 2);
        assert_eq!(snapshot.horizontal_events, 1);
        assert_eq!(snapshot.vertical_delta, -150);
        assert_eq!(snapshot.horizontal_delta, 120);
    }

    #[test]
    fn snapshot_is_coherent_while_events_are_recorded() {
        let state = Arc::new(SharedState::default());
        let writer = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            for index in 0..10_000 {
                let axis = if index % 2 == 0 {
                    ScrollAxis::Vertical
                } else {
                    ScrollAxis::Horizontal
                };
                writer.record(axis, 1);
            }
        });

        while !thread.is_finished() {
            let snapshot = state.snapshot();
            assert_eq!(
                snapshot.sequence,
                snapshot.vertical_events + snapshot.horizontal_events
            );
        }
        thread.join().unwrap();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.sequence, 10_000);
        assert_eq!(snapshot.vertical_events, 5_000);
        assert_eq!(snapshot.horizontal_events, 5_000);
    }

    #[test]
    fn wheel_delta_reads_the_signed_high_word() {
        assert_eq!(wheel_delta(120_u32 << 16), 120);
        assert_eq!(wheel_delta((-120_i16 as u16 as u32) << 16), -120);
        assert_eq!(wheel_delta(0x1234), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_hook_can_start_and_stop() {
        let mut monitor = ScrollInputMonitor::start().expect("wheel hook should start");
        assert!(monitor.is_running());
        monitor.stop().expect("wheel hook should stop");
        assert!(!monitor.is_running());
        monitor.stop().expect("stopping twice should stay harmless");
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_stub_is_inert_and_stoppable() {
        let mut monitor = ScrollInputMonitor::start().unwrap();
        assert!(!ScrollInputMonitor::is_supported());
        assert!(!monitor.is_running());
        assert_eq!(monitor.snapshot(), ScrollInputSnapshot::default());
        monitor.stop().unwrap();
        monitor.stop().unwrap();
    }
}

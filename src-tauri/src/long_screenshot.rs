use crate::browser_bridge::{
    BrowserBridge, BrowserBridgeStatus, BrowserConnectionStatus, BrowserFamily,
};
use crate::error::{AppError, AppResult};
use crate::long_screenshot_input::{ScrollInputMonitor, ScrollInputSnapshot};
use crate::phase_match::phase_offset_rgba;
use crate::screenshot::{
    self, MonitorBounds, ScreenshotExportAction, ScreenshotStore, CAPTURE_WINDOW_LABEL,
};
use crate::storage::{atomic_write, atomic_write_json, INTERNAL_DATA_DIR};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl,
    WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_LONG_HEIGHT: u32 = 100_000;
const MAX_LONG_PIXELS: u64 = 200_000_000;
const MAX_FRAME_PIXELS: u64 = 40_000_000;
const MAX_COPY_PIN_PIXELS: u64 = 16_000_000;
const DEFAULT_TILE_HEIGHT: u32 = 1_024;
const MAX_TILE_HEIGHT: u32 = 2_048;
const MAX_TILE_PIXELS: u64 = 16_000_000;
static CONTROL_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
const CONTROL_WINDOW_DESTROY_TIMEOUT: Duration = Duration::from_secs(3);
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(14);
const APP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const SETTLE_SAMPLE_INTERVAL: Duration = Duration::from_millis(90);
const SETTLE_MAX_SAMPLES: usize = 18;
const LOW_CONFIDENCE_LIMIT: u8 = 3;
const BROWSER_REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const BROWSER_MIN_OVERLAP_RATIO: f64 = 0.35;
const TOP_SCROLL_MAX_ATTEMPTS: usize = 64;
const TOP_SCROLL_NO_MOTION_CONFIRMATIONS: u8 = 2;
const CACHE_OWNER_FILE: &str = ".owner-pid";
const ANNOTATION_EXPORT_STRIP_HEIGHT: u32 = 1_024;
const ANNOTATION_EXPORT_TICKET_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ANNOTATION_STRIP_BYTES: usize = 64 * 1024 * 1024;
const ANNOTATION_EXPORT_TOKEN_HEADER: &str = "x-petaldesk-long-export-token";
const ANNOTATION_EXPORT_Y_HEADER: &str = "x-petaldesk-long-export-y";
const CONTROL_WINDOW_LABEL: &str = "screenshot-long-control";
const OUTLINE_WINDOW_LABEL: &str = "screenshot-long-outline";
const CONTROL_WINDOW_HEIGHT: u32 = 68;
const CONTROL_WINDOW_MAX_WIDTH: u32 = 680;
const MANUAL_SCROLL_STOP_CHECK_INTERVAL: Duration = Duration::from_millis(25);
const MANUAL_SCROLL_ACTIVE_CAPTURE_INTERVAL: Duration = Duration::from_millis(60);
const MANUAL_SCROLL_SETTLE_AFTER: Duration = Duration::from_millis(250);
const MANUAL_SCROLL_FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(60);
const MANUAL_SCROLL_FEEDBACK_AFTER: Duration = Duration::from_secs(3);
const MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER: Duration = Duration::from_secs(4);
const AUTO_SCROLL_END_CONFIRMATIONS: u8 = 3;
const AUTO_SCROLL_INITIAL_DELTA: i32 = 30;
const OUTLINE_MARGIN_LOGICAL: f64 = 6.0;
const MATCH_GRID_COLUMNS: usize = 8;
const MATCH_GRID_ROWS: usize = 6;
const MATCH_MIN_TEXTURED_BLOCKS: usize = 8;
const MATCH_MIN_HORIZONTAL_BANDS: usize = 2;
const MATCH_MIN_VERTICAL_BANDS: usize = 3;
const MATCH_MIN_OVERLAP_PERCENT: usize = 30;
const MATCH_SAMPLES_PER_BLOCK_AXIS: usize = 12;
const MOTION_MIN_SUPPORT_BLOCKS: usize = 4;
const MOTION_FINE_RADIUS_MAX: usize = 24;
const MATCH_CANDIDATE_GROUPS: usize = 32;
const MATCH_SELECTED_GROUPS: usize = 12;
const MATCH_VERIFICATION_GROUPS: usize = 8;
const MATCH_DISTRIBUTION_BANDS: usize = 6;
const CHANGE_SAMPLE_GROUPS: usize = 32;
const CHANGE_VERTICAL_BANDS: usize = 6;
const MATCH_MAX_COARSE_CANDIDATES: usize = 192;
const MATCH_MAX_COARSE_ROWS: usize = 192;
const MATCH_MAX_REFINE_SEEDS: usize = 4;
const MATCH_MAX_FINE_ROWS: usize = 384;

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LongCaptureEngine {
    BrowserEnhanced,
    Wheel,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LongCaptureState {
    Preparing,
    Capturing,
    Paused,
    Ready,
    Failed,
    Canceled,
}

impl LongCaptureState {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Canceled)
    }
}

fn transition_to_failed(state: &mut LongCaptureState) -> bool {
    if state.is_terminal() {
        return false;
    }
    *state = LongCaptureState::Failed;
    true
}

fn transition_to_canceled(state: &mut LongCaptureState) -> bool {
    if state.is_terminal() {
        return false;
    }
    *state = LongCaptureState::Canceled;
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LongCaptureScope {
    Selection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LongCaptureMode {
    Current,
    Top,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLongCaptureRequest {
    pub session_id: String,
    pub selection: PhysicalRect,
    pub scroll_anchor: PhysicalPoint,
    pub scope: LongCaptureScope,
    pub mode: LongCaptureMode,
    #[serde(default)]
    pub engine: Option<LongCaptureEngine>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongCaptureCapability {
    pub available: bool,
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub platform: String,
    pub engines: Vec<LongCaptureEngine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_engine: Option<LongCaptureEngine>,
    pub max_height: u32,
    pub max_pixels: u64,
    pub tile_height: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LongCaptureStatus {
    pub job_id: String,
    pub session_id: String,
    pub state: LongCaptureState,
    pub engine: LongCaptureEngine,
    pub frame_count: u32,
    pub width: u32,
    pub height: u32,
    pub message: String,
    pub can_undo: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongCaptureExportResult {
    pub action: ScreenshotExportAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_id: Option<String>,
    pub canceled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareLongCaptureAnnotationExportResult {
    pub canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    pub strip_height: u32,
}

struct LongCaptureAnnotationExportTicket {
    job: Arc<LongCaptureJob>,
    job_id: String,
    session_id: String,
    action: ScreenshotExportAction,
    save_path: Option<PathBuf>,
    directory: PathBuf,
    width: u32,
    height: u32,
    strip_height: u32,
    next_y: u32,
    issued_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LongCaptureManifest {
    schema_version: u32,
    job_id: String,
    session_id: String,
    state: LongCaptureState,
    engine: LongCaptureEngine,
    selection: PhysicalRect,
    scroll_anchor: PhysicalPoint,
    scope: LongCaptureScope,
    mode: LongCaptureMode,
    width: u32,
    height: u32,
    message: String,
    segments: Vec<LongCaptureSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fixed_bottom: Option<LongCaptureFixedBottom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LongCaptureSegment {
    index: u32,
    output_y: u32,
    height: u32,
    displacement: u32,
    confidence: f32,
    frame_file: String,
    strip_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LongCaptureFixedBottom {
    height: u32,
    file: String,
}

#[derive(Debug, Clone)]
struct CaptureTarget {
    bounds: PhysicalRect,
    scroll_anchor: PhysicalPoint,
    monitor: MonitorBounds,
    mode: LongCaptureMode,
    control_overlaps_roi: bool,
    scroll_windows: Option<ScrollTargetWindows>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollTargetWindows {
    root: isize,
    child: isize,
}

#[derive(Clone)]
struct BrowserCaptureContext {
    bridge: Arc<BrowserBridge>,
    family: BrowserFamily,
    connection_id: String,
    anchor_client_physical: PhysicalPoint,
}

#[derive(Debug, Clone, PartialEq)]
struct BrowserSessionBinding {
    tab_id: i64,
    frame_id: i64,
    session_id: String,
    device_pixel_ratio: f64,
}

struct LongCaptureRuntime {
    manifest: LongCaptureManifest,
    pause_requested: bool,
    cancel_requested: bool,
    finish_requested: bool,
    retry_current: bool,
    generation: u64,
    worker_done: bool,
    ready_emitted: bool,
    browser_active: bool,
    browser_restore_needed: bool,
    browser_session: Option<BrowserSessionBinding>,
}

struct LongCaptureJob {
    directory: PathBuf,
    target: CaptureTarget,
    control_window_instance: Option<isize>,
    hidden_pin_labels: Vec<String>,
    browser: Option<BrowserCaptureContext>,
    operation_lock: Mutex<()>,
    runtime: Mutex<LongCaptureRuntime>,
    wake: Condvar,
}

fn control_surface_needed(runtime: &LongCaptureRuntime) -> bool {
    runtime.manifest.state == LongCaptureState::Capturing
        && !runtime.pause_requested
        && !runtime.cancel_requested
        && !runtime.finish_requested
}

fn should_cleanup_after_worker(runtime: &LongCaptureRuntime) -> bool {
    runtime.cancel_requested || runtime.manifest.state == LongCaptureState::Canceled
}

fn should_cancel_after_control_destroy(state: LongCaptureState) -> bool {
    matches!(
        state,
        LongCaptureState::Preparing | LongCaptureState::Capturing | LongCaptureState::Paused
    )
}

fn wait_for_worker_done(job: &LongCaptureJob, timeout: Duration) -> bool {
    let runtime = lock_unpoisoned(&job.runtime);
    let (runtime, _) = job
        .wake
        .wait_timeout_while(runtime, timeout, |runtime| !runtime.worker_done)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    runtime.worker_done
}

fn complete_worker_after_cleanup(job: &LongCaptureJob, cleanup: impl FnOnce()) {
    // `worker_done` means all cache/window cleanup is complete. Export waits
    // without this lock, then reacquires it after the notification, so it can
    // never race a canceled worker deleting the same files.
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    cleanup();
    let mut runtime = lock_unpoisoned(&job.runtime);
    runtime.worker_done = true;
    job.wake.notify_all();
}

fn worker_shutdown_timeout_error() -> AppError {
    AppError::new(
        "long_capture_worker_shutdown_timeout",
        "长截图后台任务仍在结束，请稍后重试导出",
    )
}

impl LongCaptureJob {
    fn status(&self) -> LongCaptureStatus {
        status_from_runtime(&lock_unpoisoned(&self.runtime))
    }
}

pub struct LongScreenshotStore {
    cache_root: PathBuf,
    browser_bridge: Option<Arc<BrowserBridge>>,
    job: Mutex<Option<Arc<LongCaptureJob>>>,
    annotation_exports: Mutex<HashMap<String, LongCaptureAnnotationExportTicket>>,
    start_lock: Mutex<()>,
    pending_start: Mutex<Option<PendingLongCaptureStart>>,
    control_destroys: Mutex<ControlDestroyTracker>,
    control_destroyed: Condvar,
}

#[derive(Default)]
struct ControlDestroyTracker {
    expected: HashMap<isize, bool>,
    completed: HashSet<isize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingLongCaptureStart {
    session_id: String,
    cancel_requested: bool,
}

struct PendingLongCaptureStartGuard<'a> {
    store: &'a LongScreenshotStore,
    session_id: String,
}

impl Drop for PendingLongCaptureStartGuard<'_> {
    fn drop(&mut self) {
        self.store.finish_pending_start(&self.session_id);
    }
}

impl LongScreenshotStore {
    pub fn load(data_storage_path: &Path) -> AppResult<Self> {
        let cache_root = data_storage_path
            .join(INTERNAL_DATA_DIR)
            .join("cache")
            .join("screenshots")
            .join("long");
        std::fs::create_dir_all(&cache_root)
            .map_err(|error| AppError::io("创建长截图缓存目录", error))?;
        cleanup_orphaned_capture_directories(&cache_root);
        let annotation_export_root = cache_root.join(".annotation-exports");
        std::fs::create_dir_all(&annotation_export_root)
            .map_err(|error| AppError::io("创建长截图标注导出缓存目录", error))?;
        Ok(Self {
            cache_root,
            browser_bridge: BrowserBridge::start().ok().map(Arc::new),
            job: Mutex::new(None),
            annotation_exports: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            pending_start: Mutex::new(None),
            control_destroys: Mutex::new(ControlDestroyTracker::default()),
            control_destroyed: Condvar::new(),
        })
    }

    fn job(&self, expected_id: &str) -> AppResult<Arc<LongCaptureJob>> {
        lock_unpoisoned(&self.job)
            .as_ref()
            .filter(|job| job.status().job_id == expected_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("长截图任务不存在或已被替换"))
    }

    fn clear_job(&self, expected_id: &str) -> Option<Arc<LongCaptureJob>> {
        let mut current = lock_unpoisoned(&self.job);
        let matches = current
            .as_ref()
            .is_some_and(|job| job.status().job_id == expected_id);
        matches.then(|| current.take()).flatten()
    }

    fn job_is_current(&self, expected: &LongCaptureJob) -> bool {
        lock_unpoisoned(&self.job)
            .as_ref()
            .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), expected))
    }

    fn ensure_current_job(&self, expected: &LongCaptureJob) -> AppResult<()> {
        self.job_is_current(expected)
            .then_some(())
            .ok_or_else(|| AppError::not_found("长截图任务不存在或已被替换"))
    }

    fn begin_pending_start(&self, session_id: &str) -> AppResult<()> {
        let mut pending = lock_unpoisoned(&self.pending_start);
        if pending.is_some() {
            return Err(AppError::new(
                "long_capture_busy",
                "已有长截图正在启动，请稍候或先取消",
            ));
        }
        *pending = Some(PendingLongCaptureStart {
            session_id: session_id.to_string(),
            cancel_requested: false,
        });
        Ok(())
    }

    fn finish_pending_start(&self, session_id: &str) {
        let mut pending = lock_unpoisoned(&self.pending_start);
        if pending
            .as_ref()
            .is_some_and(|pending| pending.session_id == session_id)
        {
            *pending = None;
        }
    }

    fn request_pending_start_cancel(&self, session_id: &str) -> bool {
        let mut pending = lock_unpoisoned(&self.pending_start);
        let Some(pending) = pending
            .as_mut()
            .filter(|pending| pending.session_id == session_id)
        else {
            return false;
        };
        pending.cancel_requested = true;
        true
    }

    fn pending_start_cancel_requested(&self, session_id: &str) -> bool {
        lock_unpoisoned(&self.pending_start)
            .as_ref()
            .is_some_and(|pending| pending.session_id == session_id && pending.cancel_requested)
    }

    fn job_for_session(&self, session_id: &str) -> Option<Arc<LongCaptureJob>> {
        lock_unpoisoned(&self.job)
            .as_ref()
            .filter(|job| job.status().session_id == session_id)
            .cloned()
    }

    fn expect_control_destroy(&self, instance_id: isize, wait_for_completion: bool) -> bool {
        let mut tracker = lock_unpoisoned(&self.control_destroys);
        if tracker.completed.contains(&instance_id) {
            return false;
        }
        match tracker.expected.entry(instance_id) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                *entry.get_mut() |= wait_for_completion;
                false
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(wait_for_completion);
                true
            }
        }
    }

    fn revoke_expected_control_destroy(&self, instance_id: isize) {
        let mut tracker = lock_unpoisoned(&self.control_destroys);
        tracker.expected.remove(&instance_id);
        tracker.completed.remove(&instance_id);
        self.control_destroyed.notify_all();
    }

    fn consume_expected_control_destroy(
        &self,
        instance_id: Option<isize>,
        current_instance: Option<isize>,
    ) -> bool {
        let mut tracker = lock_unpoisoned(&self.control_destroys);
        let instance_id = match instance_id {
            Some(instance_id) => instance_id,
            None if current_instance.is_none() && tracker.expected.len() == 1 => *tracker
                .expected
                .keys()
                .next()
                .expect("one expected control destroy"),
            // Never guess while a replacement window is registered. A stale
            // unidentifiable event must not consume that window's expectation.
            None => return false,
        };
        let Some(wait_for_completion) = tracker.expected.remove(&instance_id) else {
            return false;
        };
        if wait_for_completion {
            tracker.completed.insert(instance_id);
            self.control_destroyed.notify_all();
        }
        true
    }

    fn wait_for_control_destroy(&self, instance_id: isize, timeout: Duration) -> bool {
        let mut tracker = lock_unpoisoned(&self.control_destroys);
        let deadline = Instant::now() + timeout;
        loop {
            if tracker.completed.remove(&instance_id) {
                return true;
            }
            if !tracker.expected.contains_key(&instance_id) {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                if let Some(wait_for_completion) = tracker.expected.get_mut(&instance_id) {
                    *wait_for_completion = false;
                }
                return false;
            };
            let waited = self
                .control_destroyed
                .wait_timeout(tracker, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            tracker = waited.0;
            if waited.1.timed_out() && !tracker.completed.contains(&instance_id) {
                if let Some(wait_for_completion) = tracker.expected.get_mut(&instance_id) {
                    *wait_for_completion = false;
                }
                return false;
            }
        }
    }
}

fn clear_job_cache(store: &LongScreenshotStore, expected_id: &str) {
    if let Some(job) = store.clear_job(expected_id) {
        let _ = std::fs::remove_dir_all(&job.directory);
    }
    let directories = {
        let mut tickets = lock_unpoisoned(&store.annotation_exports);
        let matching = tickets
            .iter()
            .filter(|(_, ticket)| ticket.job_id == expected_id)
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        matching
            .into_iter()
            .filter_map(|token| tickets.remove(&token).map(|ticket| ticket.directory))
            .collect::<Vec<_>>()
    };
    for directory in directories {
        let _ = std::fs::remove_dir_all(directory);
    }
}

fn clear_all_annotation_exports(store: &LongScreenshotStore) {
    let directories = lock_unpoisoned(&store.annotation_exports)
        .drain()
        .map(|(_, ticket)| ticket.directory)
        .collect::<Vec<_>>();
    for directory in directories {
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// Best-effort cleanup for the real application exit path. This is separate
/// from hiding to the tray: callers should invoke it only when the process is
/// actually terminating.
pub(crate) fn shutdown(app: &AppHandle) {
    let Some(store) = app.try_state::<LongScreenshotStore>() else {
        return;
    };
    if let Some(pending) = lock_unpoisoned(&store.pending_start).as_mut() {
        pending.cancel_requested = true;
    }
    let job = lock_unpoisoned(&store.job).as_ref().cloned();
    let worker_stopped = if let Some(job) = job.as_ref() {
        let outcome = request_job_cancel(job, "应用退出，长截图已取消");
        if let Some(error) = outcome.persistence_error {
            eprintln!("保存长截图退出状态失败: {error}");
        }
        let stopped = outcome.cleanup_now || wait_for_worker_done(job, APP_SHUTDOWN_TIMEOUT);
        if stopped {
            clear_job_cache(&store, &outcome.status.job_id);
        } else {
            // The process is already exiting. Keep the directory intact while
            // a worker may still be writing; startup orphan cleanup owns it
            // after this process has gone away.
            eprintln!("长截图后台任务未在退出超时内结束，保留缓存供下次启动清理");
        }
        let _ = restore_browser_session(job);
        restore_hidden_pin_windows(app, &job.hidden_pin_labels);
        stopped
    } else {
        true
    };
    let _ = close_control_window(app);
    clear_all_annotation_exports(&store);
    if worker_stopped {
        cleanup_cache_directories_owned_by(&store.cache_root, std::process::id());
    }
}

fn cleanup_cache_directories_owned_by(root: &Path, process_id: u32) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if entry.file_name() == ".annotation-exports" {
            cleanup_cache_directories_owned_by(&path, process_id);
        } else if cache_owner_process_id(&path) == Some(process_id) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn cleanup_orphaned_capture_directories(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if entry.file_name() == ".annotation-exports" {
            cleanup_orphaned_capture_directories(&path);
        } else if !cache_owner_is_running(&path) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn write_cache_owner(directory: &Path) -> AppResult<()> {
    atomic_write(
        &directory.join(CACHE_OWNER_FILE),
        std::process::id().to_string().as_bytes(),
    )
}

fn cache_owner_is_running(directory: &Path) -> bool {
    cache_owner_process_id(directory).is_some_and(process_is_running)
}

fn cache_owner_process_id(directory: &Path) -> Option<u32> {
    std::fs::read_to_string(directory.join(CACHE_OWNER_FILE))
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

#[cfg(windows)]
fn process_is_running(process_id: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

    if process_id == std::process::id() {
        return true;
    }
    let process = unsafe { OpenProcess(SYNCHRONIZE_ACCESS, 0, process_id) };
    if process.is_null() {
        return false;
    }
    let result = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    unsafe {
        let _ = CloseHandle(process);
    }
    result
}

#[cfg(not(windows))]
fn process_is_running(process_id: u32) -> bool {
    process_id == std::process::id()
}

fn status_from_runtime(runtime: &LongCaptureRuntime) -> LongCaptureStatus {
    LongCaptureStatus {
        job_id: runtime.manifest.job_id.clone(),
        session_id: runtime.manifest.session_id.clone(),
        state: runtime.manifest.state,
        engine: runtime.manifest.engine,
        frame_count: runtime.manifest.segments.len() as u32,
        width: runtime.manifest.width,
        height: runtime.manifest.height,
        message: runtime.manifest.message.clone(),
        can_undo: runtime.manifest.segments.len() > 1,
    }
}

#[tauri::command]
pub fn get_long_capture_capability(store: State<'_, LongScreenshotStore>) -> LongCaptureCapability {
    long_capture_capability(store.browser_bridge.as_deref())
}

fn long_capture_capability(bridge: Option<&BrowserBridge>) -> LongCaptureCapability {
    let available = cfg!(windows);
    let browser_enhanced = available
        && bridge
            .and_then(|bridge| bridge.status().ok())
            .is_some_and(|status| browser_statuses(&status).iter().any(|status| status.ready));
    let mut engines = if available {
        vec![LongCaptureEngine::Manual, LongCaptureEngine::Wheel]
    } else {
        Vec::new()
    };
    if browser_enhanced {
        engines.insert(1, LongCaptureEngine::BrowserEnhanced);
    }
    LongCaptureCapability {
        available,
        supported: available,
        reason: (!available).then(|| "长截图通用引擎仅支持 Windows 10/11".to_string()),
        platform: std::env::consts::OS.to_string(),
        engines,
        preferred_engine: available.then_some(LongCaptureEngine::Manual),
        max_height: MAX_LONG_HEIGHT,
        max_pixels: MAX_LONG_PIXELS,
        tile_height: DEFAULT_TILE_HEIGHT,
    }
}

fn browser_statuses(status: &BrowserBridgeStatus) -> [&BrowserConnectionStatus; 3] {
    [&status.chrome, &status.edge, &status.firefox]
}

fn pending_start_canceled_error() -> AppError {
    AppError::new(
        "long_capture_start_canceled",
        "长截图启动已取消，已恢复普通截图界面",
    )
}

#[tauri::command]
pub async fn start_long_capture(
    app: AppHandle,
    request: StartLongCaptureRequest,
) -> AppResult<LongCaptureStatus> {
    // Creating the control WebView synchronously dispatches to Tauri's event
    // loop. Never run that path inline in the IPC callback that owns the loop.
    tauri::async_runtime::spawn_blocking(move || start_long_capture_inner(&app, request))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("启动长截图任务异常结束: {error}"),
            )
        })?
}

fn start_long_capture_inner(
    app: &AppHandle,
    request: StartLongCaptureRequest,
) -> AppResult<LongCaptureStatus> {
    if !cfg!(windows) {
        return Err(AppError::new(
            "unsupported_platform",
            "长截图通用引擎仅支持 Windows 10/11",
        ));
    }
    let store = app.state::<LongScreenshotStore>();
    let screenshot_store = app.state::<ScreenshotStore>();
    // Register the pending long-capture owner under the same lock used by
    // ordinary screenshot start/cancel. Once pending is visible, cancellation
    // can reliably stop either the pending transition or the published job.
    let screenshot_start_guard = screenshot_store.lock_start();
    let session = screenshot_store
        .active_session()
        .filter(|session| session.id == request.session_id)
        .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?;
    let session_id = session.id.clone();
    store.begin_pending_start(&session_id)?;
    drop(screenshot_start_guard);
    let _pending_guard = PendingLongCaptureStartGuard {
        store: &store,
        session_id,
    };
    let _start_guard = lock_unpoisoned(&store.start_lock);
    start_long_capture_registered_inner(app, &store, request, session)
}

fn start_long_capture_registered_inner(
    app: &AppHandle,
    store: &LongScreenshotStore,
    request: StartLongCaptureRequest,
    session: crate::screenshot::ScreenshotSession,
) -> AppResult<LongCaptureStatus> {
    purge_expired_annotation_exports(store);
    if store.pending_start_cancel_requested(&session.id) {
        return Err(pending_start_canceled_error());
    }
    let mut target = validate_capture_target(
        &session.monitor,
        request.selection,
        request.scroll_anchor,
        request.mode,
    )?;
    let requested_engine = request.engine;

    let previous_job = lock_unpoisoned(&store.job).as_ref().cloned();
    // A stale command can already hold an Arc to the completed job. Serialize
    // replacement with that job's operation lock, then make every command
    // revalidate ownership after taking the same lock.
    let _previous_operation_guard = previous_job
        .as_ref()
        .map(|job| lock_unpoisoned(&job.operation_lock));
    let previous_job_id = if let Some(job) = previous_job.as_ref() {
        if !store.job_is_current(job) {
            None
        } else {
            let runtime = lock_unpoisoned(&job.runtime);
            if !runtime.worker_done {
                return Err(AppError::new(
                    "long_capture_busy",
                    "已有长截图任务正在运行，请先完成或取消",
                ));
            }
            let job_id = runtime.manifest.job_id.clone();
            drop(runtime);
            if lock_unpoisoned(&store.annotation_exports)
                .values()
                .any(|ticket| ticket.job_id == job_id)
            {
                return Err(AppError::new(
                    "long_capture_busy",
                    "长截图标注仍在导出，请等待导出完成或取消导出",
                ));
            }
            Some(job_id)
        }
    } else {
        None
    };
    if let Some(previous_job_id) = previous_job_id {
        clear_job_cache(&store, &previous_job_id);
    }

    let job_id = Uuid::new_v4().to_string();
    let directory = store.cache_root.join(&job_id);
    std::fs::create_dir_all(directory.join("frames"))
        .and_then(|_| std::fs::create_dir_all(directory.join("strips")))
        .map_err(|error| AppError::io("创建长截图任务目录", error))?;
    if let Err(error) = write_cache_owner(&directory) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    // Building a WebView can synchronously dispatch to Tauri's window thread.
    // Do it while the ordinary screenshot overlay is still visible and before
    // publishing the job, so a slow WebView startup never leaves the desktop
    // behind a hidden, non-responsive capture surface. Recreate it for every
    // job because the job id is part of the control page URL.
    let control_window_instance =
        match prepare_control_window(app, &target.monitor, target.bounds, &job_id) {
            Ok(instance) => instance,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&directory);
                return Err(error);
            }
        };
    if let Err(error) = prepare_outline_window(app, &target.monitor, target.bounds) {
        let _ = close_control_window(app);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    if store.pending_start_cancel_requested(&session.id) {
        let _ = close_control_window(app);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(pending_start_canceled_error());
    }
    let hidden_pin_labels = hide_visible_pin_windows(&app);
    if let Err(error) = hide_capture_overlay(&app) {
        restore_hidden_pin_windows(&app, &hidden_pin_labels);
        let _ = close_control_window(app);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    flush_desktop_compositor();
    std::thread::sleep(Duration::from_millis(60));
    if store.pending_start_cancel_requested(&session.id) {
        restore_hidden_pin_windows(app, &hidden_pin_labels);
        let error = recover_capture_overlay(app, &session.monitor, pending_start_canceled_error());
        let _ = close_control_window(app);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    target.scroll_windows = resolve_scroll_target_windows(target.scroll_anchor);
    let browser = if request.mode != LongCaptureMode::Manual
        && !matches!(
            requested_engine,
            Some(LongCaptureEngine::Wheel | LongCaptureEngine::Manual)
        ) {
        select_browser_capture_context(store.browser_bridge.as_ref(), target.scroll_anchor)
    } else {
        None
    };
    let engine = if request.mode == LongCaptureMode::Manual
        || requested_engine == Some(LongCaptureEngine::Manual)
    {
        LongCaptureEngine::Manual
    } else if browser.is_some() {
        LongCaptureEngine::BrowserEnhanced
    } else {
        LongCaptureEngine::Wheel
    };
    let preparing_message =
        if requested_engine == Some(LongCaptureEngine::BrowserEnhanced) && browser.is_none() {
            "浏览器增强不可用，正在切换通用滚动".to_string()
        } else if engine == LongCaptureEngine::BrowserEnhanced {
            "正在连接浏览器增强引擎".to_string()
        } else if engine == LongCaptureEngine::Manual {
            "正在准备手动长截图".to_string()
        } else {
            "正在准备长截图".to_string()
        };
    let manifest = LongCaptureManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        job_id: job_id.clone(),
        session_id: request.session_id,
        state: LongCaptureState::Preparing,
        engine,
        selection: request.selection,
        scroll_anchor: request.scroll_anchor,
        scope: request.scope,
        mode: request.mode,
        width: request.selection.width,
        height: 0,
        message: preparing_message,
        segments: Vec::new(),
        fixed_bottom: None,
    };
    if let Err(error) = atomic_write_json(&directory.join("manifest.json"), &manifest) {
        restore_hidden_pin_windows(&app, &hidden_pin_labels);
        let error = recover_capture_overlay(&app, &session.monitor, error);
        let _ = close_control_window(app);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    let job = Arc::new(LongCaptureJob {
        directory,
        target,
        control_window_instance,
        hidden_pin_labels,
        browser,
        operation_lock: Mutex::new(()),
        runtime: Mutex::new(LongCaptureRuntime {
            manifest,
            pause_requested: false,
            cancel_requested: false,
            finish_requested: false,
            retry_current: false,
            generation: 0,
            worker_done: false,
            ready_emitted: false,
            browser_active: false,
            browser_restore_needed: false,
            browser_session: None,
        }),
        wake: Condvar::new(),
    });
    *lock_unpoisoned(&store.job) = Some(Arc::clone(&job));

    let initial_surface_result = {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        let result = show_control_window(&app, &job).and_then(|_| focus_capture_target(&job));
        if let Err(error) = result.as_ref() {
            let mut runtime = lock_unpoisoned(&job.runtime);
            if transition_to_failed(&mut runtime.manifest.state) {
                runtime.manifest.message = error.message.clone();
            }
            runtime.worker_done = true;
            let _ = persist_runtime(&job, &runtime);
        }
        result
    };
    if let Err(error) = initial_surface_result {
        restore_hidden_pin_windows(&app, &job.hidden_pin_labels);
        let recovery = switch_visible_surface(
            || show_capture_overlay(&app, &job.target.monitor),
            || close_control_window(&app),
        );
        let failure = match recovery {
            Ok(()) => error,
            Err(surface_error) => recover_capture_surface(
                &app,
                &job,
                append_recovery_error(error, "恢复普通截图界面失败", &surface_error),
            ),
        };
        clear_job_cache(store, &job_id);
        return Err(failure);
    }
    if store.pending_start_cancel_requested(&session.id) {
        let status = {
            let mut runtime = lock_unpoisoned(&job.runtime);
            runtime.cancel_requested = true;
            runtime.pause_requested = false;
            runtime.generation = runtime.generation.wrapping_add(1);
            let _ = transition_to_canceled(&mut runtime.manifest.state);
            runtime.manifest.message = "长截图启动已取消".to_string();
            runtime.worker_done = true;
            let _ = persist_runtime(&job, &runtime);
            status_from_runtime(&runtime)
        };
        job.wake.notify_all();
        restore_hidden_pin_windows(app, &job.hidden_pin_labels);
        let surface_result = switch_visible_surface(
            || show_capture_overlay(app, &job.target.monitor),
            || close_control_window(app),
        );
        clear_job_cache(store, &status.job_id);
        surface_result?;
        return Ok(status);
    }
    flush_desktop_compositor();
    let status = job.status();
    let worker_app = app.clone();
    let worker_job = Arc::clone(&job);
    let worker_start = {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state.is_terminal() {
            runtime.worker_done = true;
            let _ = persist_runtime(&job, &runtime);
            Ok(Some(status_from_runtime(&runtime)))
        } else {
            drop(runtime);
            match std::thread::Builder::new()
                .name(format!("long-capture-{}", &job_id[..8]))
                .spawn(move || capture_worker(worker_app, worker_job))
            {
                Ok(_) => Ok(None),
                Err(error) => {
                    let failure =
                        AppError::new("capture_error", format!("启动长截图线程失败: {error}"));
                    let mut runtime = lock_unpoisoned(&job.runtime);
                    if transition_to_failed(&mut runtime.manifest.state) {
                        runtime.manifest.message = failure.message.clone();
                    }
                    runtime.worker_done = true;
                    let _ = persist_runtime(&job, &runtime);
                    Err(failure)
                }
            }
        }
    };
    match worker_start {
        Ok(None) => {}
        Ok(Some(terminal_status)) => {
            restore_hidden_pin_windows(&app, &job.hidden_pin_labels);
            let surface_result = switch_visible_surface(
                || show_capture_overlay(&app, &job.target.monitor),
                || close_control_window(&app),
            );
            clear_job_cache(store, &job_id);
            surface_result?;
            return Ok(terminal_status);
        }
        Err(failure) => {
            restore_hidden_pin_windows(&app, &job.hidden_pin_labels);
            let failure = match switch_visible_surface(
                || show_capture_overlay(&app, &job.target.monitor),
                || close_control_window(&app),
            ) {
                Ok(()) => failure,
                Err(surface_error) => recover_capture_surface(
                    &app,
                    &job,
                    append_recovery_error(failure, "恢复截图界面失败", &surface_error),
                ),
            };
            clear_job_cache(store, &job_id);
            return Err(failure);
        }
    }
    let _ = app.emit("long_capture_progress", &status);
    Ok(status)
}

#[tauri::command]
pub async fn get_long_capture_status(
    store: State<'_, LongScreenshotStore>,
    job_id: Option<String>,
) -> AppResult<Option<LongCaptureStatus>> {
    let job = lock_unpoisoned(&store.job).as_ref().cloned();
    let Some(job) = job else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let status = job.status();
        if job_id
            .as_deref()
            .is_some_and(|expected| expected != status.job_id)
        {
            return Err(AppError::not_found("长截图任务不存在或已被替换"));
        }
        Ok(Some(status))
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_status_error",
            format!("读取长截图状态异常结束: {error}"),
        )
    })?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LongCaptureReentrySurface {
    Pending,
    Control,
    Overlay,
}

fn long_capture_reentry_surface(
    pending_start: bool,
    state: Option<LongCaptureState>,
    engine: Option<LongCaptureEngine>,
) -> Option<LongCaptureReentrySurface> {
    if pending_start {
        return Some(LongCaptureReentrySurface::Pending);
    }
    match state? {
        LongCaptureState::Preparing | LongCaptureState::Capturing => {
            Some(LongCaptureReentrySurface::Control)
        }
        LongCaptureState::Paused if engine == Some(LongCaptureEngine::Manual) => {
            Some(LongCaptureReentrySurface::Control)
        }
        LongCaptureState::Paused | LongCaptureState::Ready | LongCaptureState::Failed => {
            Some(LongCaptureReentrySurface::Overlay)
        }
        LongCaptureState::Canceled => None,
    }
}

fn screenshot_session_owns_job(active_session_id: Option<&str>, job_session_id: &str) -> bool {
    active_session_id.is_some_and(|active| active == job_session_id)
}

pub(crate) fn restore_active_long_capture_surface(app: &AppHandle) -> AppResult<bool> {
    let Some(store) = app.try_state::<LongScreenshotStore>() else {
        return Ok(false);
    };
    let pending_session_id = lock_unpoisoned(&store.pending_start)
        .as_ref()
        .map(|pending| pending.session_id.clone());
    if let Some(session_id) = pending_session_id {
        // Reentry while WebView construction is pending must remain actionable.
        // Cancel the pending transition before restoring the ordinary overlay,
        // otherwise the starter could hide it again immediately afterwards.
        store.request_pending_start_cancel(&session_id);
        if let Some(session) = app
            .state::<ScreenshotStore>()
            .active_session()
            .filter(|session| session.id == session_id)
        {
            show_capture_overlay(app, &session.monitor)?;
        }
        return Ok(true);
    }
    let Some(job) = lock_unpoisoned(&store.job).as_ref().cloned() else {
        return Ok(false);
    };
    let owner_session_id = job.status().session_id;
    let active_session = app.state::<ScreenshotStore>().active_session();
    let owner_session_is_active = screenshot_session_owns_job(
        active_session.as_ref().map(|session| session.id.as_str()),
        &owner_session_id,
    );
    if !owner_session_is_active {
        // A capture WebView crash or an external session close must not leave a
        // job that consumes every future screenshot shortcut with "busy".
        if let Err(error) = cancel_for_screenshot_session_end(app, &owner_session_id) {
            eprintln!("清理失去截图会话的长截图任务失败: {error}");
            let _ = app.emit("screenshot_capture_error", error);
        }
        return Ok(false);
    }
    let (surface, surface_result) = {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        let status = job.status();
        let Some(surface) =
            long_capture_reentry_surface(false, Some(status.state), Some(status.engine))
        else {
            return Ok(false);
        };
        let result = match surface {
            LongCaptureReentrySurface::Pending => Ok(()),
            LongCaptureReentrySurface::Control => {
                // Frame capture uses the same operation lock while the control
                // is hidden. Reentry waits for that bounded transaction.
                show_control_window(app, &job)
            }
            LongCaptureReentrySurface::Overlay => show_capture_overlay(app, &job.target.monitor),
        };
        (surface, result)
    };
    if let Err(surface_error) = surface_result {
        if surface == LongCaptureReentrySurface::Control
            && surface_error.code == "long_capture_control_missing"
        {
            return match cancel_long_capture_job_inner(app, &store, &job) {
                Ok(_) => Ok(true),
                Err(cancel_error) => Err(append_recovery_error(
                    surface_error,
                    "取消失去控制窗口的长截图失败",
                    &cancel_error,
                )),
            };
        }
        return Err(surface_error);
    }
    Ok(true)
}

#[tauri::command]
pub async fn pause_long_capture(app: AppHandle, job_id: String) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || pause_long_capture_inner(&app, &job_id))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("暂停长截图任务异常结束: {error}"),
            )
        })?
}

fn pause_long_capture_inner(app: &AppHandle, job_id: &str) -> AppResult<LongCaptureStatus> {
    let store = app.state::<LongScreenshotStore>();
    let job = store.job(&job_id)?;
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    let status = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state != LongCaptureState::Capturing {
            return Err(AppError::new(
                "invalid_long_capture_state",
                "当前长截图任务无法暂停",
            ));
        }
        runtime.pause_requested = true;
        runtime.manifest.state = LongCaptureState::Paused;
        runtime.manifest.message = "长截图已暂停".to_string();
        persist_runtime(&job, &runtime)?;
        status_from_runtime(&runtime)
    };
    show_paused_capture_surface(&app, &job, status.engine)?;
    let _ = app.emit("long_capture_paused", &status);
    Ok(status)
}

#[tauri::command]
pub async fn resume_long_capture(app: AppHandle, job_id: String) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || resume_long_capture_inner(&app, &job_id, false))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("继续长截图任务异常结束: {error}"),
            )
        })?
}

#[tauri::command]
pub async fn retry_long_capture_segment(
    app: AppHandle,
    job_id: String,
) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || resume_long_capture_inner(&app, &job_id, true))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("重试长截图片段异常结束: {error}"),
            )
        })?
}

fn resume_long_capture_inner(
    app: &AppHandle,
    job_id: &str,
    explicit_retry: bool,
) -> AppResult<LongCaptureStatus> {
    let store = app.state::<LongScreenshotStore>();
    resume_or_retry(app, &store, &store.job(job_id)?, explicit_retry)
}

fn resume_or_retry(
    app: &AppHandle,
    store: &LongScreenshotStore,
    job: &Arc<LongCaptureJob>,
    explicit_retry: bool,
) -> AppResult<LongCaptureStatus> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(job)?;
    {
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state != LongCaptureState::Paused {
            return Err(AppError::new(
                "invalid_long_capture_state",
                "只有暂停中的长截图任务可以继续",
            ));
        }
    }
    switch_visible_surface(
        || show_control_window(app, job),
        || hide_capture_overlay(app),
    )?;
    focus_capture_target(job)?;
    flush_desktop_compositor();
    let status = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        runtime.pause_requested = false;
        runtime.retry_current = true;
        runtime.generation = runtime.generation.wrapping_add(1);
        runtime.manifest.state = LongCaptureState::Capturing;
        runtime.manifest.message = if explicit_retry {
            "正在重试当前片段".to_string()
        } else if runtime.manifest.engine == LongCaptureEngine::Manual {
            "长截图已继续，请在目标窗口中手动滚动".to_string()
        } else {
            "长截图已继续".to_string()
        };
        persist_runtime(job, &runtime)?;
        status_from_runtime(&runtime)
    };
    job.wake.notify_all();
    let _ = app.emit("long_capture_progress", &status);
    Ok(status)
}

#[tauri::command]
pub async fn undo_long_capture_segment(
    app: AppHandle,
    job_id: String,
) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || undo_long_capture_segment_inner(&app, &job_id))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("回退长截图片段异常结束: {error}"),
            )
        })?
}

fn undo_long_capture_segment_inner(app: &AppHandle, job_id: &str) -> AppResult<LongCaptureStatus> {
    let store = app.state::<LongScreenshotStore>();
    let job = store.job(job_id)?;
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    let removed = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state.is_terminal() {
            return Err(AppError::new(
                "invalid_long_capture_state",
                "已结束的长截图任务不能回退片段",
            ));
        }
        if runtime.manifest.segments.len() <= 1 {
            return Err(AppError::new(
                "no_long_capture_undo",
                "没有可回退的长截图片段",
            ));
        }
        runtime.pause_requested = true;
        runtime.retry_current = true;
        runtime.generation = runtime.generation.wrapping_add(1);
        runtime.manifest.state = LongCaptureState::Paused;
        runtime.manifest.message = "已回退上一段，长截图保持暂停".to_string();
        let removed = runtime
            .manifest
            .segments
            .pop()
            .expect("segment length checked");
        runtime.manifest.height = removed
            .output_y
            .saturating_add(fixed_bottom_height(&runtime.manifest));
        refresh_fixed_bottom_from_latest_frame(&job, &runtime.manifest)?;
        persist_runtime(&job, &runtime)?;
        removed
    };
    remove_segment_files(&job.directory, &removed);
    let status = job.status();
    show_paused_capture_surface(&app, &job, status.engine)?;
    let _ = app.emit("long_capture_paused", &status);
    Ok(status)
}

#[tauri::command]
pub async fn finish_long_capture(app: AppHandle, job_id: String) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || finish_long_capture_inner(&app, &job_id))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("完成长截图任务异常结束: {error}"),
            )
        })?
}

fn finish_long_capture_inner(app: &AppHandle, job_id: &str) -> AppResult<LongCaptureStatus> {
    let store = app.state::<LongScreenshotStore>();
    let job = store.job(job_id)?;
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    let (status, should_emit) = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.segments.is_empty() {
            return Err(AppError::new(
                "long_capture_not_ready",
                "长截图首帧尚未完成",
            ));
        }
        if matches!(
            runtime.manifest.state,
            LongCaptureState::Failed | LongCaptureState::Canceled
        ) {
            return Err(AppError::new(
                "invalid_long_capture_state",
                "当前长截图任务无法完成",
            ));
        }
        if runtime.manifest.state == LongCaptureState::Ready {
            (status_from_runtime(&runtime), false)
        } else {
            runtime.finish_requested = true;
            runtime.pause_requested = false;
            runtime.generation = runtime.generation.wrapping_add(1);
            runtime.manifest.state = LongCaptureState::Ready;
            runtime.manifest.message = "长截图已完成".to_string();
            runtime.ready_emitted = true;
            persist_runtime(&job, &runtime)?;
            (status_from_runtime(&runtime), true)
        }
    };
    job.wake.notify_all();
    if let Err(surface_error) = switch_visible_surface(
        || show_capture_overlay(&app, &job.target.monitor),
        || close_control_window(&app),
    ) {
        return Err(recover_capture_surface(&app, &job, surface_error));
    }
    if should_emit {
        let _ = app.emit("long_capture_ready", &status);
    }
    Ok(status)
}

#[tauri::command]
pub async fn cancel_long_capture(app: AppHandle, job_id: String) -> AppResult<LongCaptureStatus> {
    tauri::async_runtime::spawn_blocking(move || cancel_long_capture_inner(&app, &job_id))
        .await
        .map_err(|error| {
            AppError::new(
                "long_capture_task_error",
                format!("取消长截图任务异常结束: {error}"),
            )
        })?
}

fn cancel_long_capture_inner(app: &AppHandle, job_id: &str) -> AppResult<LongCaptureStatus> {
    let store = app.state::<LongScreenshotStore>();
    let job = store.job(job_id)?;
    cancel_long_capture_job_inner(app, &store, &job)
}

#[tauri::command]
pub async fn cancel_long_capture_session(
    app: AppHandle,
    session_id: String,
) -> AppResult<Option<LongCaptureStatus>> {
    tauri::async_runtime::spawn_blocking(move || {
        cancel_long_capture_session_inner(&app, &session_id)
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_task_error",
            format!("取消启动中的长截图任务异常结束: {error}"),
        )
    })?
}

fn cancel_long_capture_session_inner(
    app: &AppHandle,
    session_id: &str,
) -> AppResult<Option<LongCaptureStatus>> {
    let screenshot_store = app.state::<ScreenshotStore>();
    let _screenshot_start_guard = screenshot_store.lock_start();
    let store = app.state::<LongScreenshotStore>();
    store.request_pending_start_cancel(session_id);
    let Some(job) = store.job_for_session(session_id) else {
        // Idempotent owner recovery: status polling may discover that the job
        // has already been removed while the reusable screenshot WebView is
        // still hidden. Re-present the active owner even without a job.
        if let Some(session) = screenshot_store
            .active_session()
            .filter(|session| session.id == session_id)
        {
            show_capture_overlay(app, &session.monitor)?;
        }
        return Ok(None);
    };
    cancel_long_capture_job_inner(app, &store, &job).map(Some)
}

/// Ends a long-capture job because its owning ordinary screenshot session is
/// being closed. Unlike the user-facing cancel command this must not restore
/// the overlay that is currently closing or has already been destroyed.
pub(crate) fn cancel_for_screenshot_session_end(
    app: &AppHandle,
    session_id: &str,
) -> AppResult<()> {
    let store = app.state::<LongScreenshotStore>();
    store.request_pending_start_cancel(session_id);
    let Some(job) = store.job_for_session(session_id) else {
        return Ok(());
    };
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    let outcome = request_job_cancel(&job, "截图窗口已关闭，长截图已取消");
    let close_error = close_control_window(app).err();
    if outcome.cleanup_now {
        clear_job_cache(&store, &outcome.status.job_id);
    }
    match (outcome.persistence_error, close_error) {
        (Some(persistence_error), Some(close_error)) => Err(append_recovery_error(
            persistence_error,
            "关闭长截图控制窗口失败",
            &close_error,
        )),
        (Some(persistence_error), None) => Err(persistence_error),
        (None, Some(close_error)) => Err(close_error),
        (None, None) => Ok(()),
    }
}

fn cancel_long_capture_job_inner(
    app: &AppHandle,
    store: &LongScreenshotStore,
    job: &Arc<LongCaptureJob>,
) -> AppResult<LongCaptureStatus> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(job)?;
    let outcome = request_job_cancel(job, "长截图已取消");
    let job_id = outcome.status.job_id.clone();
    let surface_result = switch_visible_surface(
        || show_capture_overlay(app, &job.target.monitor),
        || close_control_window(app),
    );
    if outcome.cleanup_now {
        clear_job_cache(store, &job_id);
    }
    let surface_error = surface_result
        .err()
        .map(|error| recover_capture_surface(app, job, error));
    match (outcome.persistence_error, surface_error) {
        (Some(persistence_error), Some(surface_error)) => Err(append_recovery_error(
            persistence_error,
            "恢复普通截图界面失败",
            &surface_error,
        )),
        (Some(persistence_error), None) => Err(persistence_error),
        (None, Some(surface_error)) => Err(surface_error),
        (None, None) => Ok(outcome.status),
    }
}

struct CancelJobOutcome {
    status: LongCaptureStatus,
    cleanup_now: bool,
    persistence_error: Option<AppError>,
}

fn request_job_cancel(job: &LongCaptureJob, message: &str) -> CancelJobOutcome {
    let (status, cleanup_now, persistence_error) = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        let cleanup_now = runtime.worker_done;
        runtime.cancel_requested = true;
        runtime.pause_requested = false;
        runtime.generation = runtime.generation.wrapping_add(1);
        let persistence_error = if transition_to_canceled(&mut runtime.manifest.state) {
            runtime.manifest.message = message.to_string();
            persist_runtime(job, &runtime).err()
        } else {
            None
        };
        (
            status_from_runtime(&runtime),
            cleanup_now,
            persistence_error,
        )
    };
    // Cancellation is a runtime safety action, not a persistence transaction.
    // A deleted cache directory or full disk must never leave a paused worker
    // asleep behind a failed manifest write.
    job.wake.notify_all();
    CancelJobOutcome {
        status,
        cleanup_now,
        persistence_error,
    }
}

#[tauri::command]
pub async fn get_long_capture_tile(
    store: State<'_, LongScreenshotStore>,
    job_id: String,
    y: u32,
    height: Option<u32>,
) -> AppResult<Response> {
    let job = store.job(&job_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (manifest, directory) = {
            let runtime = lock_unpoisoned(&job.runtime);
            (runtime.manifest.clone(), job.directory.clone())
        };
        if y >= manifest.height {
            return Err(AppError::invalid("长截图瓦片起点超出图片范围"));
        }
        let requested = height
            .unwrap_or(DEFAULT_TILE_HEIGHT)
            .clamp(1, MAX_TILE_HEIGHT);
        let pixel_limited = (MAX_TILE_PIXELS / u64::from(manifest.width)).max(1) as u32;
        let tile_height = requested
            .min(pixel_limited)
            .min(manifest.height.saturating_sub(y));
        let frame = compose_tile(&directory, &manifest, y, tile_height)?;
        Ok(Response::new(encode_png(&frame)?))
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_tile_error",
            format!("生成长截图预览瓦片异常结束: {error}"),
        )
    })?
}

fn purge_expired_annotation_exports(store: &LongScreenshotStore) {
    let directories = {
        let mut tickets = lock_unpoisoned(&store.annotation_exports);
        let expired = tickets
            .iter()
            .filter(|(_, ticket)| ticket.issued_at.elapsed() > ANNOTATION_EXPORT_TICKET_TTL)
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        expired
            .into_iter()
            .filter_map(|token| tickets.remove(&token).map(|ticket| ticket.directory))
            .collect::<Vec<_>>()
    };
    for directory in directories {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[tauri::command]
pub async fn prepare_long_capture_annotation_export(
    app: AppHandle,
    job_id: String,
    action: ScreenshotExportAction,
) -> AppResult<PrepareLongCaptureAnnotationExportResult> {
    tauri::async_runtime::spawn_blocking(move || {
        prepare_long_capture_annotation_export_inner(&app, &job_id, action)
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_export_error",
            format!("准备长截图标注导出失败: {error}"),
        )
    })?
}

fn prepare_long_capture_annotation_export_inner(
    app: &AppHandle,
    job_id: &str,
    action: ScreenshotExportAction,
) -> AppResult<PrepareLongCaptureAnnotationExportResult> {
    let store = app.state::<LongScreenshotStore>();
    purge_expired_annotation_exports(&store);
    let job = store.job(job_id)?;
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    let manifest = {
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state != LongCaptureState::Ready {
            return Err(AppError::new(
                "long_capture_not_ready",
                "长截图尚未完成，不能导出标注",
            ));
        }
        runtime.manifest.clone()
    };
    let strip_height = annotation_export_strip_height(manifest.width);
    if action != ScreenshotExportAction::Save {
        validate_copy_pin_dimensions(manifest.width, manifest.height)?;
    }

    let save_path = if action == ScreenshotExportAction::Save {
        let Some(path) = choose_long_capture_save_path(app)? else {
            return Ok(PrepareLongCaptureAnnotationExportResult {
                canceled: true,
                ticket: None,
                strip_height,
            });
        };
        Some(path)
    } else {
        None
    };

    let token = Uuid::new_v4().to_string();
    let directory = store.cache_root.join(".annotation-exports").join(&token);
    std::fs::create_dir_all(&directory)
        .map_err(|error| AppError::io("创建长截图标注导出目录", error))?;
    if let Err(error) = write_cache_owner(&directory) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    lock_unpoisoned(&store.annotation_exports).insert(
        token.clone(),
        LongCaptureAnnotationExportTicket {
            job: Arc::clone(&job),
            job_id: manifest.job_id,
            session_id: manifest.session_id,
            action,
            save_path,
            directory,
            width: manifest.width,
            height: manifest.height,
            strip_height,
            next_y: 0,
            issued_at: Instant::now(),
        },
    );
    Ok(PrepareLongCaptureAnnotationExportResult {
        canceled: false,
        ticket: Some(token),
        strip_height,
    })
}

#[tauri::command]
pub fn upload_long_capture_annotation_strip(
    store: State<'_, LongScreenshotStore>,
    request: Request<'_>,
) -> AppResult<()> {
    let token = request
        .headers()
        .get(ANNOTATION_EXPORT_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::invalid("缺少长截图标注导出凭证"))?
        .to_string();
    let y = request
        .headers()
        .get(ANNOTATION_EXPORT_Y_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| AppError::invalid("长截图标注条带起点无效"))?;
    let png = match request.body() {
        InvokeBody::Raw(bytes)
            if !bytes.is_empty() && bytes.len() <= MAX_ANNOTATION_STRIP_BYTES =>
        {
            bytes.clone()
        }
        InvokeBody::Raw(_) => return Err(AppError::invalid("长截图标注条带大小无效")),
        InvokeBody::Json(_) => {
            return Err(AppError::invalid(
                "长截图标注条带必须使用 Uint8Array 原始二进制上传",
            ))
        }
    };

    purge_expired_annotation_exports(&store);
    let mut tickets = lock_unpoisoned(&store.annotation_exports);
    let ticket = tickets
        .get_mut(&token)
        .ok_or_else(|| AppError::not_found("长截图标注导出票据不存在或已过期"))?;
    if y != ticket.next_y || y >= ticket.height {
        return Err(AppError::invalid("长截图标注条带顺序不连续"));
    }
    let expected_height = ticket.strip_height.min(ticket.height - y);
    let frame = decode_png(&png)?;
    if frame.width != ticket.width || frame.height != expected_height {
        return Err(AppError::invalid("长截图标注条带尺寸与导出计划不一致"));
    }
    if u64::from(frame.width) * u64::from(frame.height) > MAX_TILE_PIXELS {
        return Err(AppError::invalid("长截图标注条带像素数超过安全限制"));
    }
    atomic_write(&ticket.directory.join(format!("{y:08}.png")), &png)?;
    ticket.next_y = ticket
        .next_y
        .checked_add(expected_height)
        .ok_or_else(|| AppError::invalid("长截图标注条带高度溢出"))?;
    Ok(())
}

struct AnnotationExportDirectoryCleanup(PathBuf);

impl Drop for AnnotationExportDirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tauri::command]
pub async fn finish_long_capture_annotation_export(
    app: AppHandle,
    ticket: String,
) -> AppResult<LongCaptureExportResult> {
    tauri::async_runtime::spawn_blocking(move || {
        finish_long_capture_annotation_export_inner(&app, &ticket)
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_export_error",
            format!("完成长截图标注导出失败: {error}"),
        )
    })?
}

fn finish_long_capture_annotation_export_inner(
    app: &AppHandle,
    token: &str,
) -> AppResult<LongCaptureExportResult> {
    let store = app.state::<LongScreenshotStore>();
    let screenshot_store = app.state::<ScreenshotStore>();
    let _screenshot_start_guard = screenshot_store.lock_start();
    let job = {
        let tickets = lock_unpoisoned(&store.annotation_exports);
        Arc::clone(
            &tickets
                .get(token)
                .ok_or_else(|| AppError::not_found("长截图标注导出票据不存在或已过期"))?
                .job,
        )
    };
    {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        store.ensure_current_job(&job)?;
    }
    if !wait_for_worker_done(&job, WORKER_SHUTDOWN_TIMEOUT) {
        return Err(worker_shutdown_timeout_error());
    }
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(&job)?;
    purge_expired_annotation_exports(&store);
    let ticket = lock_unpoisoned(&store.annotation_exports)
        .remove(token)
        .ok_or_else(|| AppError::not_found("长截图标注导出票据不存在或已过期"))?;
    let _cleanup = AnnotationExportDirectoryCleanup(ticket.directory.clone());
    if ticket.next_y != ticket.height {
        return Err(AppError::invalid("长截图标注条带尚未全部上传"));
    }
    let mut result = LongCaptureExportResult {
        action: ticket.action,
        saved_path: None,
        pin_id: None,
        canceled: false,
    };
    match ticket.action {
        ScreenshotExportAction::Save => {
            let path = ticket
                .save_path
                .as_ref()
                .ok_or_else(|| AppError::invalid("长截图标注导出缺少保存路径"))?;
            stream_annotation_export_png(&ticket, path)?;
            result.saved_path = Some(path.to_string_lossy().into_owned());
        }
        ScreenshotExportAction::Copy | ScreenshotExportAction::Pin => {
            let frame = compose_annotation_export_frame(&ticket)?;
            let png = encode_png(&frame)?;
            if ticket.action == ScreenshotExportAction::Copy {
                screenshot::copy_png_bytes(&png)?;
            } else {
                result.pin_id = Some(screenshot::pin_png_bytes(
                    app.clone(),
                    png,
                    ticket.width,
                    ticket.height,
                )?);
            }
        }
    }
    screenshot::finish_capture_after_cleanup_locked(
        app,
        &screenshot_store,
        &ticket.session_id,
        || {
            store.request_pending_start_cancel(&ticket.session_id);
            clear_job_cache(&store, &ticket.job_id);
        },
    );
    Ok(result)
}

#[tauri::command]
pub fn cancel_long_capture_annotation_export(
    store: State<'_, LongScreenshotStore>,
    ticket: String,
) -> AppResult<()> {
    if let Some(ticket) = lock_unpoisoned(&store.annotation_exports).remove(&ticket) {
        let _ = std::fs::remove_dir_all(ticket.directory);
    }
    Ok(())
}

#[tauri::command]
pub async fn export_long_capture(
    app: AppHandle,
    store: State<'_, LongScreenshotStore>,
    job_id: String,
    action: ScreenshotExportAction,
    annotation_payload: Option<Value>,
) -> AppResult<LongCaptureExportResult> {
    let job = store.job(&job_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        export_long_capture_inner(&app, &job, action, annotation_payload.as_ref())
    })
    .await
    .map_err(|error| {
        AppError::new(
            "long_capture_export_error",
            format!("长截图导出任务异常结束: {error}"),
        )
    })?
}

fn export_long_capture_inner(
    app: &AppHandle,
    job: &Arc<LongCaptureJob>,
    action: ScreenshotExportAction,
    annotation_payload: Option<&Value>,
) -> AppResult<LongCaptureExportResult> {
    let screenshot_store = app.state::<ScreenshotStore>();
    let _screenshot_start_guard = screenshot_store.lock_start();
    let store = app.state::<LongScreenshotStore>();
    {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        store.ensure_current_job(job)?;
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state != LongCaptureState::Ready {
            return Err(AppError::new(
                "long_capture_not_ready",
                "长截图尚未完成，不能导出",
            ));
        }
    }
    if !wait_for_worker_done(job, WORKER_SHUTDOWN_TIMEOUT) {
        return Err(worker_shutdown_timeout_error());
    }
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    store.ensure_current_job(job)?;
    let manifest = {
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state != LongCaptureState::Ready {
            return Err(AppError::new(
                "long_capture_not_ready",
                "长截图尚未完成，不能导出",
            ));
        }
        runtime.manifest.clone()
    };
    validate_annotation_payload(annotation_payload, &manifest)?;
    let result = match action {
        ScreenshotExportAction::Save => {
            let Some(path) = choose_long_capture_save_path(app)? else {
                return Ok(LongCaptureExportResult {
                    action,
                    saved_path: None,
                    pin_id: None,
                    canceled: true,
                });
            };
            stream_manifest_png(&job.directory, &manifest, &path)?;
            LongCaptureExportResult {
                action,
                saved_path: Some(path.to_string_lossy().into_owned()),
                pin_id: None,
                canceled: false,
            }
        }
        ScreenshotExportAction::Copy | ScreenshotExportAction::Pin => {
            validate_copy_pin_dimensions(manifest.width, manifest.height)?;
            let frame = compose_region(
                &job.directory,
                &manifest,
                0,
                manifest.height,
                MAX_COPY_PIN_PIXELS,
            )?;
            let png = encode_png(&frame)?;
            if action == ScreenshotExportAction::Copy {
                screenshot::copy_png_bytes(&png)?;
                LongCaptureExportResult {
                    action,
                    saved_path: None,
                    pin_id: None,
                    canceled: false,
                }
            } else {
                let pin_id =
                    screenshot::pin_png_bytes(app.clone(), png, manifest.width, manifest.height)?;
                LongCaptureExportResult {
                    action,
                    saved_path: None,
                    pin_id: Some(pin_id),
                    canceled: false,
                }
            }
        }
    };
    screenshot::finish_capture_after_cleanup_locked(
        app,
        &screenshot_store,
        &manifest.session_id,
        || {
            store.request_pending_start_cancel(&manifest.session_id);
            clear_job_cache(&store, &manifest.job_id);
        },
    );
    Ok(result)
}

fn validate_copy_pin_dimensions(width: u32, height: u32) -> AppResult<()> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .unwrap_or(u64::MAX);
    if pixels == 0 || pixels > MAX_COPY_PIN_PIXELS || width > 32_767 || height > 32_767 {
        return Err(AppError::new(
            "long_capture_clipboard_limit",
            "长图超过复制或置顶限制，请保存原图",
        ));
    }
    Ok(())
}

fn validate_annotation_payload(
    payload: Option<&Value>,
    manifest: &LongCaptureManifest,
) -> AppResult<()> {
    let Some(payload) = payload else {
        return Ok(());
    };
    let width = payload.get("width").and_then(Value::as_u64);
    let height = payload.get("height").and_then(Value::as_u64);
    let coordinate_space = payload.get("coordinateSpace").and_then(Value::as_str);
    if width != Some(u64::from(manifest.width))
        || height != Some(u64::from(manifest.height))
        || coordinate_space != Some("longImagePixels")
    {
        return Err(AppError::invalid("长截图标注坐标与当前成品尺寸不一致"));
    }
    if payload
        .get("annotations")
        .and_then(Value::as_array)
        .is_some_and(|annotations| !annotations.is_empty())
    {
        return Err(AppError::new(
            "long_capture_annotation_export_unsupported",
            "当前后端尚不能无损渲染长截图标注，请清除标注后导出",
        ));
    }
    Ok(())
}

fn choose_long_capture_save_path(app: &AppHandle) -> AppResult<Option<PathBuf>> {
    let file_name = format!("PetalDesk长截图-{}.png", Utc::now().format("%Y%m%d-%H%M%S"));
    let screenshot_store = app.state::<ScreenshotStore>();
    let settings = screenshot_store.settings();
    let mut dialog = app
        .dialog()
        .file()
        .add_filter("PNG 图片", &["png"])
        .set_title("保存长截图 - 飞花 - PetalDesk")
        .set_file_name(file_name);
    if let Some(directory) = settings
        .last_save_directory
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_dir())
    {
        dialog = dialog.set_directory(directory);
    }
    let Some(path) = dialog.blocking_save_file() else {
        return Ok(None);
    };
    let mut path = path
        .into_path()
        .map_err(|error| AppError::invalid(format!("长截图保存路径无效: {error}")))?;
    if path.extension().is_none() {
        path.set_extension("png");
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err(AppError::invalid("长截图只能保存为 PNG 文件"));
    }
    screenshot_store.update_last_save_directory(&path)?;
    Ok(Some(path))
}

fn annotation_strip_path(ticket: &LongCaptureAnnotationExportTicket, y: u32) -> PathBuf {
    ticket.directory.join(format!("{y:08}.png"))
}

fn annotation_export_strip_height(width: u32) -> u32 {
    if width == 0 {
        return 1;
    }
    let pixel_limited = (MAX_TILE_PIXELS / u64::from(width)).max(1) as u32;
    // The frontend requests up to 128 px of source halo on both sides of a
    // core strip so blur and mosaic output remains continuous at boundaries.
    ANNOTATION_EXPORT_STRIP_HEIGHT.min(pixel_limited.saturating_sub(256).max(1))
}

fn load_annotation_export_strip(
    ticket: &LongCaptureAnnotationExportTicket,
    y: u32,
) -> AppResult<Frame> {
    if y >= ticket.height {
        return Err(AppError::invalid("长截图标注条带起点超出图片范围"));
    }
    let expected_height = ticket.strip_height.min(ticket.height - y);
    let bytes = std::fs::read(annotation_strip_path(ticket, y))
        .map_err(|error| AppError::io("读取长截图标注条带", error))?;
    let frame = decode_png(&bytes)?;
    if frame.width != ticket.width || frame.height != expected_height {
        return Err(AppError::new(
            "invalid_long_capture_cache",
            "长截图标注条带尺寸与导出计划不一致",
        ));
    }
    Ok(frame)
}

fn compose_annotation_export_frame(ticket: &LongCaptureAnnotationExportTicket) -> AppResult<Frame> {
    let expected = checked_rgba_len(ticket.width, ticket.height, MAX_COPY_PIN_PIXELS)?;
    let mut rgba = Vec::with_capacity(expected);
    let mut y = 0_u32;
    while y < ticket.height {
        let strip = load_annotation_export_strip(ticket, y)?;
        rgba.extend_from_slice(&strip.rgba);
        y = y
            .checked_add(strip.height)
            .ok_or_else(|| AppError::invalid("长截图标注条带高度溢出"))?;
    }
    if rgba.len() != expected {
        return Err(AppError::new(
            "invalid_long_capture_cache",
            "长截图标注导出像素长度不完整",
        ));
    }
    Ok(Frame {
        width: ticket.width,
        height: ticket.height,
        rgba,
    })
}

fn stream_annotation_export_png(
    ticket: &LongCaptureAnnotationExportTicket,
    destination: &Path,
) -> AppResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::invalid("长截图保存路径没有父目录"))?;
    std::fs::create_dir_all(parent).map_err(|error| AppError::io("创建长截图保存目录", error))?;
    let temporary = parent.join(format!(".petaldesk-long-{}.tmp", Uuid::new_v4()));
    let result = (|| -> AppResult<()> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| AppError::io("创建长截图临时文件", error))?;
        let sync_file = file
            .try_clone()
            .map_err(|error| AppError::io("准备长截图文件刷新", error))?;
        let mut encoder = png::Encoder::new(file, ticket.width, ticket.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|error| AppError::new("png_error", format!("创建长截图 PNG 失败: {error}")))?;
        {
            let mut stream = writer.stream_writer_with_size(64 * 1024).map_err(|error| {
                AppError::new("png_error", format!("创建长截图流式编码器失败: {error}"))
            })?;
            let mut y = 0_u32;
            while y < ticket.height {
                let strip = load_annotation_export_strip(ticket, y)?;
                stream
                    .write_all(&strip.rgba)
                    .map_err(|error| AppError::io("写入长截图标注 PNG", error))?;
                y = y
                    .checked_add(strip.height)
                    .ok_or_else(|| AppError::invalid("长截图标注条带高度溢出"))?;
            }
            stream.finish().map_err(|error| {
                AppError::new("png_error", format!("完成长截图标注流失败: {error}"))
            })?;
        }
        writer.finish().map_err(|error| {
            AppError::new("png_error", format!("完成长截图标注 PNG 失败: {error}"))
        })?;
        sync_file
            .sync_all()
            .map_err(|error| AppError::io("刷新长截图标注文件", error))?;
        atomic_replace_long_capture_file(&temporary, destination)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn atomic_replace_long_capture_file(source: &Path, destination: &Path) -> AppResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(AppError::io(
            "原子替换长截图文件",
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace_long_capture_file(source: &Path, destination: &Path) -> AppResult<()> {
    std::fs::rename(source, destination).map_err(|error| AppError::io("原子替换长截图文件", error))
}

fn stream_manifest_png(
    directory: &Path,
    manifest: &LongCaptureManifest,
    path: &Path,
) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid("长截图保存路径没有父目录"))?;
    std::fs::create_dir_all(parent).map_err(|error| AppError::io("创建长截图保存目录", error))?;
    let temporary = parent.join(format!(".petaldesk-long-{}.tmp", Uuid::new_v4()));
    let result = stream_manifest_png_file(directory, manifest, &temporary).and_then(|()| {
        atomic_replace_long_capture_file(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn stream_manifest_png_file(
    directory: &Path,
    manifest: &LongCaptureManifest,
    path: &Path,
) -> AppResult<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| AppError::io("创建长截图临时文件", error))?;
    let sync_file = file
        .try_clone()
        .map_err(|error| AppError::io("准备长截图文件刷新", error))?;
    let mut encoder = png::Encoder::new(file, manifest.width, manifest.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder
        .write_header()
        .map_err(|error| AppError::new("png_error", format!("创建长截图 PNG 失败: {error}")))?;
    {
        let mut stream = writer.stream_writer_with_size(64 * 1024).map_err(|error| {
            AppError::new("png_error", format!("创建长截图流式编码器失败: {error}"))
        })?;
        let mut written_rows = 0_u32;
        for segment in &manifest.segments {
            let strip = decode_png(
                &std::fs::read(directory.join(&segment.strip_file))
                    .map_err(|error| AppError::io("读取长截图导出条带", error))?,
            )?;
            if strip.width != manifest.width || strip.height != segment.height {
                return Err(AppError::new(
                    "invalid_long_capture_cache",
                    "长截图导出条带尺寸与清单不一致",
                ));
            }
            stream
                .write_all(&strip.rgba)
                .map_err(|error| AppError::io("写入长截图 PNG", error))?;
            written_rows = written_rows.saturating_add(segment.height);
        }
        if let Some(fixed_bottom) = manifest.fixed_bottom.as_ref() {
            let footer = decode_png(
                &std::fs::read(directory.join(&fixed_bottom.file))
                    .map_err(|error| AppError::io("读取长截图固定底栏", error))?,
            )?;
            if footer.width != manifest.width || footer.height != fixed_bottom.height {
                return Err(AppError::new(
                    "invalid_long_capture_cache",
                    "长截图固定底栏尺寸与清单不一致",
                ));
            }
            stream
                .write_all(&footer.rgba)
                .map_err(|error| AppError::io("写入长截图固定底栏", error))?;
            written_rows = written_rows.saturating_add(fixed_bottom.height);
        }
        if written_rows != manifest.height {
            return Err(AppError::new(
                "invalid_long_capture_cache",
                "长截图导出条带总高度与清单不一致",
            ));
        }
        stream
            .finish()
            .map_err(|error| AppError::new("png_error", format!("完成长截图流失败: {error}")))?;
    }
    writer
        .finish()
        .map_err(|error| AppError::new("png_error", format!("完成长截图 PNG 失败: {error}")))?;
    sync_file
        .sync_all()
        .map_err(|error| AppError::io("刷新长截图文件", error))
}

fn validate_capture_target(
    monitor: &MonitorBounds,
    selection: PhysicalRect,
    scroll_anchor: PhysicalPoint,
    mode: LongCaptureMode,
) -> AppResult<CaptureTarget> {
    if selection.x < 0 || selection.y < 0 || selection.width == 0 || selection.height == 0 {
        return Err(AppError::invalid(
            "长截图选区必须是显示器内的非空物理像素区域",
        ));
    }
    let right = i64::from(selection.x) + i64::from(selection.width);
    let bottom = i64::from(selection.y) + i64::from(selection.height);
    if right > i64::from(monitor.width) || bottom > i64::from(monitor.height) {
        return Err(AppError::invalid("长截图选区超出当前截图显示器"));
    }
    let frame_pixels = u64::from(selection.width) * u64::from(selection.height);
    if frame_pixels > MAX_FRAME_PIXELS || frame_pixels > MAX_LONG_PIXELS {
        return Err(AppError::invalid("长截图选区像素数超过安全限制"));
    }
    let anchor_inside = scroll_anchor.x >= selection.x
        && scroll_anchor.y >= selection.y
        && i64::from(scroll_anchor.x) < right
        && i64::from(scroll_anchor.y) < bottom;
    if !anchor_inside {
        return Err(AppError::invalid("滚动锚点必须位于长截图选区内"));
    }
    let desktop_x = monitor
        .x
        .checked_add(selection.x)
        .ok_or_else(|| AppError::invalid("长截图选区横坐标溢出"))?;
    let desktop_y = monitor
        .y
        .checked_add(selection.y)
        .ok_or_else(|| AppError::invalid("长截图选区纵坐标溢出"))?;
    let anchor_x = monitor
        .x
        .checked_add(scroll_anchor.x)
        .ok_or_else(|| AppError::invalid("滚动锚点横坐标溢出"))?;
    let anchor_y = monitor
        .y
        .checked_add(scroll_anchor.y)
        .ok_or_else(|| AppError::invalid("滚动锚点纵坐标溢出"))?;
    let desktop_bounds = PhysicalRect {
        x: desktop_x,
        y: desktop_y,
        width: selection.width,
        height: selection.height,
    };
    let (_, _, control_overlaps_roi) = control_window_geometry(monitor, desktop_bounds);
    Ok(CaptureTarget {
        bounds: desktop_bounds,
        scroll_anchor: PhysicalPoint {
            x: anchor_x,
            y: anchor_y,
        },
        monitor: monitor.clone(),
        mode,
        control_overlaps_roi,
        scroll_windows: None,
    })
}

#[derive(Debug, Clone, Copy)]
struct DetectedBrowserTarget {
    family: BrowserFamily,
    anchor_client_physical: PhysicalPoint,
}

fn select_browser_capture_context(
    bridge: Option<&Arc<BrowserBridge>>,
    anchor: PhysicalPoint,
) -> Option<BrowserCaptureContext> {
    let bridge = bridge?.clone();
    let detected = detect_browser_target(anchor)?;
    let status = bridge.status().ok()?;
    let connection = browser_connection(&status, detected.family);
    let required = ["prepare", "start", "step", "status", "restore"];
    let supports_protocol = connection.capabilities.is_empty()
        || required.iter().all(|required| {
            connection
                .capabilities
                .iter()
                .any(|value| value == required)
        });
    if !connection.connected || !connection.ready || !supports_protocol {
        return None;
    }
    let connection_id = connection.connection_id.clone()?;
    if !bridge
        .connection_is_unique(detected.family, &connection_id)
        .ok()?
    {
        return None;
    }
    Some(BrowserCaptureContext {
        bridge,
        family: detected.family,
        connection_id,
        anchor_client_physical: detected.anchor_client_physical,
    })
}

fn browser_connection(
    status: &BrowserBridgeStatus,
    family: BrowserFamily,
) -> &BrowserConnectionStatus {
    match family {
        BrowserFamily::Chrome => &status.chrome,
        BrowserFamily::Edge => &status.edge,
        BrowserFamily::Firefox => &status.firefox,
    }
}

fn classify_browser_executable(path: &str) -> Option<BrowserFamily> {
    let file_name = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    match file_name.as_str() {
        "chrome.exe" | "chrome" => Some(BrowserFamily::Chrome),
        "msedge.exe" | "msedge" => Some(BrowserFamily::Edge),
        "firefox.exe" | "firefox" => Some(BrowserFamily::Firefox),
        _ => None,
    }
}

#[cfg(windows)]
fn detect_browser_target(anchor: PhysicalPoint) -> Option<DetectedBrowserTarget> {
    use windows_sys::Win32::Foundation::{CloseHandle, POINT};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
    };

    struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    let screen_point = POINT {
        x: anchor.x,
        y: anchor.y,
    };
    let hit_window = unsafe { WindowFromPoint(screen_point) };
    if hit_window.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hit_window, GA_ROOT) };
    let process_window = if root.is_null() { hit_window } else { root };
    let mut process_id = 0_u32;
    if unsafe { GetWindowThreadProcessId(process_window, &mut process_id) } == 0 || process_id == 0
    {
        return None;
    }
    let process =
        ProcessHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) });
    if process.0.is_null() {
        return None;
    }
    let mut image_path = vec![0_u16; 32_768];
    let mut image_path_len = image_path.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(process.0, 0, image_path.as_mut_ptr(), &mut image_path_len)
    } == 0
    {
        return None;
    }
    let family = classify_browser_executable(&String::from_utf16_lossy(
        &image_path[..image_path_len as usize],
    ))?;

    let mut client_point = screen_point;
    if unsafe { ScreenToClient(hit_window, &mut client_point) } == 0
        || client_point.x < 0
        || client_point.y < 0
    {
        return None;
    }
    let anchor_client_physical = PhysicalPoint {
        x: client_point.x,
        y: client_point.y,
    };
    Some(DetectedBrowserTarget {
        family,
        anchor_client_physical,
    })
}

#[cfg(not(windows))]
fn detect_browser_target(_anchor: PhysicalPoint) -> Option<DetectedBrowserTarget> {
    None
}

fn persist_runtime(job: &LongCaptureJob, runtime: &LongCaptureRuntime) -> AppResult<()> {
    atomic_write_json(&job.directory.join("manifest.json"), &runtime.manifest)
}

fn hide_capture_overlay(app: &AppHandle) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(CAPTURE_WINDOW_LABEL) {
        window
            .hide()
            .map_err(|error| AppError::new("window_error", format!("隐藏截图窗口失败: {error}")))?;
    }
    Ok(())
}

fn switch_visible_surface<ShowTarget, DismissSource>(
    show_target: ShowTarget,
    dismiss_source: DismissSource,
) -> AppResult<()>
where
    ShowTarget: FnOnce() -> AppResult<()>,
    DismissSource: FnOnce() -> AppResult<()>,
{
    show_target()?;
    dismiss_source()
}

fn append_recovery_error(primary: AppError, action: &str, recovery: &AppError) -> AppError {
    AppError::new(
        primary.code,
        format!("{}；{action}: {}", primary.message, recovery.message),
    )
}

fn show_main_recovery_window(app: &AppHandle) -> AppResult<()> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| AppError::new("window_error", "飞花主窗口不可用"))?;
    window
        .show()
        .and_then(|_| window.unminimize())
        .and_then(|_| window.set_focus())
        .map_err(|error| AppError::new("window_error", format!("显示飞花主窗口失败: {error}")))
}

#[cfg(windows)]
fn show_capture_recovery_dialog(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };

    let message = format!("长截图界面恢复失败。\r\n\r\n{message}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let title = "飞花 - PetalDesk"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(not(windows))]
fn show_capture_recovery_dialog(message: &str) {
    eprintln!("长截图界面恢复失败: {message}");
}

fn hide_visible_pin_windows(app: &AppHandle) -> Vec<String> {
    let mut hidden = Vec::new();
    for (label, window) in app.webview_windows() {
        if label.starts_with("screenshot-pin-")
            && window.is_visible().unwrap_or(false)
            && window.hide().is_ok()
        {
            hidden.push(label);
        }
    }
    hidden
}

fn restore_hidden_pin_windows(app: &AppHandle, labels: &[String]) {
    for label in labels {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.show();
        }
    }
}

fn show_capture_overlay(app: &AppHandle, monitor: &MonitorBounds) -> AppResult<()> {
    screenshot::prepare_capture_window(app, monitor)?;
    screenshot::present_capture_window(app)
}

fn show_paused_capture_surface(
    app: &AppHandle,
    job: &LongCaptureJob,
    engine: LongCaptureEngine,
) -> AppResult<()> {
    if engine == LongCaptureEngine::Manual {
        // Manual capture depends on the user interacting with the real target.
        // Keep that target visible and leave the independent control available
        // so pause never turns into an inert frozen screenshot overlay.
        switch_visible_surface(
            || show_control_window(app, job),
            || hide_capture_overlay(app),
        )
    } else {
        switch_visible_surface(
            || show_capture_overlay(app, &job.target.monitor),
            || hide_control_window(app),
        )
    }
}

fn control_window_geometry(
    monitor: &MonitorBounds,
    selection: PhysicalRect,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>, bool) {
    let scale = if monitor.scale_factor.is_finite() && monitor.scale_factor > 0.0 {
        monitor.scale_factor
    } else {
        1.0
    };
    let desired_width = (f64::from(CONTROL_WINDOW_MAX_WIDTH) * scale).round() as u32;
    let desired_height = (f64::from(CONTROL_WINDOW_HEIGHT) * scale).round() as u32;
    let width = desired_width.min(monitor.width.saturating_sub(16).max(240));
    let height = desired_height.min(monitor.height.saturating_sub(8).max(48));
    let monitor_right = i64::from(monitor.x) + i64::from(monitor.width);
    let monitor_bottom = i64::from(monitor.y) + i64::from(monitor.height);
    let selection_right = i64::from(selection.x) + i64::from(selection.width);
    let selection_bottom = i64::from(selection.y) + i64::from(selection.height);
    let centered_x = i64::from(selection.x) + i64::from(selection.width) / 2 - i64::from(width) / 2;
    let x = centered_x.clamp(
        i64::from(monitor.x) + 8,
        monitor_right - i64::from(width) - 8,
    ) as i32;
    let gap = 8_i64;
    let (y, overlaps) = if i64::from(selection.y) - i64::from(monitor.y) >= i64::from(height) + gap
    {
        (i64::from(selection.y) - i64::from(height) - gap, false)
    } else if monitor_bottom - selection_bottom >= i64::from(height) + gap {
        (selection_bottom + gap, false)
    } else if i64::from(selection.x) - i64::from(monitor.x) >= i64::from(width) + gap {
        let side_x = i64::from(selection.x) - i64::from(width) - gap;
        let y = i64::from(selection.y).clamp(
            i64::from(monitor.y) + 4,
            monitor_bottom - i64::from(height) - 4,
        );
        return (
            PhysicalPosition::new(side_x as i32, y as i32),
            PhysicalSize::new(width, height),
            false,
        );
    } else if monitor_right - selection_right >= i64::from(width) + gap {
        let side_x = selection_right + gap;
        let y = i64::from(selection.y).clamp(
            i64::from(monitor.y) + 4,
            monitor_bottom - i64::from(height) - 4,
        );
        return (
            PhysicalPosition::new(side_x as i32, y as i32),
            PhysicalSize::new(width, height),
            false,
        );
    } else {
        (i64::from(monitor.y) + 8, true)
    };
    (
        PhysicalPosition::new(x, y as i32),
        PhysicalSize::new(width, height),
        overlaps,
    )
}

fn outline_window_geometry(
    monitor: &MonitorBounds,
    selection: PhysicalRect,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let scale = if monitor.scale_factor.is_finite() && monitor.scale_factor > 0.0 {
        monitor.scale_factor
    } else {
        1.0
    };
    // CSS paints at most five logical pixels inward from the outline window.
    // Keep a sixth pixel outside the ROI so CAPTUREBLT never sees the border,
    // including at high DPI and on monitors with negative coordinates.
    let margin = ((OUTLINE_MARGIN_LOGICAL * scale).ceil() as u32).max(6);
    let margin_i32 = i32::try_from(margin).unwrap_or(i32::MAX);
    (
        PhysicalPosition::new(
            selection.x.saturating_sub(margin_i32),
            selection.y.saturating_sub(margin_i32),
        ),
        PhysicalSize::new(
            selection.width.saturating_add(margin.saturating_mul(2)),
            selection.height.saturating_add(margin.saturating_mul(2)),
        ),
    )
}

fn control_overlap_in_roi(target: &CaptureTarget) -> Option<PhysicalRect> {
    let (position, size, overlaps) = control_window_geometry(&target.monitor, target.bounds);
    if !overlaps {
        return None;
    }
    let control_left = i64::from(position.x);
    let control_top = i64::from(position.y);
    let control_right = control_left + i64::from(size.width);
    let control_bottom = control_top + i64::from(size.height);
    let roi_left = i64::from(target.bounds.x);
    let roi_top = i64::from(target.bounds.y);
    let roi_right = roi_left + i64::from(target.bounds.width);
    let roi_bottom = roi_top + i64::from(target.bounds.height);
    let left = control_left.max(roi_left);
    let top = control_top.max(roi_top);
    let right = control_right.min(roi_right);
    let bottom = control_bottom.min(roi_bottom);
    if right <= left || bottom <= top {
        return None;
    }
    Some(PhysicalRect {
        x: (left - roi_left) as i32,
        y: (top - roi_top) as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

#[cfg(windows)]
fn control_window_ex_style(current: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE;
    current | WS_EX_NOACTIVATE as isize
}

#[cfg(windows)]
fn configure_control_window_no_activate(
    window: &tauri::WebviewWindow<tauri::Wry>,
) -> AppResult<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };

    let hwnd = window.hwnd().map_err(|error| {
        AppError::new(
            "window_error",
            format!("读取长截图控制窗口句柄失败: {error}"),
        )
    })?;
    let hwnd = hwnd.0 as *mut std::ffi::c_void;
    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let desired = control_window_ex_style(current);
    if desired != current {
        unsafe {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired);
        }
    }
    let confirmed = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if confirmed & WS_EX_NOACTIVATE as isize == 0 {
        return Err(last_windows_error("设置长截图控制窗口为不抢焦点模式"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn configure_control_window_no_activate(
    _window: &tauri::WebviewWindow<tauri::Wry>,
) -> AppResult<()> {
    Ok(())
}

#[cfg(windows)]
fn show_control_window_no_activate(window: &tauri::WebviewWindow<tauri::Wry>) -> AppResult<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_SHOWWINDOW,
    };

    let hwnd = window.hwnd().map_err(|error| {
        AppError::new(
            "window_error",
            format!("读取长截图控制窗口句柄失败: {error}"),
        )
    })?;
    let hwnd = hwnd.0 as *mut std::ffi::c_void;
    if unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        )
    } == 0
    {
        return Err(last_windows_error("无焦点显示长截图控制窗口"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn show_control_window_no_activate(window: &tauri::WebviewWindow<tauri::Wry>) -> AppResult<()> {
    window
        .show()
        .map_err(|error| AppError::new("window_error", format!("显示长截图控制窗口失败: {error}")))
}

fn position_control_window(
    window: &tauri::WebviewWindow<tauri::Wry>,
    monitor: &MonitorBounds,
    selection: PhysicalRect,
) -> AppResult<()> {
    let (position, size, _) = control_window_geometry(monitor, selection);
    configure_control_window_no_activate(&window)?;
    window
        .set_position(position)
        .and_then(|_| window.set_size(size))
        .map_err(|error| AppError::new("window_error", format!("定位长截图控制窗口失败: {error}")))
}

fn prepare_control_window(
    app: &AppHandle,
    monitor: &MonitorBounds,
    selection: PhysicalRect,
    job_id: &str,
) -> AppResult<Option<isize>> {
    let (_, size, _) = control_window_geometry(monitor, selection);
    let _creation_guard = lock_unpoisoned(&CONTROL_WINDOW_CREATION_LOCK);
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        destroy_control_window_instance_and_wait(app, &window, "重建长截图控制窗口前关闭旧窗口")?;
    }
    let window = WebviewWindowBuilder::new(
        app,
        CONTROL_WINDOW_LABEL,
        WebviewUrl::App(format!("?tool=screenshot&longControl={job_id}").into()),
    )
    .title("长截图控制 - 飞花 - PetalDesk")
    .decorations(false)
    .shadow(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(false)
    .inner_size(f64::from(size.width), f64::from(size.height))
    .build()
    .map_err(|error| AppError::new("window_error", format!("创建长截图控制窗口失败: {error}")))?;
    position_control_window(&window, monitor, selection)?;
    let instance_id = control_window_instance_id(&window);
    #[cfg(windows)]
    if instance_id.is_none() {
        return Err(AppError::new(
            "long_capture_control_missing",
            "无法识别长截图控制窗口实例，请重新截图",
        ));
    }
    Ok(instance_id)
}

fn prepare_outline_window(
    app: &AppHandle,
    monitor: &MonitorBounds,
    selection: PhysicalRect,
) -> AppResult<()> {
    let _creation_guard = lock_unpoisoned(&CONTROL_WINDOW_CREATION_LOCK);
    let window = if let Some(window) = app.get_webview_window(OUTLINE_WINDOW_LABEL) {
        window
    } else {
        WebviewWindowBuilder::new(
            app,
            OUTLINE_WINDOW_LABEL,
            WebviewUrl::App("?tool=screenshot&longOutline=1".into()),
        )
        .title("长截图范围 - 飞花 - PetalDesk")
        .decorations(false)
        .shadow(false)
        .transparent(true)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .inner_size(320.0, 200.0)
        .build()
        .map_err(|error| {
            AppError::new("window_error", format!("创建长截图选区边框失败: {error}"))
        })?
    };
    let (position, size) = outline_window_geometry(monitor, selection);
    configure_control_window_no_activate(&window)?;
    window
        .set_ignore_cursor_events(true)
        .and_then(|_| window.set_position(position))
        .and_then(|_| window.set_size(size))
        .map_err(|error| AppError::new("window_error", format!("定位长截图选区边框失败: {error}")))
}

fn show_outline_window(app: &AppHandle, job: &LongCaptureJob) -> AppResult<()> {
    prepare_outline_window(app, &job.target.monitor, job.target.bounds)?;
    let window = app
        .get_webview_window(OUTLINE_WINDOW_LABEL)
        .ok_or_else(|| AppError::new("window_error", "长截图选区边框不可用"))?;
    show_control_window_no_activate(&window)
}

fn hide_outline_window(app: &AppHandle) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(OUTLINE_WINDOW_LABEL) {
        window.hide().map_err(|error| {
            AppError::new("window_error", format!("隐藏长截图选区边框失败: {error}"))
        })?;
    }
    Ok(())
}

fn show_control_window(app: &AppHandle, job: &LongCaptureJob) -> AppResult<()> {
    if let Err(error) = show_outline_window(app, job) {
        // A reused outline may already be visible when repositioning or
        // showing it fails. Roll it back just like a panel failure so an
        // orphaned topmost border cannot remain on the desktop.
        let _ = hide_outline_window(app);
        return Err(error);
    }
    if let Err(error) = show_control_panel(app, job) {
        let _ = hide_outline_window(app);
        return Err(error);
    }
    Ok(())
}

fn show_control_panel(app: &AppHandle, job: &LongCaptureJob) -> AppResult<()> {
    let window = app
        .get_webview_window(CONTROL_WINDOW_LABEL)
        .ok_or_else(|| {
            AppError::new(
                "long_capture_control_missing",
                "长截图控制窗口已关闭，正在恢复普通截图界面",
            )
        })?;
    position_control_window(&window, &job.target.monitor, job.target.bounds)?;
    if let Err(error) = show_control_window_no_activate(&window) {
        let _ = window.hide();
        return Err(error);
    }
    Ok(())
}

fn hide_control_panel(app: &AppHandle) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        window.hide().map_err(|error| {
            AppError::new("window_error", format!("隐藏长截图控制窗口失败: {error}"))
        })?;
    }
    Ok(())
}

fn hide_control_window(app: &AppHandle) -> AppResult<()> {
    let outline_error = hide_outline_window(app).err();
    hide_control_panel(app)?;
    outline_error.map_or(Ok(()), Err)
}

fn close_control_window(app: &AppHandle) -> AppResult<()> {
    let outline_error = hide_outline_window(app).err();
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        destroy_control_window_instance(app, &window, "关闭长截图控制窗口")?;
    }
    outline_error.map_or(Ok(()), Err)
}

fn destroy_control_window_instance(
    app: &AppHandle,
    window: &tauri::WebviewWindow<tauri::Wry>,
    action: &str,
) -> AppResult<()> {
    let store = app.try_state::<LongScreenshotStore>();
    let instance_id = control_window_instance_id(window);
    let registered = if let (Some(store), Some(instance_id)) = (store.as_ref(), instance_id) {
        store.expect_control_destroy(instance_id, false)
    } else {
        true
    };
    if !registered {
        return Ok(());
    }
    if let Err(error) = window.destroy() {
        if let (Some(store), Some(instance_id)) = (store.as_ref(), instance_id) {
            store.revoke_expected_control_destroy(instance_id);
        }
        return Err(AppError::new(
            "window_error",
            format!("{action}失败: {error}"),
        ));
    }
    Ok(())
}

fn destroy_control_window_instance_and_wait(
    app: &AppHandle,
    window: &tauri::WebviewWindow<tauri::Wry>,
    action: &str,
) -> AppResult<()> {
    let store = app.try_state::<LongScreenshotStore>().ok_or_else(|| {
        AppError::new(
            "long_capture_control_destroy_error",
            format!("{action}失败: 长截图状态尚未初始化"),
        )
    })?;
    let instance_id = control_window_instance_id(window).ok_or_else(|| {
        AppError::new(
            "long_capture_control_destroy_error",
            format!("{action}失败: 无法识别原控制窗口"),
        )
    })?;
    let deadline = Instant::now() + CONTROL_WINDOW_DESTROY_TIMEOUT;
    if store.expect_control_destroy(instance_id, true) {
        if let Err(error) = window.destroy() {
            store.revoke_expected_control_destroy(instance_id);
            return Err(AppError::new(
                "window_error",
                format!("{action}失败: {error}"),
            ));
        }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() || !store.wait_for_control_destroy(instance_id, remaining) {
        return Err(AppError::new(
            "long_capture_control_destroy_timeout",
            format!("{action}超时，请重试截图"),
        ));
    }
    while app.get_webview_window(CONTROL_WINDOW_LABEL).is_some() {
        if Instant::now() >= deadline {
            return Err(AppError::new(
                "long_capture_control_destroy_timeout",
                format!("{action}超时，请重试截图"),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(windows)]
fn control_window_instance_id(window: &tauri::WebviewWindow<tauri::Wry>) -> Option<isize> {
    window.hwnd().ok().map(|handle| handle.0 as isize)
}

#[cfg(not(windows))]
fn control_window_instance_id(_window: &tauri::WebviewWindow<tauri::Wry>) -> Option<isize> {
    None
}

pub(crate) fn handle_control_window_close_requested(
    app: &AppHandle,
    label: &str,
    instance_id: Option<isize>,
) -> bool {
    if label != CONTROL_WINDOW_LABEL {
        return false;
    }
    let Some(store) = app.try_state::<LongScreenshotStore>() else {
        return false;
    };
    let job = lock_unpoisoned(&store.job).as_ref().cloned();
    let Some(job) = job else {
        return false;
    };
    let current_instance = app
        .get_webview_window(CONTROL_WINDOW_LABEL)
        .as_ref()
        .and_then(control_window_instance_id);
    if !destroyed_control_window_matches_job(
        instance_id,
        job.control_window_instance,
        current_instance,
    ) {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(store) = app.try_state::<LongScreenshotStore>() else {
            return;
        };
        let _start_guard = lock_unpoisoned(&store.start_lock);
        if !lock_unpoisoned(&store.job)
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &job))
        {
            return;
        }
        let _ = cancel_long_capture_job_inner(&app, &store, &job);
    });
    true
}

fn destroyed_control_window_matches_job(
    destroyed_instance: Option<isize>,
    job_instance: Option<isize>,
    current_instance: Option<isize>,
) -> bool {
    match (destroyed_instance, job_instance) {
        (Some(destroyed), Some(owner)) => destroyed == owner,
        (Some(_), None) => false,
        (None, Some(_)) => current_instance.is_none(),
        (None, None) => current_instance.is_none(),
    }
}

pub(crate) fn handle_control_window_destroyed(
    app: &AppHandle,
    label: &str,
    instance_id: Option<isize>,
) -> bool {
    if label != CONTROL_WINDOW_LABEL {
        return false;
    }
    let Some(store) = app.try_state::<LongScreenshotStore>() else {
        return true;
    };
    let current_instance = app
        .get_webview_window(CONTROL_WINDOW_LABEL)
        .as_ref()
        .and_then(control_window_instance_id);
    if store.consume_expected_control_destroy(instance_id, current_instance) {
        return true;
    }
    let job = lock_unpoisoned(&store.job).as_ref().cloned();
    let Some(job) = job else {
        return true;
    };
    if !destroyed_control_window_matches_job(
        instance_id,
        job.control_window_instance,
        current_instance,
    ) {
        return true;
    }
    if !should_cancel_after_control_destroy(job.status().state) {
        // Finishing intentionally destroys the controller. Even if Windows no
        // longer exposes its HWND to the event callback, a completed capture
        // must remain available for preview and export.
        return true;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(store) = app.try_state::<LongScreenshotStore>() else {
            return;
        };
        let _start_guard = lock_unpoisoned(&store.start_lock);
        if !lock_unpoisoned(&store.job)
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &job))
        {
            return;
        }
        let status = job.status();
        let active_session = app.state::<ScreenshotStore>().active_session();
        let session_is_active = screenshot_session_owns_job(
            active_session.as_ref().map(|session| session.id.as_str()),
            &status.session_id,
        );
        let result = if session_is_active {
            cancel_long_capture_job_inner(&app, &store, &job).map(|_| ())
        } else {
            cancel_for_screenshot_session_end(&app, &status.session_id)
        };
        if let Err(error) = result {
            eprintln!("长截图控制窗口异常销毁后的清理失败: {error}");
            let _ = app.emit("screenshot_capture_error", error);
        }
    });
    true
}

fn recover_capture_overlay(
    app: &AppHandle,
    monitor: &MonitorBounds,
    failure: AppError,
) -> AppError {
    match show_capture_overlay(app, monitor) {
        Ok(()) => failure,
        Err(overlay_error) => {
            let mut combined =
                append_recovery_error(failure, "恢复截图编辑界面失败", &overlay_error);
            if let Err(hide_error) = hide_capture_overlay(app) {
                combined = append_recovery_error(combined, "隐藏异常截图界面失败", &hide_error);
            }
            if let Err(main_error) = show_main_recovery_window(app) {
                combined = append_recovery_error(combined, "显示主窗口回退失败", &main_error);
            }
            show_capture_recovery_dialog(&combined.message);
            combined
        }
    }
}

fn recover_capture_surface(app: &AppHandle, job: &LongCaptureJob, failure: AppError) -> AppError {
    match show_capture_overlay(app, &job.target.monitor) {
        Ok(()) => failure,
        Err(overlay_error) => {
            let mut combined =
                append_recovery_error(failure, "恢复截图编辑界面失败", &overlay_error);
            let overlay_hide_error = hide_capture_overlay(app).err();
            match show_control_window(app, job) {
                Ok(()) if overlay_hide_error.is_none() => combined,
                Ok(()) => {
                    if let Some(hide_error) = overlay_hide_error.as_ref() {
                        combined =
                            append_recovery_error(combined, "隐藏异常截图界面失败", hide_error);
                    }
                    if let Err(main_error) = show_main_recovery_window(app) {
                        combined =
                            append_recovery_error(combined, "显示主窗口回退失败", &main_error);
                    }
                    show_capture_recovery_dialog(&combined.message);
                    combined
                }
                Err(control_error) => {
                    combined =
                        append_recovery_error(combined, "恢复长截图控制窗口失败", &control_error);
                    if let Err(main_error) = show_main_recovery_window(app) {
                        combined =
                            append_recovery_error(combined, "显示主窗口回退失败", &main_error);
                    }
                    show_capture_recovery_dialog(&combined.message);
                    combined
                }
            }
        }
    }
}

#[cfg(windows)]
fn flush_desktop_compositor() {
    unsafe {
        let _ = windows_sys::Win32::Graphics::Dwm::DwmFlush();
    }
}

#[cfg(not(windows))]
fn flush_desktop_compositor() {}

fn remove_segment_files(directory: &Path, segment: &LongCaptureSegment) {
    let frame = directory.join(&segment.frame_file);
    let strip = directory.join(&segment.strip_file);
    let _ = std::fs::remove_file(&frame);
    if strip != frame {
        let _ = std::fs::remove_file(strip);
    }
}

struct BrowserRestoreGuard {
    job: Arc<LongCaptureJob>,
}

impl BrowserRestoreGuard {
    fn new(job: Arc<LongCaptureJob>) -> Self {
        Self { job }
    }

    fn restore_now(&self) {
        let _ = restore_browser_session(&self.job);
    }
}

impl Drop for BrowserRestoreGuard {
    fn drop(&mut self) {
        let _ = restore_browser_session(&self.job);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BrowserStepResult {
    moved: bool,
    at_bottom: bool,
    actual_distance: f64,
}

fn browser_prepare_request(
    context: &BrowserCaptureContext,
    command: &str,
    payload: Value,
) -> Result<Value, String> {
    context.bridge.request_connection(
        context.family,
        &context.connection_id,
        command,
        payload,
        BROWSER_REQUEST_TIMEOUT,
    )
}

fn parse_browser_session_binding(response: &Value) -> Result<BrowserSessionBinding, String> {
    let tab_id = response
        .get("tabId")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| "browser extension prepare response has an invalid tabId".to_string())?;
    let frame_id = response
        .get("frameId")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| "browser extension prepare response has an invalid frameId".to_string())?;
    let session_id = response
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 160)
        .ok_or_else(|| "browser extension prepare response has an invalid sessionId".to_string())?
        .to_string();
    let device_pixel_ratio = response
        .get("devicePixelRatio")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && (0.1..=16.0).contains(value))
        .ok_or_else(|| {
            "browser extension prepare response has an invalid devicePixelRatio".to_string()
        })?;
    Ok(BrowserSessionBinding {
        tab_id,
        frame_id,
        session_id,
        device_pixel_ratio,
    })
}

fn browser_payload(binding: &BrowserSessionBinding, payload: Value) -> Result<Value, String> {
    let mut payload = payload
        .as_object()
        .cloned()
        .ok_or_else(|| "browser extension command payload must be an object".to_string())?;
    payload.insert("tabId".to_string(), binding.tab_id.into());
    payload.insert("frameId".to_string(), binding.frame_id.into());
    payload.insert(
        "sessionId".to_string(),
        Value::String(binding.session_id.clone()),
    );
    Ok(Value::Object(payload))
}

fn validate_browser_response_binding(
    response: &Value,
    expected: &BrowserSessionBinding,
) -> Result<(), String> {
    let actual = parse_browser_session_binding(response)?;
    if actual.tab_id != expected.tab_id
        || actual.frame_id != expected.frame_id
        || actual.session_id != expected.session_id
        || (actual.device_pixel_ratio - expected.device_pixel_ratio).abs() > 0.000_001
    {
        return Err("browser extension capture session identity changed".to_string());
    }
    Ok(())
}

fn browser_bound_request(
    context: &BrowserCaptureContext,
    binding: &BrowserSessionBinding,
    command: &str,
    payload: Value,
) -> Result<Value, String> {
    let response = context.bridge.request_connection(
        context.family,
        &context.connection_id,
        command,
        browser_payload(binding, payload)?,
        BROWSER_REQUEST_TIMEOUT,
    )?;
    validate_browser_response_binding(&response, binding)?;
    Ok(response)
}

fn browser_step_distance_css(
    selection_height: u32,
    device_pixel_ratio: f64,
) -> Result<u32, String> {
    if selection_height == 0
        || !device_pixel_ratio.is_finite()
        || !(0.1..=16.0).contains(&device_pixel_ratio)
    {
        return Err("browser capture geometry is invalid".to_string());
    }
    let physical_distance = f64::from(selection_height) * (1.0_f64 - BROWSER_MIN_OVERLAP_RATIO);
    let css_distance = (physical_distance / device_pixel_ratio).floor();
    if css_distance < 1.0 || css_distance > f64::from(u32::MAX) {
        return Err("capture selection is too short for browser-enhanced scrolling".to_string());
    }
    Ok(css_distance as u32)
}

fn prepare_browser_capture(job: &LongCaptureJob) -> Result<(), String> {
    let Some(context) = job.browser.as_ref() else {
        return Err("browser enhancement is unavailable".to_string());
    };
    let prepared = browser_prepare_request(
        context,
        "prepare",
        serde_json::json!({
            "anchor": {
                "x": context.anchor_client_physical.x,
                "y": context.anchor_client_physical.y
            },
            "coordinateSpace": "clientPhysical",
            "domQuietMs": 120,
            "domQuietTimeoutMs": 2_500,
        }),
    )?;
    if prepared.get("prepared").and_then(Value::as_bool) != Some(true) {
        return Err("browser extension did not prepare the capture target".to_string());
    }
    let binding = parse_browser_session_binding(&prepared)?;
    {
        let mut runtime = lock_unpoisoned(&job.runtime);
        runtime.browser_session = Some(binding.clone());
        runtime.browser_restore_needed = true;
    }
    if requested_worker_stop(job).is_some() {
        return Err("browser capture startup was canceled".to_string());
    }
    let started = browser_bound_request(
        context,
        &binding,
        "start",
        serde_json::json!({
            "from": if job.target.mode == LongCaptureMode::Top { "top" } else { "current" },
            "domQuietMs": 120,
            "domQuietTimeoutMs": 2_500,
        }),
    )?;
    if started.get("started").and_then(Value::as_bool) != Some(true)
        || started.get("state").and_then(Value::as_str) != Some("capturing")
    {
        return Err("browser extension did not start capture mode".to_string());
    }
    if requested_worker_stop(job).is_some() {
        return Err("browser capture startup was canceled".to_string());
    }
    let status = browser_bound_request(context, &binding, "status", serde_json::json!({}))?;
    if status.get("state").and_then(Value::as_str) != Some("capturing") {
        return Err("browser extension capture session changed during startup".to_string());
    }
    if requested_worker_stop(job).is_some() {
        return Err("browser capture startup was canceled".to_string());
    }
    let mut runtime = lock_unpoisoned(&job.runtime);
    runtime.browser_active = true;
    Ok(())
}

fn step_browser_capture(job: &LongCaptureJob) -> Result<BrowserStepResult, String> {
    let context = job
        .browser
        .as_ref()
        .ok_or_else(|| "browser enhancement is unavailable".to_string())?;
    let binding = lock_unpoisoned(&job.runtime)
        .browser_session
        .clone()
        .ok_or_else(|| "browser capture session was not prepared".to_string())?;
    let distance_px =
        browser_step_distance_css(job.target.bounds.height, binding.device_pixel_ratio)?;
    let response = browser_bound_request(
        context,
        &binding,
        "step",
        serde_json::json!({
            "distancePx": distance_px,
            "bottomTolerancePx": 2,
            "domQuietMs": 120,
            "domQuietTimeoutMs": 2_500,
        }),
    )?;
    if response.get("state").and_then(Value::as_str) != Some("capturing") {
        return Err("browser extension capture session is no longer active".to_string());
    }
    Ok(parse_browser_step(&response))
}

fn parse_browser_step(response: &Value) -> BrowserStepResult {
    BrowserStepResult {
        moved: response
            .get("moved")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        at_bottom: response
            .get("atBottom")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        actual_distance: response
            .get("actualDistance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    }
}

fn fallback_to_wheel(job: &LongCaptureJob, reason: &str) -> AppResult<Option<LongCaptureStatus>> {
    let short_reason = reason.chars().take(180).collect::<String>();
    let status = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        runtime.browser_active = false;
        if runtime.manifest.state.is_terminal() {
            None
        } else {
            runtime.manifest.engine = LongCaptureEngine::Wheel;
            runtime.manifest.message =
                format!("浏览器增强已断开，继续使用通用滚动: {short_reason}");
            persist_runtime(job, &runtime)?;
            Some(status_from_runtime(&runtime))
        }
    };
    let _ = restore_browser_session(job);
    Ok(status)
}

fn restore_browser_session(job: &LongCaptureJob) -> Result<(), String> {
    // Claim restoration while holding the runtime lock. The worker guard and
    // application shutdown can race, but only one may issue a restore command.
    let binding = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        claim_browser_restore(&mut runtime)
    };
    let Some(binding) = binding else {
        return Ok(());
    };
    let Some(context) = job.browser.as_ref() else {
        return Ok(());
    };
    browser_bound_request(context, &binding, "restore", serde_json::json!({})).map(|_| ())
}

fn claim_browser_restore(runtime: &mut LongCaptureRuntime) -> Option<BrowserSessionBinding> {
    if !runtime.browser_restore_needed {
        return None;
    }
    runtime.browser_restore_needed = false;
    runtime.browser_active = false;
    runtime.browser_session.clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStop {
    Finish,
    Cancel,
}

fn requested_worker_stop(job: &LongCaptureJob) -> Option<WorkerStop> {
    let runtime = lock_unpoisoned(&job.runtime);
    if runtime.cancel_requested || runtime.manifest.state == LongCaptureState::Canceled {
        Some(WorkerStop::Cancel)
    } else if runtime.finish_requested || runtime.manifest.state == LongCaptureState::Ready {
        Some(WorkerStop::Finish)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerCheckpoint {
    generation: u64,
    retry_current: bool,
}

fn capture_worker(app: AppHandle, job: Arc<LongCaptureJob>) {
    let browser_restore = BrowserRestoreGuard::new(Arc::clone(&job));
    let original_cursor = current_cursor_position();
    let started_status = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state == LongCaptureState::Preparing {
            runtime.manifest.state = LongCaptureState::Capturing;
            runtime.manifest.message =
                if runtime.manifest.engine == LongCaptureEngine::BrowserEnhanced {
                    "正在准备浏览器增强捕获".to_string()
                } else if runtime.manifest.engine == LongCaptureEngine::Manual {
                    "正在捕获首帧，随后请手动滚动".to_string()
                } else {
                    "正在捕获长截图首帧".to_string()
                };
            let _ = persist_runtime(&job, &runtime);
        }
        status_from_runtime(&runtime)
    };
    let _ = app.emit("long_capture_progress", &started_status);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_capture_worker(&app, &job)
    }))
    .unwrap_or_else(|panic| {
        let message = if let Some(message) = panic.downcast_ref::<&str>() {
            (*message).to_string()
        } else if let Some(message) = panic.downcast_ref::<String>() {
            message.clone()
        } else {
            "未知 Rust panic".to_string()
        };
        Err(AppError::new(
            "long_capture_worker_panic",
            format!("长截图后台任务异常中止: {message}"),
        ))
    });
    let canceled = matches!(&result, Ok(WorkerStop::Cancel));
    browser_restore.restore_now();
    if let Some(position) = original_cursor {
        let _ = set_cursor_position(position);
    }

    match result {
        Ok(WorkerStop::Cancel) => {}
        Ok(WorkerStop::Finish) => {}
        Err(error) => {
            let (status, transitioned) = {
                let mut runtime = lock_unpoisoned(&job.runtime);
                let transitioned = transition_to_failed(&mut runtime.manifest.state);
                if transitioned {
                    runtime.manifest.message = error.message.clone();
                    let _ = persist_runtime(&job, &runtime);
                }
                (status_from_runtime(&runtime), transitioned)
            };
            if transitioned {
                let status = match switch_visible_surface(
                    || show_capture_overlay(&app, &job.target.monitor),
                    || close_control_window(&app),
                ) {
                    Ok(()) => status,
                    Err(surface_error) => {
                        let failure = recover_capture_surface(
                            &app,
                            &job,
                            append_recovery_error(error, "恢复截图界面失败", &surface_error),
                        );
                        let mut runtime = lock_unpoisoned(&job.runtime);
                        if runtime.manifest.state == LongCaptureState::Failed {
                            runtime.manifest.message = failure.message;
                            let _ = persist_runtime(&job, &runtime);
                        }
                        status_from_runtime(&runtime)
                    }
                };
                let _ = app.emit("long_capture_failed", &status);
            } else {
                eprintln!(
                    "忽略长截图任务终态 {:?} 后的线程错误: {}",
                    status.state, error
                );
            }
        }
    }

    complete_worker_after_cleanup(&job, || {
        let (cleanup_requested, job_id) = {
            let runtime = lock_unpoisoned(&job.runtime);
            (
                should_cleanup_after_worker(&runtime),
                runtime.manifest.job_id.clone(),
            )
        };
        restore_hidden_pin_windows(&app, &job.hidden_pin_labels);
        if canceled || cleanup_requested {
            clear_job_cache(&app.state::<LongScreenshotStore>(), &job_id);
            // The store may already have stopped owning this job. Its cache is
            // still private to the worker and must be gone before notifying.
            let _ = std::fs::remove_dir_all(&job.directory);
        }
    });
}

fn run_capture_worker(app: &AppHandle, job: &Arc<LongCaptureJob>) -> AppResult<WorkerStop> {
    let mut browser_enhanced = {
        let runtime = lock_unpoisoned(&job.runtime);
        runtime.manifest.engine == LongCaptureEngine::BrowserEnhanced
    };
    if browser_enhanced {
        if let Err(error) = prepare_browser_capture(job) {
            if let Some(status) = fallback_to_wheel(job, &error)? {
                let _ = app.emit("long_capture_progress", status);
            }
            browser_enhanced = false;
        }
    }
    if let Some(stop) = requested_worker_stop(job) {
        return Ok(stop);
    }
    let manual_scrolling = {
        let runtime = lock_unpoisoned(&job.runtime);
        engine_uses_manual_scrolling(runtime.manifest.engine)
    };
    let manual_input = if manual_scrolling {
        match ScrollInputMonitor::start() {
            Ok(input) => Some(input),
            Err(error) => {
                eprintln!("系统滚轮监听不可用，长截图改用画面变化兼容检测: {error}");
                None
            }
        }
    } else {
        None
    };
    if job.target.mode == LongCaptureMode::Top && !browser_enhanced && !manual_scrolling {
        scroll_target_to_top(app, job)?;
    }
    let mut previous = loop {
        let checkpoint = match wait_for_worker(job) {
            Ok(checkpoint) => checkpoint,
            Err(stop) => return Ok(stop),
        };
        std::thread::sleep(Duration::from_millis(120));
        let frame = capture_job_roi(app, job, false)?;
        if !iteration_is_current(job, checkpoint.generation) {
            continue;
        }
        if !accept_initial_frame(job, &frame, checkpoint.generation)? {
            continue;
        }
        emit_progress(app, job);
        break frame;
    };

    let mut known_generation = {
        let runtime = lock_unpoisoned(&job.runtime);
        runtime.generation
    };
    let mut manual_input_snapshot = manual_input
        .as_ref()
        .map(ScrollInputMonitor::snapshot)
        .unwrap_or_default();
    let mut needs_scroll = true;
    let mut no_motion_count = 0_u8;
    let mut low_confidence_count = 0_u8;
    let mut wheel_delta_units = AUTO_SCROLL_INITIAL_DELTA;
    let mut fixed_bottom_tracker = FixedBottomTracker::default();

    loop {
        let checkpoint = match wait_for_worker(job) {
            Ok(checkpoint) => checkpoint,
            Err(stop) => return Ok(stop),
        };
        if checkpoint.generation != known_generation {
            previous = load_latest_accepted_frame(job)?;
            known_generation = checkpoint.generation;
            if let Some(input) = manual_input.as_ref() {
                manual_input_snapshot = input.snapshot();
            }
            needs_scroll = !checkpoint.retry_current;
            no_motion_count = 0;
            low_confidence_count = 0;
            fixed_bottom_tracker = FixedBottomTracker::default();
        } else if checkpoint.retry_current {
            needs_scroll = false;
            low_confidence_count = 0;
        }

        let mut browser_step = None;
        if needs_scroll && !manual_scrolling {
            if browser_enhanced {
                match step_browser_capture(job) {
                    Ok(step) => {
                        let _actual_distance = step.actual_distance;
                        browser_step = Some(step);
                    }
                    Err(error) => {
                        if let Some(status) = fallback_to_wheel(job, &error)? {
                            let _ = app.emit("long_capture_progress", status);
                        }
                        browser_enhanced = false;
                        if let Some(stop) = requested_worker_stop(job) {
                            return Ok(stop);
                        }
                        // The extension may have completed the scroll just as
                        // the bridge timed out. Capture once before issuing a
                        // wheel event so a late response cannot skip a page.
                    }
                }
            } else {
                let scroll_result = {
                    let _operation_guard = job
                        .target
                        .control_overlaps_roi
                        .then(|| lock_unpoisoned(&job.operation_lock));
                    if job.target.control_overlaps_roi {
                        hide_control_panel(app)?;
                        flush_desktop_compositor();
                    }
                    send_wheel_scroll(&job.target, wheel_delta_units)
                };
                if let Err(scroll_error) = scroll_result {
                    if job.target.control_overlaps_roi {
                        let _operation_guard = lock_unpoisoned(&job.operation_lock);
                        let should_restore = {
                            let runtime = lock_unpoisoned(&job.runtime);
                            control_surface_needed(&runtime)
                        };
                        if should_restore {
                            if let Err(restore_error) = show_control_panel(app, job) {
                                return Err(append_recovery_error(
                                    scroll_error,
                                    "恢复长截图控制窗口失败",
                                    &restore_error,
                                ));
                            }
                        }
                    }
                    return Err(scroll_error);
                }
            }
        }
        let (current, detected_overlap) = if manual_scrolling && needs_scroll {
            wait_for_manual_scroll(
                app,
                job,
                &previous,
                checkpoint.generation,
                manual_input.as_ref(),
                &mut manual_input_snapshot,
            )?
        } else {
            (capture_job_roi(app, job, true)?, None)
        };
        if !iteration_is_current(job, checkpoint.generation) {
            continue;
        }

        // Try the stronger displacement estimator first. A small scroll in a
        // sparse or mostly blank window can have a tiny whole-frame average
        // difference while still exposing valid new rows.
        let overlap = detected_overlap.or_else(|| find_vertical_overlap(&previous, &current));
        let stationary_score = sampled_frame_difference(&previous, &current);
        if overlap.is_none() && stationary_score <= 2.25 {
            no_motion_count = if browser_step.is_some_and(|step| step.at_bottom && !step.moved) {
                AUTO_SCROLL_END_CONFIRMATIONS
            } else {
                no_motion_count.saturating_add(1)
            };
            if !manual_scrolling && !browser_enhanced {
                // Some classic Win32 controls only react after accumulated
                // wheel input reaches WHEEL_DELTA. Grow through 30/60/120
                // before concluding that the target is at the bottom.
                wheel_delta_units = wheel_delta_units.saturating_mul(2).clamp(30, 240);
            }
            needs_scroll = true;
            update_message(
                job,
                checkpoint.generation,
                if no_motion_count >= AUTO_SCROLL_END_CONFIRMATIONS {
                    "已检测到滚动区域底部"
                } else {
                    "未检测到新内容，正在确认是否到底"
                },
            )?;
            emit_progress(app, job);
            if no_motion_count >= AUTO_SCROLL_END_CONFIRMATIONS {
                if job.status().frame_count <= 1 {
                    pause_for_attention(
                        app,
                        job,
                        checkpoint.generation,
                        "目标内容没有滚动。请返回普通截图，重新点击选区内真正可滚动的内容",
                    )?;
                    needs_scroll = false;
                    continue;
                }
                finish_worker_capture(app, job, "已到达滚动区域底部")?;
                return Ok(WorkerStop::Finish);
            }
            continue;
        }
        no_motion_count = 0;

        let Some(overlap) = overlap else {
            low_confidence_count = low_confidence_count.saturating_add(1);
            if !manual_scrolling && !browser_enhanced {
                // The visual surface moved but no safe overlap remained. Put
                // it back where the accepted frame was captured, reduce the
                // step, and retry instead of repeatedly sampling the same bad
                // position or committing a guessed seam.
                send_wheel_scroll(&job.target, -wheel_delta_units)?;
                std::thread::sleep(Duration::from_millis(260));
                wheel_delta_units = (wheel_delta_units / 2).max(30);
                needs_scroll = true;
                update_message(
                    job,
                    checkpoint.generation,
                    "本次滚动超出可靠重叠范围，已回滚并缩小步长",
                )?;
                emit_progress(app, job);
                if low_confidence_count < LOW_CONFIDENCE_LIMIT {
                    continue;
                }
            } else {
                needs_scroll = false;
            }
            if low_confidence_count >= LOW_CONFIDENCE_LIMIT {
                pause_for_attention(
                    app,
                    job,
                    checkpoint.generation,
                    "连续三次无法可靠定位接缝，请重试当前片段或手动调整滚动位置",
                )?;
            } else {
                update_message(
                    job,
                    checkpoint.generation,
                    "当前片段接缝置信度不足，正在重新采样",
                )?;
                std::thread::sleep(Duration::from_millis(180));
            }
            continue;
        };

        let reached_limit = accept_scrolled_frame(
            job,
            &current,
            overlap,
            checkpoint.generation,
            &mut fixed_bottom_tracker,
        )?;
        if !manual_scrolling && !browser_enhanced {
            wheel_delta_units = calibrated_wheel_delta_units(
                job.target.bounds.height,
                overlap.displacement,
                wheel_delta_units,
            );
        }
        previous = current;
        needs_scroll = true;
        low_confidence_count = 0;
        emit_progress(app, job);
        if job.target.control_overlaps_roi {
            std::thread::sleep(Duration::from_millis(240));
        }
        if reached_limit {
            finish_worker_capture(app, job, "已达到长截图资源上限")?;
            return Ok(WorkerStop::Finish);
        }
    }
}

fn capture_job_roi(
    app: &AppHandle,
    job: &LongCaptureJob,
    wait_for_settle: bool,
) -> AppResult<Frame> {
    let capture = || {
        if wait_for_settle {
            capture_settled_roi(job.target.bounds)
        } else {
            capture_roi(job.target.bounds)
        }
    };
    if !job.target.control_overlaps_roi {
        return capture();
    }

    // Reentry, pause and cancel use the same lock. Only the panel is hidden;
    // the click-through outline stays outside the selected pixels and remains
    // visible while frames are sampled.
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    hide_control_panel(app)?;
    flush_desktop_compositor();
    let result = capture();
    let should_restore_control = {
        let runtime = lock_unpoisoned(&job.runtime);
        control_surface_needed(&runtime)
    };
    if !should_restore_control {
        return result;
    }
    if let Err(restore_error) = show_control_panel(app, job) {
        let _ = hide_outline_window(app);
        let failure = match result {
            Ok(_) => restore_error,
            Err(capture_error) => {
                append_recovery_error(capture_error, "恢复长截图控制窗口失败", &restore_error)
            }
        };
        return Err(recover_capture_surface(app, job, failure));
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualCaptureTrigger {
    ActiveScroll,
    Settled,
    FallbackPoll,
}

#[derive(Debug)]
struct ManualCaptureSchedule {
    last_capture_at: Instant,
    settle_deadline: Option<Instant>,
    next_fallback_at: Instant,
}

impl ManualCaptureSchedule {
    fn new(now: Instant) -> Self {
        Self {
            last_capture_at: now
                .checked_sub(MANUAL_SCROLL_ACTIVE_CAPTURE_INTERVAL)
                .unwrap_or(now),
            settle_deadline: None,
            next_fallback_at: now + MANUAL_SCROLL_FALLBACK_POLL_INTERVAL,
        }
    }

    fn observe_scroll(&mut self, now: Instant) {
        self.settle_deadline = Some(now + MANUAL_SCROLL_SETTLE_AFTER);
    }

    fn next_trigger(&mut self, now: Instant) -> Option<ManualCaptureTrigger> {
        let trigger = if self.settle_deadline.is_some_and(|deadline| now >= deadline) {
            self.settle_deadline = None;
            Some(ManualCaptureTrigger::Settled)
        } else if self.settle_deadline.is_some()
            && now.duration_since(self.last_capture_at) >= MANUAL_SCROLL_ACTIVE_CAPTURE_INTERVAL
        {
            Some(ManualCaptureTrigger::ActiveScroll)
        } else if now >= self.next_fallback_at {
            Some(ManualCaptureTrigger::FallbackPoll)
        } else {
            None
        }?;
        self.last_capture_at = now;
        self.next_fallback_at = now + MANUAL_SCROLL_FALLBACK_POLL_INTERVAL;
        Some(trigger)
    }
}

#[derive(Debug, Default)]
struct ManualUnmatchedMotion {
    started_at: Option<Instant>,
}

impl ManualUnmatchedMotion {
    fn observe(&mut self, now: Instant, visible_movement: bool) -> bool {
        if !visible_movement {
            self.started_at = None;
            return false;
        }
        let started_at = *self.started_at.get_or_insert(now);
        now.saturating_duration_since(started_at) >= MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER
    }
}

fn wait_for_manual_scroll(
    app: &AppHandle,
    job: &LongCaptureJob,
    previous: &Frame,
    generation: u64,
    input: Option<&ScrollInputMonitor>,
    observed_input: &mut ScrollInputSnapshot,
) -> AppResult<(Frame, Option<OverlapMatch>)> {
    let mut input_available = input.is_some_and(ScrollInputMonitor::is_running);
    update_message(
        job,
        generation,
        if input_available {
            "等待在选区内滚动"
        } else {
            "等待内容滚动（兼容检测模式）"
        },
    )?;
    emit_progress(app, job);
    let started = Instant::now();
    let mut schedule = ManualCaptureSchedule::new(started);
    let mut unmatched_motion = ManualUnmatchedMotion::default();
    let mut movement_seen = false;
    let mut feedback_for_movement = None;
    loop {
        if !iteration_is_current(job, generation) {
            return Ok((previous.clone(), None));
        }

        let now = Instant::now();
        input_available = input.is_some_and(ScrollInputMonitor::is_running);
        if let Some(input) = input.filter(|input| input.is_running()) {
            let snapshot = input.snapshot();
            let received_vertical_wheel =
                snapshot.vertical_events != observed_input.vertical_events;
            *observed_input = snapshot;
            if received_vertical_wheel && cursor_inside_capture_bounds(job) {
                schedule.observe_scroll(now);
            }
        }

        let Some(trigger) = schedule.next_trigger(now) else {
            if started.elapsed() >= MANUAL_SCROLL_FEEDBACK_AFTER && feedback_for_movement.is_none()
            {
                update_message(
                    job,
                    generation,
                    if input_available {
                        "请在选区内向下滚动；也支持键盘和拖动滚动条"
                    } else {
                        "正在兼容检测画面变化；请滚动、按翻页键或拖动滚动条"
                    },
                )?;
                emit_progress(app, job);
                feedback_for_movement = Some(false);
            }
            std::thread::sleep(MANUAL_SCROLL_STOP_CHECK_INTERVAL);
            continue;
        };

        let candidate = capture_manual_candidate(app, job, previous)?;
        if !iteration_is_current(job, generation) {
            return Ok((candidate, None));
        }
        let visual_change = sparse_frame_change(previous, &candidate);
        let match_probe_change =
            visual_change.is_some_and(SparseFrameChange::is_match_probe_candidate);
        let visible_movement =
            visual_change.is_some_and(SparseFrameChange::is_visible_movement_candidate);
        let overlap = (trigger != ManualCaptureTrigger::FallbackPoll || match_probe_change)
            .then(|| find_vertical_overlap(previous, &candidate))
            .flatten();
        if let Some(overlap) = overlap {
            if !job.target.control_overlaps_roi {
                return Ok((candidate, Some(overlap)));
            }
            let clean = capture_job_roi(app, job, false)?;
            if let Some(overlap) = find_vertical_overlap(previous, &clean) {
                return Ok((clean, Some(overlap)));
            }
        }
        if unmatched_motion.observe(Instant::now(), visible_movement) {
            pause_for_attention(
                app,
                job,
                generation,
                "滚动跨度已超出可靠接缝范围，长截图已暂停以避免漏段。请向上回滚，让上一段已确认内容重新出现在选区中，再点击继续",
            )?;
            return Ok((candidate, None));
        }
        movement_seen |= visible_movement;
        if trigger != ManualCaptureTrigger::FallbackPoll || movement_seen {
            if feedback_for_movement != Some(movement_seen) {
                update_message(
                    job,
                    generation,
                    if movement_seen {
                        "已检测到画面滚动，但还没有可靠接缝；请放慢滚动或向上回滚少量内容"
                    } else {
                        "已收到滚轮，但选区内容没有变化；请确认鼠标位于真正可滚动的内容上"
                    },
                )?;
                emit_progress(app, job);
                feedback_for_movement = Some(movement_seen);
            }
        }
    }
}

fn cursor_inside_capture_bounds(job: &LongCaptureJob) -> bool {
    let Some(cursor) = current_cursor_position() else {
        return false;
    };
    let bounds = job.target.bounds;
    let right = i64::from(bounds.x) + i64::from(bounds.width);
    let bottom = i64::from(bounds.y) + i64::from(bounds.height);
    cursor.x >= bounds.x
        && cursor.y >= bounds.y
        && i64::from(cursor.x) < right
        && i64::from(cursor.y) < bottom
}

fn capture_manual_candidate(
    app: &AppHandle,
    job: &LongCaptureJob,
    reference: &Frame,
) -> AppResult<Frame> {
    let Some(mask) = control_overlap_in_roi(&job.target) else {
        return capture_job_roi(app, job, false);
    };
    let mut frame = capture_roi(job.target.bounds)?;
    copy_frame_region(&mut frame, reference, mask)?;
    Ok(frame)
}

fn scroll_target_to_top(app: &AppHandle, job: &LongCaptureJob) -> AppResult<()> {
    let mut previous = capture_job_roi(app, job, false)?;
    let mut no_motion = 0_u8;
    for attempt in 0..TOP_SCROLL_MAX_ATTEMPTS {
        {
            let runtime = lock_unpoisoned(&job.runtime);
            if runtime.cancel_requested || runtime.finish_requested {
                return Ok(());
            }
        }
        send_scroll_ticks(&job.target, 20)?;
        let current = capture_job_roi(app, job, true)?;
        if sampled_frame_difference(&previous, &current) <= 1.5 {
            no_motion = no_motion.saturating_add(1);
        } else {
            no_motion = 0;
        }
        if top_scroll_attempt_complete(attempt + 1, no_motion)? {
            return Ok(());
        }
        previous = current;
    }
    unreachable!("top scroll attempt bound is checked on every iteration")
}

fn top_scroll_attempt_complete(attempts: usize, no_motion: u8) -> AppResult<bool> {
    if no_motion >= TOP_SCROLL_NO_MOTION_CONFIRMATIONS {
        return Ok(true);
    }
    if attempts >= TOP_SCROLL_MAX_ATTEMPTS {
        return Err(AppError::new(
            "long_capture_top_not_reached",
            "连续向上滚动后仍未确认到达顶部，请改用当前位置或手动模式重试",
        ));
    }
    Ok(false)
}

fn wait_for_worker(job: &LongCaptureJob) -> Result<WorkerCheckpoint, WorkerStop> {
    let mut runtime = lock_unpoisoned(&job.runtime);
    loop {
        if runtime.cancel_requested || runtime.manifest.state == LongCaptureState::Canceled {
            return Err(WorkerStop::Cancel);
        }
        if runtime.finish_requested || runtime.manifest.state == LongCaptureState::Ready {
            return Err(WorkerStop::Finish);
        }
        if !runtime.pause_requested && runtime.manifest.state == LongCaptureState::Capturing {
            let checkpoint = WorkerCheckpoint {
                generation: runtime.generation,
                retry_current: runtime.retry_current,
            };
            runtime.retry_current = false;
            return Ok(checkpoint);
        }
        runtime = job
            .wake
            .wait(runtime)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

fn iteration_is_current(job: &LongCaptureJob, generation: u64) -> bool {
    let runtime = lock_unpoisoned(&job.runtime);
    runtime.generation == generation
        && !runtime.pause_requested
        && !runtime.cancel_requested
        && !runtime.finish_requested
        && runtime.manifest.state == LongCaptureState::Capturing
}

fn accept_initial_frame(job: &LongCaptureJob, frame: &Frame, generation: u64) -> AppResult<bool> {
    let frame_file = "frames/000000.png".to_string();
    atomic_write(&job.directory.join(&frame_file), &encode_png(frame)?)?;
    let mut runtime = lock_unpoisoned(&job.runtime);
    if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing {
        let _ = std::fs::remove_file(job.directory.join(&frame_file));
        return Ok(false);
    }
    runtime.manifest.height = frame.height;
    runtime.manifest.message = if engine_uses_manual_scrolling(runtime.manifest.engine) {
        "已捕获首帧，请在目标窗口中滚动内容".to_string()
    } else {
        "已捕获首帧，正在自动滚动".to_string()
    };
    runtime.manifest.segments.push(LongCaptureSegment {
        index: 0,
        output_y: 0,
        height: frame.height,
        displacement: frame.height,
        confidence: 1.0,
        frame_file: frame_file.clone(),
        strip_file: frame_file,
    });
    persist_runtime(job, &runtime)?;
    Ok(true)
}

fn engine_uses_manual_scrolling(engine: LongCaptureEngine) -> bool {
    engine == LongCaptureEngine::Manual
}

fn focus_capture_target(job: &LongCaptureJob) -> AppResult<()> {
    let engine = {
        let runtime = lock_unpoisoned(&job.runtime);
        runtime.manifest.engine
    };
    match focus_scroll_target(&job.target) {
        Ok(()) => Ok(()),
        Err(error) if engine_uses_manual_scrolling(engine) => {
            eprintln!(
                "手动长截图未能自动激活目标窗口，等待用户自行点击目标: {}",
                error.message
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

const FIXED_BOTTOM_FILE: &str = "fixed-bottom.png";

fn fixed_bottom_height(manifest: &LongCaptureManifest) -> u32 {
    manifest
        .fixed_bottom
        .as_ref()
        .map(|fixed| fixed.height)
        .unwrap_or(0)
}

fn write_fixed_bottom(job: &LongCaptureJob, frame: &Frame, height: u32) -> AppResult<()> {
    let start = frame
        .height
        .checked_sub(height)
        .ok_or_else(|| AppError::invalid("长截图固定底栏高度超过当前帧"))?;
    let footer = crop_rows(frame, start, height)?;
    atomic_write(
        &job.directory.join(FIXED_BOTTOM_FILE),
        &encode_png(&footer)?,
    )
}

fn rebuild_strips_with_fixed_bottom(
    job: &LongCaptureJob,
    manifest: &mut LongCaptureManifest,
    height: u32,
) -> AppResult<()> {
    if height < 6 || height >= manifest.selection.height / 2 {
        return Err(AppError::invalid("长截图固定底栏高度无效"));
    }
    let mut output_y = 0_u32;
    let mut rebuilt = Vec::with_capacity(manifest.segments.len());
    let mut latest_frame = None;
    for (position, segment) in manifest.segments.iter().enumerate() {
        let frame = decode_png(
            &std::fs::read(job.directory.join(&segment.frame_file))
                .map_err(|error| AppError::io("读取长截图固定底栏源帧", error))?,
        )?;
        let body_end = frame
            .height
            .checked_sub(height)
            .ok_or_else(|| AppError::invalid("长截图固定底栏超过源帧"))?;
        let (start, rows) = if position == 0 {
            (0, body_end)
        } else {
            (
                body_end
                    .checked_sub(segment.displacement)
                    .ok_or_else(|| AppError::invalid("长截图固定底栏片段位移无效"))?,
                segment.height.min(segment.displacement),
            )
        };
        let strip = crop_rows(&frame, start, rows)?;
        let strip_file = format!("strips/{:06}.png", segment.index);
        atomic_write(&job.directory.join(&strip_file), &encode_png(&strip)?)?;
        let mut rebuilt_segment = segment.clone();
        rebuilt_segment.output_y = output_y;
        rebuilt_segment.height = rows;
        rebuilt_segment.strip_file = strip_file;
        output_y = output_y.saturating_add(rows);
        rebuilt.push(rebuilt_segment);
        latest_frame = Some(frame);
    }
    let latest_frame = latest_frame
        .ok_or_else(|| AppError::new("long_capture_not_ready", "长截图没有固定底栏源帧"))?;
    write_fixed_bottom(job, &latest_frame, height)?;
    manifest.segments = rebuilt;
    manifest.fixed_bottom = Some(LongCaptureFixedBottom {
        height,
        file: FIXED_BOTTOM_FILE.to_string(),
    });
    manifest.height = output_y.saturating_add(height);
    Ok(())
}

fn refresh_fixed_bottom_from_latest_frame(
    job: &LongCaptureJob,
    manifest: &LongCaptureManifest,
) -> AppResult<()> {
    let Some(fixed_bottom) = manifest.fixed_bottom.as_ref() else {
        return Ok(());
    };
    let frame_file = manifest
        .segments
        .last()
        .map(|segment| segment.frame_file.as_str())
        .ok_or_else(|| AppError::new("long_capture_not_ready", "长截图没有固定底栏源帧"))?;
    let frame = decode_png(
        &std::fs::read(job.directory.join(frame_file))
            .map_err(|error| AppError::io("读取长截图固定底栏源帧", error))?,
    )?;
    write_fixed_bottom(job, &frame, fixed_bottom.height)
}

fn accept_scrolled_frame(
    job: &LongCaptureJob,
    frame: &Frame,
    overlap: OverlapMatch,
    generation: u64,
    fixed_bottom_tracker: &mut FixedBottomTracker,
) -> AppResult<bool> {
    let (index, output_y, allowed_rows, fixed_bottom) = {
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing
        {
            return Ok(false);
        }
        let maximum_height = maximum_height_for_width(runtime.manifest.width);
        let remaining = maximum_height.saturating_sub(runtime.manifest.height);
        let fixed_bottom = fixed_bottom_height(&runtime.manifest);
        (
            runtime.manifest.segments.len() as u32,
            runtime.manifest.height.saturating_sub(fixed_bottom),
            overlap.displacement.min(remaining),
            fixed_bottom,
        )
    };
    if allowed_rows == 0 {
        return Ok(true);
    }

    let frame_file = format!("frames/{index:06}.png");
    let strip_file = format!("strips/{index:06}.png");
    let body_end = frame
        .height
        .checked_sub(fixed_bottom)
        .ok_or_else(|| AppError::invalid("长截图固定底栏超过当前帧"))?;
    let strip_start = body_end
        .checked_sub(overlap.displacement)
        .ok_or_else(|| AppError::invalid("长截图接缝位移超过正文区域"))?;
    let strip = crop_rows(frame, strip_start, allowed_rows)?;
    atomic_write(&job.directory.join(&frame_file), &encode_png(frame)?)?;
    atomic_write(&job.directory.join(&strip_file), &encode_png(&strip)?)?;

    let mut runtime = lock_unpoisoned(&job.runtime);
    if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing {
        let _ = std::fs::remove_file(job.directory.join(&frame_file));
        let _ = std::fs::remove_file(job.directory.join(&strip_file));
        return Ok(false);
    }
    if fixed_bottom > 0 {
        write_fixed_bottom(job, frame, fixed_bottom)?;
    }
    runtime.manifest.segments.push(LongCaptureSegment {
        index,
        output_y,
        height: allowed_rows,
        displacement: overlap.displacement,
        confidence: overlap.confidence,
        frame_file,
        strip_file,
    });
    runtime.manifest.height = output_y
        .saturating_add(allowed_rows)
        .saturating_add(fixed_bottom);
    if runtime.manifest.fixed_bottom.is_none() {
        if let Some(height) = fixed_bottom_tracker.observe(overlap.static_bottom) {
            rebuild_strips_with_fixed_bottom(job, &mut runtime.manifest, height)?;
        }
    }
    runtime.manifest.message = format!(
        "已确认第 {} 帧，接缝置信度 {:.0}%",
        runtime.manifest.segments.len(),
        overlap.confidence * 100.0
    );
    persist_runtime(job, &runtime)?;
    Ok(allowed_rows < overlap.displacement
        || runtime.manifest.height >= maximum_height_for_width(runtime.manifest.width))
}

fn load_latest_accepted_frame(job: &LongCaptureJob) -> AppResult<Frame> {
    let frame_file = {
        let runtime = lock_unpoisoned(&job.runtime);
        runtime
            .manifest
            .segments
            .last()
            .map(|segment| segment.frame_file.clone())
            .ok_or_else(|| AppError::new("long_capture_not_ready", "长截图还没有可用帧"))?
    };
    decode_png(
        &std::fs::read(job.directory.join(frame_file))
            .map_err(|error| AppError::io("读取长截图帧", error))?,
    )
}

fn update_message(job: &LongCaptureJob, generation: u64, message: &str) -> AppResult<()> {
    let mut runtime = lock_unpoisoned(&job.runtime);
    if runtime.generation == generation && runtime.manifest.state == LongCaptureState::Capturing {
        runtime.manifest.message = message.to_string();
        persist_runtime(job, &runtime)?;
    }
    Ok(())
}

fn pause_for_attention(
    app: &AppHandle,
    job: &LongCaptureJob,
    generation: u64,
    message: &str,
) -> AppResult<()> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    let status = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing
        {
            return Ok(());
        }
        runtime.pause_requested = true;
        runtime.manifest.state = LongCaptureState::Paused;
        runtime.manifest.message = message.to_string();
        persist_runtime(job, &runtime)?;
        status_from_runtime(&runtime)
    };
    let _ = app.emit("long_capture_attention_required", &status);
    let _ = app.emit("long_capture_paused", &status);
    show_paused_capture_surface(app, job, status.engine)
}

fn finish_worker_capture(app: &AppHandle, job: &LongCaptureJob, message: &str) -> AppResult<()> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    let (status, should_emit) = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        if runtime.manifest.state.is_terminal() {
            return Ok(());
        }
        runtime.finish_requested = true;
        runtime.pause_requested = false;
        runtime.manifest.state = LongCaptureState::Ready;
        runtime.manifest.message = message.to_string();
        let should_emit = !runtime.ready_emitted;
        runtime.ready_emitted = true;
        persist_runtime(job, &runtime)?;
        (status_from_runtime(&runtime), should_emit)
    };
    if let Err(surface_error) = switch_visible_surface(
        || show_capture_overlay(app, &job.target.monitor),
        || close_control_window(app),
    ) {
        return Err(recover_capture_surface(app, job, surface_error));
    }
    if should_emit {
        let _ = app.emit("long_capture_ready", &status);
    }
    Ok(())
}

fn emit_progress(app: &AppHandle, job: &LongCaptureJob) {
    let _ = app.emit("long_capture_progress", job.status());
}

fn maximum_height_for_width(width: u32) -> u32 {
    if width == 0 {
        return 0;
    }
    MAX_LONG_HEIGHT.min((MAX_LONG_PIXELS / u64::from(width)) as u32)
}

#[derive(Debug, Clone)]
struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    fn validate(&self) -> AppResult<()> {
        let expected = checked_rgba_len(self.width, self.height, MAX_FRAME_PIXELS)?;
        if self.rgba.len() != expected {
            return Err(AppError::new("capture_error", "长截图像素缓冲区长度无效"));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct GrayStrips {
    height: usize,
    samples_per_row: usize,
    values: Vec<u8>,
    texture: f32,
}

impl GrayStrips {
    fn from_frame(frame: &Frame, x_positions: &[usize]) -> Self {
        let width = frame.width as usize;
        let height = frame.height as usize;
        let samples_per_row = x_positions.len();
        let mut values = Vec::with_capacity(height * samples_per_row);
        for y in 0..height {
            for &x in x_positions {
                let offset = (y * width + x) * 4;
                values.push(luma(&frame.rgba[offset..offset + 3]));
            }
        }
        let mut texture_sum = 0_u64;
        let mut texture_count = 0_u64;
        for y in 1..height {
            let row = y * samples_per_row;
            let previous = row - samples_per_row;
            for sample in 0..samples_per_row {
                texture_sum += u64::from(values[row + sample].abs_diff(values[previous + sample]));
                texture_count += 1;
            }
        }
        Self {
            height,
            samples_per_row,
            values,
            texture: if texture_count == 0 {
                0.0
            } else {
                texture_sum as f32 / texture_count as f32
            },
        }
    }
}

#[derive(Debug, Clone)]
struct MatchColumnGroup {
    index: usize,
    positions: Vec<usize>,
    score: f32,
    temporal_difference: f32,
}

#[derive(Debug)]
struct MatchColumnSelection {
    primary: Vec<usize>,
    verification: Vec<usize>,
}

fn distributed_column_groups(width: usize, maximum_groups: usize) -> Vec<Vec<usize>> {
    if width == 0 || maximum_groups == 0 {
        return Vec::new();
    }
    let group_count = width.min(maximum_groups).max(1);
    let span = (width / group_count.saturating_mul(6).max(1)).clamp(1, 6);
    (0..group_count)
        .map(|index| {
            let center = ((index * 2 + 1) * width / (group_count * 2)).min(width - 1);
            let mut positions = vec![
                center.saturating_sub(span),
                center,
                (center + span).min(width - 1),
            ];
            positions.sort_unstable();
            positions.dedup();
            positions
        })
        .collect()
}

fn column_group_score(left: &Frame, right: &Frame, positions: &[usize]) -> f32 {
    let width = left.width as usize;
    let height = left.height as usize;
    let row_step = (height / 720).max(1);
    let mut left_texture = 0_u64;
    let mut right_texture = 0_u64;
    let mut samples = 0_u64;
    for y in (row_step..height).step_by(row_step) {
        let previous_y = y - row_step;
        for &x in positions {
            let offset = (y * width + x) * 4;
            let previous_offset = (previous_y * width + x) * 4;
            let left_value = luma(&left.rgba[offset..offset + 3]);
            let right_value = luma(&right.rgba[offset..offset + 3]);
            left_texture += u64::from(
                left_value.abs_diff(luma(&left.rgba[previous_offset..previous_offset + 3])),
            );
            right_texture += u64::from(
                right_value.abs_diff(luma(&right.rgba[previous_offset..previous_offset + 3])),
            );
            samples += 1;
        }
    }
    if samples == 0 {
        return 0.0;
    }
    left_texture.min(right_texture) as f32 / samples as f32
}

fn column_group_temporal_difference(left: &Frame, right: &Frame, positions: &[usize]) -> f32 {
    let width = left.width as usize;
    let height = left.height as usize;
    let row_step = (height / 720).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for y in (0..height).step_by(row_step) {
        for &x in positions {
            let offset = (y * width + x) * 4;
            difference += u64::from(
                luma(&left.rgba[offset..offset + 3])
                    .abs_diff(luma(&right.rgba[offset..offset + 3])),
            );
            samples += 1;
        }
    }
    if samples == 0 {
        0.0
    } else {
        difference as f32 / samples as f32
    }
}

fn select_match_group_indices(
    candidates: &[MatchColumnGroup],
    requested_count: usize,
    excluded: &[usize],
) -> Vec<usize> {
    let available = candidates
        .iter()
        .filter(|candidate| !excluded.contains(&candidate.index))
        .count();
    let selected_count = requested_count.min(available);
    if selected_count == 0 {
        return Vec::new();
    }
    let band_count = MATCH_DISTRIBUTION_BANDS.min(selected_count).max(1);
    let candidate_count = candidates
        .iter()
        .map(|candidate| candidate.index)
        .max()
        .map(|index| index + 1)
        .unwrap_or(1);
    let mut selected = Vec::with_capacity(selected_count);
    for band in 0..band_count {
        if let Some(index) = candidates
            .iter()
            .filter(|candidate| {
                !excluded.contains(&candidate.index)
                    && candidate.index * band_count / candidate_count == band
            })
            .max_by(|left, right| left.score.total_cmp(&right.score))
            .map(|candidate| candidate.index)
        {
            selected.push(index);
        }
    }

    let mut ranked = candidates
        .iter()
        .filter(|candidate| !excluded.contains(&candidate.index))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.score.total_cmp(&left.score));
    for candidate in ranked {
        if selected.len() >= selected_count {
            break;
        }
        if !selected.contains(&candidate.index) {
            selected.push(candidate.index);
        }
    }
    selected
}

fn match_positions(candidates: &[MatchColumnGroup], selected: &[usize]) -> Vec<usize> {
    let mut positions = selected
        .iter()
        .filter_map(|index| {
            candidates
                .iter()
                .find(|candidate| candidate.index == *index)
        })
        .flat_map(|candidate| candidate.positions.iter().copied())
        .collect::<Vec<_>>();
    positions.sort_unstable();
    positions.dedup();
    positions
}

fn select_match_columns(left: &Frame, right: &Frame) -> MatchColumnSelection {
    let groups = distributed_column_groups(left.width as usize, MATCH_CANDIDATE_GROUPS);
    let candidates = groups
        .into_iter()
        .enumerate()
        .map(|(index, positions)| MatchColumnGroup {
            index,
            score: column_group_score(left, right, &positions),
            temporal_difference: column_group_temporal_difference(left, right, &positions),
            positions,
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return MatchColumnSelection {
            primary: Vec::new(),
            verification: Vec::new(),
        };
    }
    // Static sidebars and headers can contain more vertical texture than a
    // short code line. When at least two groups visibly changed, exclude only
    // fully stationary groups from displacement matching. This is a binary
    // eligibility gate rather than animation-amplitude weighting.
    let moving_candidates = candidates
        .iter()
        .filter(|candidate| candidate.score >= 1.1 && candidate.temporal_difference >= 0.75)
        .cloned()
        .collect::<Vec<_>>();
    let distributed_motion = sparse_frame_change(left, right)
        .is_some_and(SparseFrameChange::is_distributed_change_candidate);
    let selection_candidates = if distributed_motion && moving_candidates.len() >= 2 {
        &moving_candidates
    } else {
        &candidates
    };
    let primary_groups =
        select_match_group_indices(selection_candidates, MATCH_SELECTED_GROUPS, &[]);
    let verification_groups = select_match_group_indices(
        selection_candidates,
        MATCH_VERIFICATION_GROUPS,
        &primary_groups,
    );
    MatchColumnSelection {
        primary: match_positions(selection_candidates, &primary_groups),
        verification: match_positions(selection_candidates, &verification_groups),
    }
}

fn frames_have_matching_geometry(left: &Frame, right: &Frame) -> bool {
    left.width == right.width
        && left.height == right.height
        && left.width > 0
        && left.height > 0
        && left.validate().is_ok()
        && right.validate().is_ok()
}

#[cfg(test)]
fn gray_strips_for_pair(left: &Frame, right: &Frame) -> Option<(GrayStrips, GrayStrips)> {
    if !frames_have_matching_geometry(left, right) {
        return None;
    }
    let selection = select_match_columns(left, right);
    if selection.primary.is_empty() {
        return None;
    }
    Some((
        GrayStrips::from_frame(left, &selection.primary),
        GrayStrips::from_frame(right, &selection.primary),
    ))
}

#[derive(Debug, Clone, Copy)]
struct SparseFrameChange {
    changed_rows: usize,
    sampled_rows: usize,
    changed_samples: usize,
    strong_samples: usize,
    total_samples: usize,
    changed_groups: usize,
    sampled_groups: usize,
    changed_vertical_bands: usize,
}

impl SparseFrameChange {
    fn is_visible_movement_candidate(self) -> bool {
        self.is_match_probe_candidate() || self.is_distributed_change_candidate()
    }

    fn is_match_probe_candidate(self) -> bool {
        self.sampled_rows > 0
            && self.total_samples > 0
            && self.changed_rows * 100 >= self.sampled_rows * 2
            && self.changed_samples * 1_000 >= self.total_samples
            && self.strong_samples * 5_000 >= self.total_samples
            && self.changed_groups > 0
            && self.changed_vertical_bands >= 2
    }

    fn is_distributed_change_candidate(self) -> bool {
        let required_vertical_bands = CHANGE_VERTICAL_BANDS.min(self.sampled_rows).min(4);
        self.sampled_rows > 0
            && self.total_samples > 0
            && self.changed_rows * 100 >= self.sampled_rows * 8
            && self.changed_samples * 300 >= self.total_samples
            && self.strong_samples * 1_000 >= self.total_samples
            // Two independently sampled column groups are enough for a narrow
            // code or document column. Requiring horizontal bands made real
            // VS Code scrolling depend on how long each source line happened
            // to be.
            && self.changed_groups * 16 >= self.sampled_groups
            // A caret or compact animation can affect a few columns, but not
            // meaningful rows throughout most of the viewport.
            && self.changed_vertical_bands >= required_vertical_bands
    }
}

fn sparse_frame_change(left: &Frame, right: &Frame) -> Option<SparseFrameChange> {
    if !frames_have_matching_geometry(left, right) {
        return None;
    }
    let width = left.width as usize;
    let height = left.height as usize;
    let groups = distributed_column_groups(width, CHANGE_SAMPLE_GROUPS);
    let positions = groups
        .iter()
        .enumerate()
        .flat_map(|(group, positions)| positions.iter().map(move |position| (*position, group)))
        .collect::<Vec<_>>();
    let row_step = (height / 720).max(1);
    let changed_per_row = (positions.len() / 48).max(1);
    let mut changed_rows = 0_usize;
    let mut sampled_rows = 0_usize;
    let mut changed_samples = 0_usize;
    let mut strong_samples = 0_usize;
    let mut group_changes = vec![0_usize; groups.len()];
    let mut vertical_band_changes = [0_usize; CHANGE_VERTICAL_BANDS];
    let mut vertical_band_samples = [0_usize; CHANGE_VERTICAL_BANDS];
    for y in (0..height).step_by(row_step) {
        let vertical_band = (y * CHANGE_VERTICAL_BANDS / height).min(CHANGE_VERTICAL_BANDS - 1);
        vertical_band_samples[vertical_band] += 1;
        let mut row_changes = 0_usize;
        for &(x, group) in &positions {
            let offset = (y * width + x) * 4;
            let difference = luma(&left.rgba[offset..offset + 3])
                .abs_diff(luma(&right.rgba[offset..offset + 3]));
            if difference >= 10 {
                changed_samples += 1;
                row_changes += 1;
                group_changes[group] += 1;
            }
            if difference >= 28 {
                strong_samples += 1;
            }
        }
        if row_changes >= changed_per_row {
            changed_rows += 1;
            vertical_band_changes[vertical_band] += 1;
        }
        sampled_rows += 1;
    }
    let mut changed_groups = 0_usize;
    for (group, changes) in group_changes.into_iter().enumerate() {
        let group_samples = sampled_rows.saturating_mul(groups[group].len());
        if group_samples > 0 && changes.saturating_mul(100) >= group_samples.saturating_mul(2) {
            changed_groups += 1;
        }
    }
    Some(SparseFrameChange {
        changed_rows,
        sampled_rows,
        changed_samples,
        strong_samples,
        total_samples: sampled_rows * positions.len(),
        changed_groups,
        sampled_groups: groups.len(),
        changed_vertical_bands: vertical_band_changes
            .into_iter()
            .zip(vertical_band_samples)
            .filter(|(changed, sampled)| {
                *sampled > 0 && *changed >= 2 && *changed * 100 >= *sampled * 4
            })
            .count(),
    })
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualScrollEvidence {
    SignificantVisualChange,
}

#[cfg(test)]
fn manual_scroll_evidence(left: &Frame, right: &Frame) -> Option<ManualScrollEvidence> {
    // A verified vertical displacement is sufficient evidence of scrolling.
    // Whole-frame difference thresholds reject small scrolls in sparse or
    // mostly uniform windows before the stronger overlap matcher can run.
    find_vertical_overlap(left, right)?;
    Some(ManualScrollEvidence::SignificantVisualChange)
}

#[derive(Debug, Clone, Copy)]
struct OverlapMatch {
    displacement: u32,
    confidence: f32,
    static_bottom: u32,
}

#[derive(Debug, Default)]
struct FixedBottomTracker {
    candidate: Option<(u32, u8)>,
}

impl FixedBottomTracker {
    fn observe(&mut self, height: u32) -> Option<u32> {
        if height < 6 {
            self.candidate = None;
            return None;
        }
        let (candidate, confirmations) = match self.candidate {
            Some((candidate, confirmations)) if candidate.abs_diff(height) <= 3 => {
                (candidate.min(height), confirmations.saturating_add(1))
            }
            _ => (height, 1),
        };
        self.candidate = Some((candidate, confirmations));
        (confirmations >= 2).then_some(candidate)
    }
}

#[derive(Debug, Clone, Copy)]
struct ScoredDisplacement {
    displacement: usize,
    score: f32,
}

fn coarse_displacement_candidates(minimum: usize, maximum: usize) -> (Vec<usize>, usize) {
    if maximum < minimum {
        return (Vec::new(), 1);
    }
    let span = maximum - minimum + 1;
    let step = span.div_ceil(MATCH_MAX_COARSE_CANDIDATES).max(1);
    let mut candidates = (minimum..=maximum).step_by(step).collect::<Vec<_>>();
    if candidates.last().copied() != Some(maximum) {
        if candidates.len() >= MATCH_MAX_COARSE_CANDIDATES {
            if let Some(last) = candidates.last_mut() {
                *last = maximum;
            }
        } else {
            candidates.push(maximum);
        }
    }
    (candidates, step)
}

fn select_refine_seeds(scores: &[ScoredDisplacement], coarse_step: usize) -> Vec<usize> {
    let mut ranked = scores.to_vec();
    ranked.sort_by(|left, right| left.score.total_cmp(&right.score));
    let minimum_separation = coarse_step.saturating_mul(2).max(2);
    let mut seeds = Vec::with_capacity(MATCH_MAX_REFINE_SEEDS);
    for candidate in ranked {
        if seeds
            .iter()
            .all(|seed: &usize| seed.abs_diff(candidate.displacement) > minimum_separation)
        {
            seeds.push(candidate.displacement);
            if seeds.len() >= MATCH_MAX_REFINE_SEEDS {
                break;
            }
        }
    }
    seeds
}

fn refinement_candidates(
    seeds: &[usize],
    minimum: usize,
    maximum: usize,
    coarse_step: usize,
) -> Vec<usize> {
    let radius = coarse_step.max(1);
    let mut candidates = Vec::with_capacity(seeds.len() * (radius * 2 + 1));
    for seed in seeds {
        let start = seed.saturating_sub(radius).max(minimum);
        let end = seed.saturating_add(radius).min(maximum);
        candidates.extend(start..=end);
    }
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn alignment_score_bounded(
    previous: &GrayStrips,
    current: &GrayStrips,
    displacement: usize,
    maximum_rows: usize,
) -> Option<f32> {
    if displacement >= previous.height || maximum_rows == 0 {
        return None;
    }
    let overlap = previous.height - displacement;
    let trim = (overlap / 12).min(48);
    let sampled_span = overlap.saturating_sub(trim.saturating_mul(2));
    let row_step = sampled_span.div_ceil(maximum_rows).max(1);
    alignment_score(previous, current, displacement, row_step)
}

fn alignment_detail_score_bounded(
    previous: &GrayStrips,
    current: &GrayStrips,
    displacement: usize,
    maximum_rows: usize,
) -> Option<f32> {
    if previous.height != current.height
        || previous.samples_per_row != current.samples_per_row
        || previous.samples_per_row == 0
        || displacement >= previous.height
        || maximum_rows == 0
    {
        return None;
    }
    let overlap = previous.height - displacement;
    if overlap < 8 {
        return None;
    }
    let trim = (overlap / 12).min(48);
    let start = trim;
    let end = overlap.saturating_sub(trim);
    if end <= start + 4 {
        return None;
    }
    let row_step = (end - start).div_ceil(maximum_rows).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for y in (start..end).step_by(row_step) {
        let previous_row = (y + displacement) * previous.samples_per_row;
        let current_row = y * current.samples_per_row;
        for sample in 0..previous.samples_per_row {
            difference += u64::from(
                previous.values[previous_row + sample]
                    .abs_diff(current.values[current_row + sample]),
            );
            samples += 1;
        }
    }
    if samples == 0 {
        None
    } else {
        Some(difference as f32 / samples as f32)
    }
}

#[derive(Debug, Clone, Copy)]
struct MotionSupport {
    textured_blocks: usize,
    support_blocks: usize,
    horizontal_bands: usize,
    vertical_bands: usize,
}

fn rgb_difference(left: &[u8], right: &[u8]) -> u32 {
    (0..3)
        .map(|channel| u32::from(left[channel].abs_diff(right[channel])))
        .sum()
}

fn block_motion_support(
    previous: &Frame,
    current: &Frame,
    displacement: usize,
) -> Option<MotionSupport> {
    if !frames_have_matching_geometry(previous, current)
        || displacement == 0
        || displacement >= previous.height as usize
    {
        return None;
    }
    let width = previous.width as usize;
    let overlap = previous.height as usize - displacement;
    let mut textured_blocks = 0_usize;
    let mut support_blocks = 0_usize;
    let mut horizontal_support = [false; MATCH_GRID_COLUMNS];
    let mut vertical_support = [false; MATCH_GRID_ROWS];

    for block_y in 0..MATCH_GRID_ROWS {
        let top = block_y * overlap / MATCH_GRID_ROWS;
        let bottom = ((block_y + 1) * overlap / MATCH_GRID_ROWS).max(top + 1);
        for block_x in 0..MATCH_GRID_COLUMNS {
            let left = block_x * width / MATCH_GRID_COLUMNS;
            let right = ((block_x + 1) * width / MATCH_GRID_COLUMNS).max(left + 1);
            let mut previous_sum = [0_f64; 3];
            let mut current_sum = [0_f64; 3];
            let mut previous_square = [0_f64; 3];
            let mut current_square = [0_f64; 3];
            let mut matched_difference = 0_u64;
            let mut stationary_difference = 0_u64;
            let mut pixels = 0_u64;
            for sample_y in 0..MATCH_SAMPLES_PER_BLOCK_AXIS {
                let y = top
                    + ((sample_y * 2 + 1) * (bottom - top) / (MATCH_SAMPLES_PER_BLOCK_AXIS * 2))
                        .min(bottom - top - 1);
                for sample_x in 0..MATCH_SAMPLES_PER_BLOCK_AXIS {
                    let x = left
                        + ((sample_x * 2 + 1) * (right - left)
                            / (MATCH_SAMPLES_PER_BLOCK_AXIS * 2))
                            .min(right - left - 1);
                    let matched_offset = ((y + displacement) * width + x) * 4;
                    let stationary_offset = (y * width + x) * 4;
                    let current_offset = stationary_offset;
                    matched_difference += u64::from(rgb_difference(
                        &previous.rgba[matched_offset..matched_offset + 3],
                        &current.rgba[current_offset..current_offset + 3],
                    ));
                    stationary_difference += u64::from(rgb_difference(
                        &previous.rgba[stationary_offset..stationary_offset + 3],
                        &current.rgba[current_offset..current_offset + 3],
                    ));
                    for channel in 0..3 {
                        let left_value = f64::from(previous.rgba[matched_offset + channel]);
                        let right_value = f64::from(current.rgba[current_offset + channel]);
                        previous_sum[channel] += left_value;
                        current_sum[channel] += right_value;
                        previous_square[channel] += left_value * left_value;
                        current_square[channel] += right_value * right_value;
                    }
                    pixels += 1;
                }
            }
            if pixels == 0 {
                continue;
            }
            let pixel_count = pixels as f64;
            let mut variance = 0_f64;
            for channel in 0..3 {
                variance += (previous_square[channel] / pixel_count
                    - (previous_sum[channel] / pixel_count).powi(2))
                .max(0.0);
                variance += (current_square[channel] / pixel_count
                    - (current_sum[channel] / pixel_count).powi(2))
                .max(0.0);
            }
            let texture = (variance / 6.0).sqrt();
            if texture < 1.5 {
                continue;
            }
            textured_blocks += 1;
            let channels = pixels * 3;
            let matched = matched_difference as f64 / channels as f64;
            let stationary = stationary_difference as f64 / channels as f64;
            let required_improvement = (stationary * 0.06).max(0.35);
            if matched <= 38.0 && matched + required_improvement < stationary {
                support_blocks += 1;
                horizontal_support[block_x] = true;
                vertical_support[block_y] = true;
            }
        }
    }

    Some(MotionSupport {
        textured_blocks,
        support_blocks,
        horizontal_bands: horizontal_support
            .into_iter()
            .filter(|value| *value)
            .count(),
        vertical_bands: vertical_support.into_iter().filter(|value| *value).count(),
    })
}

#[derive(Debug)]
struct BlockMatchFrame<'a> {
    width: usize,
    height: usize,
    scale: usize,
    frame: &'a Frame,
}

impl<'a> BlockMatchFrame<'a> {
    fn from_frame(frame: &'a Frame, scale: usize) -> Self {
        let scale = scale.max(1);
        let source_width = frame.width as usize;
        let source_height = frame.height as usize;
        let width = (source_width / scale).max(1);
        let height = (source_height / scale).max(1);
        Self {
            width,
            height,
            scale,
            frame,
        }
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let source_x = (x * self.scale).min(self.frame.width as usize - 1);
        let source_y = (y * self.scale).min(self.frame.height as usize - 1);
        let offset = (source_y * self.frame.width as usize + source_x) * 4;
        [
            self.frame.rgba[offset],
            self.frame.rgba[offset + 1],
            self.frame.rgba[offset + 2],
        ]
    }

    fn difference(&self, x: usize, y: usize, other: &Self, other_y: usize) -> u32 {
        self.pixel(x, y)
            .into_iter()
            .zip(other.pixel(x, other_y))
            .map(|(left, right)| u32::from(left.abs_diff(right)))
            .sum::<u32>()
            / 3
    }
}

#[derive(Debug, Clone, Copy)]
struct BlockMotionCandidate {
    displacement: usize,
    score: f64,
    eligible_blocks: usize,
    support_blocks: usize,
    horizontal_bands: usize,
    vertical_bands: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct StaticMatchMargins {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
}

fn stationary_row_difference(
    previous: &BlockMatchFrame<'_>,
    current: &BlockMatchFrame<'_>,
    y: usize,
) -> Option<f64> {
    if previous.width != current.width || previous.height != current.height || y >= previous.height
    {
        return None;
    }
    let step = (previous.width / 240).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for x in (0..previous.width).step_by(step) {
        difference += u64::from(previous.difference(x, y, current, y));
        samples += 1;
    }
    (samples > 0).then_some(difference as f64 / samples as f64)
}

fn stationary_column_difference(
    previous: &BlockMatchFrame<'_>,
    current: &BlockMatchFrame<'_>,
    x: usize,
    top: usize,
    bottom: usize,
) -> Option<f64> {
    if previous.width != current.width
        || previous.height != current.height
        || x >= previous.width
        || top + bottom >= previous.height
    {
        return None;
    }
    let height = previous.height - top - bottom;
    let step = (height / 200).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for y in (top..previous.height - bottom).step_by(step) {
        difference += u64::from(previous.difference(x, y, current, y));
        samples += 1;
    }
    (samples > 0).then_some(difference as f64 / samples as f64)
}

fn detect_static_match_margins(
    previous: &BlockMatchFrame<'_>,
    current: &BlockMatchFrame<'_>,
) -> StaticMatchMargins {
    if previous.width != current.width || previous.height != current.height {
        return StaticMatchMargins::default();
    }
    let maximum = previous.height / 5;
    let mut top = 0_usize;
    while top < maximum
        && stationary_row_difference(previous, current, top).is_some_and(|score| score <= 4.0)
    {
        top += 1;
    }
    let mut bottom = 0_usize;
    while bottom < maximum
        && top + bottom + MATCH_GRID_ROWS * 2 < previous.height
        && stationary_row_difference(previous, current, previous.height - 1 - bottom)
            .is_some_and(|score| score <= 4.0)
    {
        bottom += 1;
    }
    top = if top >= 3 { top } else { 0 };
    bottom = if bottom >= 3 { bottom } else { 0 };

    let maximum_horizontal = previous.width / 3;
    let mut left = 0_usize;
    while left < maximum_horizontal
        && stationary_column_difference(previous, current, left, top, bottom)
            .is_some_and(|score| score <= 4.0)
    {
        left += 1;
    }
    let mut right = 0_usize;
    while right < maximum_horizontal
        && left + right + MATCH_GRID_COLUMNS * 2 < previous.width
        && stationary_column_difference(previous, current, previous.width - 1 - right, top, bottom)
            .is_some_and(|score| score <= 4.0)
    {
        right += 1;
    }
    StaticMatchMargins {
        top,
        bottom,
        left: if left >= 3 { left } else { 0 },
        right: if right >= 3 { right } else { 0 },
    }
}

fn score_block_motion_candidate(
    previous: &BlockMatchFrame<'_>,
    current: &BlockMatchFrame<'_>,
    displacement: usize,
    sample_step: usize,
    margins: StaticMatchMargins,
) -> Option<BlockMotionCandidate> {
    if previous.width != current.width || previous.height != current.height {
        return None;
    }
    let content_height = previous
        .height
        .checked_sub(margins.top.saturating_add(margins.bottom))?;
    if displacement >= content_height {
        return None;
    }
    let content_width = previous
        .width
        .checked_sub(margins.left.saturating_add(margins.right))?;
    let overlap_height = content_height - displacement;
    if overlap_height * 100 < content_height * MATCH_MIN_OVERLAP_PERCENT
        || content_width < MATCH_GRID_COLUMNS * 2
        || overlap_height < MATCH_GRID_ROWS * 2
    {
        return None;
    }

    let block_width = (content_width / MATCH_GRID_COLUMNS).max(2);
    let block_height = (overlap_height / MATCH_GRID_ROWS).max(2);
    let mut scores = Vec::with_capacity(MATCH_GRID_COLUMNS * MATCH_GRID_ROWS);
    let mut support_blocks = 0_usize;
    let mut horizontal_support = [false; MATCH_GRID_COLUMNS];
    let mut vertical_support = [false; MATCH_GRID_ROWS];

    for block_y in 0..MATCH_GRID_ROWS {
        let top = block_y * block_height;
        let bottom = if block_y + 1 == MATCH_GRID_ROWS {
            overlap_height
        } else {
            (top + block_height).min(overlap_height)
        };
        for block_x in 0..MATCH_GRID_COLUMNS {
            let left = block_x * block_width;
            let right = if block_x + 1 == MATCH_GRID_COLUMNS {
                content_width
            } else {
                (left + block_width).min(content_width)
            };
            let x_step = sample_step.max(((right - left) / 6).max(1));
            let y_step = sample_step.max(((bottom - top) / 5).max(1));
            let mut matched_difference = 0_u64;
            let mut stationary_difference = 0_u64;
            let mut texture = 0_u64;
            let mut samples = 0_u64;

            for y in (top..bottom).step_by(y_step) {
                for x in (left..right).step_by(x_step) {
                    let x = margins.left + x;
                    let current_y = margins.top + y;
                    let previous_y = current_y + displacement;
                    matched_difference +=
                        u64::from(previous.difference(x, previous_y, current, current_y));
                    stationary_difference +=
                        u64::from(previous.difference(x, current_y, current, current_y));
                    if x + 1 < previous.width {
                        let here = previous.pixel(x, previous_y);
                        let beside = previous.pixel(x + 1, previous_y);
                        texture += u64::from(
                            here.into_iter()
                                .zip(beside)
                                .map(|(left, right)| u32::from(left.abs_diff(right)))
                                .sum::<u32>()
                                / 3,
                        );
                    }
                    if previous_y + 1 < previous.height {
                        texture +=
                            u64::from(previous.difference(x, previous_y, previous, previous_y + 1));
                    }
                    samples += 1;
                }
            }
            if samples < 4 || texture as f64 / (samples as f64) < 1.5 {
                continue;
            }
            let matched = matched_difference as f64 / samples as f64;
            let stationary = stationary_difference as f64 / samples as f64;

            // A toolbar, title row, sidebar or scrollbar remains better
            // aligned at zero displacement. Exclude it from candidate ranking
            // instead of allowing fixed chrome to outvote the scrolling body.
            if displacement > 0 && stationary <= 1.25 && matched > stationary + 0.35 {
                continue;
            }
            scores.push(matched);
            if displacement > 0 {
                let required_improvement = (stationary * 0.06).max(0.35);
                if matched + required_improvement < stationary {
                    support_blocks += 1;
                    horizontal_support[block_x] = true;
                    vertical_support[block_y] = true;
                }
            }
        }
    }

    if scores.len() < 4 {
        return None;
    }
    scores.sort_by(f64::total_cmp);
    let retained = scores.len().saturating_mul(92).div_ceil(100).max(4);
    let score = scores.iter().take(retained).sum::<f64>() / retained as f64;
    Some(BlockMotionCandidate {
        displacement,
        score,
        eligible_blocks: scores.len(),
        support_blocks,
        horizontal_bands: horizontal_support
            .into_iter()
            .filter(|supported| *supported)
            .count(),
        vertical_bands: vertical_support
            .into_iter()
            .filter(|supported| *supported)
            .count(),
    })
}

fn reliable_vertical_overlap(previous: &Frame, current: &Frame) -> Option<OverlapMatch> {
    if !frames_have_matching_geometry(previous, current) || previous.height < 24 {
        return None;
    }
    let full_height = previous.height as usize;
    let full_width = previous.width as usize;
    let scale = full_width.max(full_height).div_ceil(220).max(1);
    let previous_coarse = BlockMatchFrame::from_frame(previous, scale);
    let current_coarse = BlockMatchFrame::from_frame(current, scale);
    let coarse_margins = detect_static_match_margins(&previous_coarse, &current_coarse);
    let stationary =
        score_block_motion_candidate(&previous_coarse, &current_coarse, 0, 1, coarse_margins)?;
    if stationary.score <= 1.25 {
        return None;
    }

    let previous_full = BlockMatchFrame::from_frame(previous, 1);
    let current_full = BlockMatchFrame::from_frame(current, 1);
    let full_margins = detect_static_match_margins(&previous_full, &current_full);
    let full_content_height =
        full_height.saturating_sub(full_margins.top.saturating_add(full_margins.bottom));
    let maximum_full = full_content_height.saturating_mul(100 - MATCH_MIN_OVERLAP_PERCENT) / 100;
    let coarse_content_height = previous_coarse
        .height
        .saturating_sub(coarse_margins.top.saturating_add(coarse_margins.bottom));
    let maximum_coarse =
        coarse_content_height.saturating_mul(100 - MATCH_MIN_OVERLAP_PERCENT) / 100;
    let mut coarse = (1..=maximum_coarse)
        .filter_map(|displacement| {
            score_block_motion_candidate(
                &previous_coarse,
                &current_coarse,
                displacement,
                1,
                coarse_margins,
            )
        })
        .collect::<Vec<_>>();
    coarse.sort_by(|left, right| left.score.total_cmp(&right.score));
    if coarse.is_empty() {
        return None;
    }
    let radius = scale.saturating_mul(4).clamp(4, MOTION_FINE_RADIUS_MAX);
    let mut fine_displacements = coarse
        .iter()
        .take(8)
        .flat_map(|candidate| {
            let estimate = candidate.displacement.saturating_mul(scale);
            let start = estimate.saturating_sub(radius).max(2);
            let end = estimate.saturating_add(radius).min(maximum_full);
            start..=end
        })
        .collect::<Vec<_>>();
    fine_displacements.sort_unstable();
    fine_displacements.dedup();
    let mut fine = fine_displacements
        .into_iter()
        .filter_map(|displacement| {
            score_block_motion_candidate(
                &previous_full,
                &current_full,
                displacement,
                (scale / 2).max(1),
                full_margins,
            )
        })
        .collect::<Vec<_>>();
    fine.sort_by(|left, right| left.score.total_cmp(&right.score));
    let best = *fine.first()?;
    let second = fine
        .iter()
        .find(|candidate| candidate.displacement.abs_diff(best.displacement) > 3)
        .copied();
    let confidence_gap = second
        .map(|candidate| {
            ((candidate.score - best.score) / candidate.score.max(1.0)).clamp(0.0, 1.0)
        })
        .unwrap_or(1.0);
    let distributed_support = best.vertical_bands >= MATCH_MIN_VERTICAL_BANDS
        || (best.score <= 1.5
            && best.support_blocks >= 8
            && best.horizontal_bands >= 4
            && best.vertical_bands >= 2);

    if best.score > 38.0
        || best.eligible_blocks < MATCH_MIN_TEXTURED_BLOCKS
        || best.support_blocks < MOTION_MIN_SUPPORT_BLOCKS
        || best.horizontal_bands < MATCH_MIN_HORIZONTAL_BANDS
        || !distributed_support
        || (best.score > 8.0 && confidence_gap < 0.025)
    {
        return None;
    }

    let quality = (1.0 - best.score / 38.0).clamp(0.0, 1.0);
    let support_ratio = (best.support_blocks as f64 / best.eligible_blocks as f64).clamp(0.0, 1.0);
    Some(OverlapMatch {
        displacement: best.displacement as u32,
        confidence: (quality * 0.65 + confidence_gap * 0.25 + support_ratio * 0.1).clamp(0.0, 1.0)
            as f32,
        static_bottom: 0,
    })
}

fn phase_vertical_overlap(previous: &Frame, current: &Frame) -> Option<OverlapMatch> {
    if !frames_have_matching_geometry(previous, current) || previous.height < 24 {
        return None;
    }
    let phase = phase_offset_rgba(
        &previous.rgba,
        &current.rgba,
        previous.width as usize,
        previous.height as usize,
    )?;
    if phase.psr < 5.0 {
        return None;
    }
    let horizontal_tolerance =
        (phase.factor as i32 * 2 + 2).max((previous.width as i32 / 100).max(3));
    if phase.dx.abs() > horizontal_tolerance {
        return None;
    }
    let estimate = phase.dy.checked_neg()?;
    if estimate < 2 {
        return None;
    }

    let previous_view = BlockMatchFrame::from_frame(previous, 1);
    let current_view = BlockMatchFrame::from_frame(current, 1);
    let margins = detect_static_match_margins(&previous_view, &current_view);
    let content_height = previous_view
        .height
        .checked_sub(margins.top.saturating_add(margins.bottom))?;
    let maximum = content_height.saturating_mul(100 - MATCH_MIN_OVERLAP_PERCENT) / 100;
    let radius = phase.factor as usize + 3;
    let start = (estimate as usize).saturating_sub(radius).max(2);
    let end = (estimate as usize).saturating_add(radius).min(maximum);
    if end < start {
        return None;
    }
    let sample_step = (phase.factor as usize / 2).max(1);
    let best = (start..=end)
        .filter_map(|displacement| {
            score_block_motion_candidate(
                &previous_view,
                &current_view,
                displacement,
                sample_step,
                margins,
            )
        })
        .filter(|candidate| {
            candidate.score <= 38.0
                && candidate.eligible_blocks >= MATCH_MIN_TEXTURED_BLOCKS.saturating_sub(2)
                && candidate.support_blocks >= MOTION_MIN_SUPPORT_BLOCKS
                && candidate.horizontal_bands >= MATCH_MIN_HORIZONTAL_BANDS
                && candidate.vertical_bands >= 2
        })
        .min_by(|left, right| left.score.total_cmp(&right.score))?;
    let quality = (1.0 - best.score / 38.0).clamp(0.0, 1.0);
    let phase_quality = ((f64::from(phase.psr) - 5.0) / 15.0).clamp(0.0, 1.0);
    let support = (best.support_blocks as f64 / best.eligible_blocks.max(1) as f64).clamp(0.0, 1.0);
    Some(OverlapMatch {
        displacement: best.displacement as u32,
        confidence: (quality * 0.55 + phase_quality * 0.25 + support * 0.2) as f32,
        static_bottom: 0,
    })
}

fn row_transition_score(frame: &Frame, boundary_y: usize) -> Option<f64> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    if boundary_y == 0 || boundary_y >= height {
        return None;
    }
    let step = (width / 240).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for x in (0..width).step_by(step) {
        let above = ((boundary_y - 1) * width + x) * 4;
        let below = (boundary_y * width + x) * 4;
        difference += u64::from(rgb_difference(
            &frame.rgba[above..above + 3],
            &frame.rgba[below..below + 3],
        ));
        samples += 3;
    }
    (samples > 0).then_some(difference as f64 / samples as f64)
}

fn refine_static_bottom_boundary(frame: &Frame, maximum: usize) -> usize {
    let height = frame.height as usize;
    let maximum = maximum.min(height.saturating_sub(1));
    let best = (6..=maximum)
        .filter_map(|bottom| {
            row_transition_score(frame, height - bottom).map(|score| (bottom, score))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1));
    best.filter(|(_, score)| *score >= 3.0)
        .map(|(bottom, _)| bottom)
        .unwrap_or(0)
}

fn attach_static_bottom(
    previous: &Frame,
    current: &Frame,
    mut matched: OverlapMatch,
) -> OverlapMatch {
    let previous_view = BlockMatchFrame::from_frame(previous, 1);
    let current_view = BlockMatchFrame::from_frame(current, 1);
    let margins = detect_static_match_margins(&previous_view, &current_view);
    if margins.bottom >= 6 && margins.bottom <= previous.height as usize / 5 {
        matched.static_bottom = refine_static_bottom_boundary(current, margins.bottom) as u32;
    }
    matched
}

fn find_vertical_overlap(previous: &Frame, current: &Frame) -> Option<OverlapMatch> {
    let block_match = reliable_vertical_overlap(previous, current);
    if let Some(matched) = block_match.filter(|matched| matched.confidence >= 0.82) {
        return Some(attach_static_bottom(previous, current, matched));
    }
    let narrow_match = find_vertical_overlap_legacy(previous, current).and_then(|matched| {
        let support = block_motion_support(previous, current, matched.displacement as usize)?;
        let broad_support = support.textured_blocks >= MATCH_MIN_TEXTURED_BLOCKS
            && support.support_blocks >= MOTION_MIN_SUPPORT_BLOCKS
            && support.horizontal_bands >= MATCH_MIN_HORIZONTAL_BANDS
            && support.vertical_bands >= MATCH_MIN_VERTICAL_BANDS;
        let narrow_high_confidence_support = matched.confidence >= 0.985
            && support.textured_blocks >= MATCH_MIN_TEXTURED_BLOCKS
            && support.support_blocks >= 1
            && support.horizontal_bands >= 1
            && support.vertical_bands >= 1
            && sparse_frame_change(previous, current)
                .is_some_and(SparseFrameChange::is_distributed_change_candidate);
        (broad_support || narrow_high_confidence_support).then_some(matched)
    });
    let verified = match (block_match, narrow_match) {
        (Some(block), Some(narrow)) if block.displacement != narrow.displacement => {
            if block.confidence >= narrow.confidence {
                Some(block)
            } else {
                Some(narrow)
            }
        }
        (Some(block), _) => Some(block),
        (None, narrow) => narrow,
    };
    verified
        .or_else(|| phase_vertical_overlap(previous, current))
        .map(|matched| attach_static_bottom(previous, current, matched))
}

fn maximum_legacy_displacement(height: usize) -> usize {
    height.saturating_mul(100 - MATCH_MIN_OVERLAP_PERCENT) / 100
}

fn find_vertical_overlap_legacy(previous: &Frame, current: &Frame) -> Option<OverlapMatch> {
    if !frames_have_matching_geometry(previous, current) || previous.height < 24 {
        return None;
    }
    let selection = select_match_columns(previous, current);
    if selection.primary.is_empty() {
        return None;
    }
    let previous_primary = GrayStrips::from_frame(previous, &selection.primary);
    let current_primary = GrayStrips::from_frame(current, &selection.primary);
    if previous_primary.texture.min(current_primary.texture) < 1.1 {
        return None;
    }
    let verification = (!selection.verification.is_empty()).then(|| {
        (
            GrayStrips::from_frame(previous, &selection.verification),
            GrayStrips::from_frame(current, &selection.verification),
        )
    });
    let height = previous_primary.height;
    let minimum = 2_usize;
    let maximum = maximum_legacy_displacement(height);
    if maximum <= minimum {
        return None;
    }

    let (coarse_candidates, coarse_step) = coarse_displacement_candidates(minimum, maximum);
    let coarse_scores = coarse_candidates
        .into_iter()
        .filter_map(|displacement| {
            alignment_score_bounded(
                &previous_primary,
                &current_primary,
                displacement,
                MATCH_MAX_COARSE_ROWS,
            )
            .map(|score| ScoredDisplacement {
                displacement,
                score,
            })
        })
        .collect::<Vec<_>>();
    if coarse_scores.is_empty() {
        return None;
    }
    let seeds = select_refine_seeds(&coarse_scores, coarse_step);
    let fine_candidates = refinement_candidates(&seeds, minimum, maximum, coarse_step);
    let candidates = fine_candidates
        .into_iter()
        .filter_map(|displacement| {
            alignment_score_bounded(
                &previous_primary,
                &current_primary,
                displacement,
                MATCH_MAX_FINE_ROWS,
            )
            .map(|score| ScoredDisplacement {
                displacement,
                score,
            })
        })
        .collect::<Vec<_>>();
    let robust_best = candidates
        .iter()
        .min_by(|left, right| left.score.total_cmp(&right.score))?;
    let exclusion = (height / 100).clamp(4, 16);
    let detail_tolerance = (robust_best.score * 0.05).max(0.25);
    let best = candidates
        .iter()
        .filter(|candidate| {
            candidate.displacement.abs_diff(robust_best.displacement) <= exclusion
                && candidate.score <= robust_best.score + detail_tolerance
        })
        .filter_map(|candidate| {
            alignment_detail_score_bounded(
                &previous_primary,
                &current_primary,
                candidate.displacement,
                MATCH_MAX_FINE_ROWS,
            )
            .map(|detail_score| (candidate, detail_score))
        })
        .min_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.score.total_cmp(&right.0.score))
        })
        .map(|(candidate, _)| candidate)
        .unwrap_or(robust_best);
    let best_displacement = best.displacement;
    let best_score = best.score;
    if !best_score.is_finite() {
        return None;
    }

    let second_score = candidates
        .iter()
        .filter(|candidate| candidate.displacement.abs_diff(best_displacement) > exclusion)
        .map(|candidate| candidate.score)
        .fold(f32::INFINITY, f32::min);
    let gap = second_score - best_score;
    let required_gap = (best_score * 0.10).max(0.55);
    if best_score > 9.5 || !second_score.is_finite() || gap < required_gap {
        return None;
    }

    if let Some((previous_verification, current_verification)) = verification {
        if previous_verification
            .texture
            .min(current_verification.texture)
            >= 1.1
        {
            let matched_score = alignment_score_bounded(
                &previous_verification,
                &current_verification,
                best_displacement,
                MATCH_MAX_FINE_ROWS,
            )?;
            let stationary_score = alignment_score_bounded(
                &previous_verification,
                &current_verification,
                0,
                MATCH_MAX_FINE_ROWS,
            )?;
            let required_improvement = (stationary_score * 0.05).max(0.35);
            if matched_score > 11.5 || matched_score + required_improvement >= stationary_score {
                return None;
            }
        }
    }

    let quality = (1.0 - best_score / 14.0).clamp(0.0, 1.0);
    let separation = (gap / (best_score + gap).max(0.1)).clamp(0.0, 1.0);
    Some(OverlapMatch {
        displacement: best_displacement as u32,
        confidence: (quality * 0.72 + separation * 0.28).clamp(0.0, 1.0),
        static_bottom: 0,
    })
}

fn alignment_score(
    previous: &GrayStrips,
    current: &GrayStrips,
    displacement: usize,
    row_step: usize,
) -> Option<f32> {
    if previous.height != current.height
        || previous.samples_per_row != current.samples_per_row
        || displacement >= previous.height
    {
        return None;
    }
    let overlap = previous.height - displacement;
    if overlap < 8 {
        return None;
    }
    let trim = (overlap / 12).min(48);
    let start = trim;
    let end = overlap.saturating_sub(trim);
    if end <= start + 4 {
        return None;
    }

    // Keep a histogram of per-row errors and average the best 75%. Fixed
    // headers, carets and animated blocks therefore cannot dominate a match.
    let mut histogram = [0_u32; 65];
    let mut rows = 0_u32;
    for y in (start..end).step_by(row_step.max(1)) {
        let previous_row = (y + displacement) * previous.samples_per_row;
        let current_row = y * current.samples_per_row;
        let mut row_error = 0_u32;
        for sample in 0..previous.samples_per_row {
            row_error += u32::from(
                previous.values[previous_row + sample]
                    .abs_diff(current.values[current_row + sample]),
            );
        }
        let mean = (row_error / previous.samples_per_row as u32).min(64) as usize;
        histogram[mean] += 1;
        rows += 1;
    }
    if rows < 4 {
        return None;
    }
    let keep = (rows * 3 / 4).max(1);
    let mut retained = 0_u32;
    let mut weighted = 0_u64;
    for (error, count) in histogram.into_iter().enumerate() {
        let take = count.min(keep - retained);
        weighted += error as u64 * u64::from(take);
        retained += take;
        if retained == keep {
            break;
        }
    }
    Some(weighted as f32 / retained as f32)
}

fn luma(rgb: &[u8]) -> u8 {
    ((u32::from(rgb[0]) * 77 + u32::from(rgb[1]) * 150 + u32::from(rgb[2]) * 29) >> 8) as u8
}

fn capture_settled_roi(bounds: PhysicalRect) -> AppResult<Frame> {
    std::thread::sleep(Duration::from_millis(70));
    let mut previous = capture_roi(bounds)?;
    let mut stable_samples = 0_u8;
    for _ in 0..SETTLE_MAX_SAMPLES {
        std::thread::sleep(SETTLE_SAMPLE_INTERVAL);
        let current = capture_roi(bounds)?;
        if sampled_frame_difference(&previous, &current) <= 1.25 {
            stable_samples = stable_samples.saturating_add(1);
        } else {
            stable_samples = 0;
        }
        previous = current;
        if stable_samples >= 2 {
            break;
        }
    }
    Ok(previous)
}

fn copy_frame_region(
    destination: &mut Frame,
    source: &Frame,
    region: PhysicalRect,
) -> AppResult<()> {
    destination.validate()?;
    source.validate()?;
    if destination.width != source.width || destination.height != source.height {
        return Err(AppError::invalid("长截图控制条遮罩帧尺寸不一致"));
    }
    let x =
        usize::try_from(region.x).map_err(|_| AppError::invalid("长截图控制条遮罩横坐标无效"))?;
    let y =
        usize::try_from(region.y).map_err(|_| AppError::invalid("长截图控制条遮罩纵坐标无效"))?;
    let width = region.width as usize;
    let height = region.height as usize;
    let right = x
        .checked_add(width)
        .filter(|right| *right <= destination.width as usize)
        .ok_or_else(|| AppError::invalid("长截图控制条遮罩宽度越界"))?;
    let bottom = y
        .checked_add(height)
        .filter(|bottom| *bottom <= destination.height as usize)
        .ok_or_else(|| AppError::invalid("长截图控制条遮罩高度越界"))?;
    let row_bytes = destination.width as usize * 4;
    let copy_bytes = (right - x) * 4;
    for row in y..bottom {
        let start = row * row_bytes + x * 4;
        let end = start + copy_bytes;
        destination.rgba[start..end].copy_from_slice(&source.rgba[start..end]);
    }
    Ok(())
}

fn sampled_frame_difference(left: &Frame, right: &Frame) -> f32 {
    if left.width != right.width
        || left.height != right.height
        || left.rgba.len() != right.rgba.len()
    {
        return f32::INFINITY;
    }
    let width = left.width as usize;
    let height = left.height as usize;
    let step_x = (width / 64).max(1);
    let step_y = (height / 64).max(1);
    let mut difference = 0_u64;
    let mut samples = 0_u64;
    for y in (0..height).step_by(step_y) {
        for x in (0..width).step_by(step_x) {
            let offset = (y * width + x) * 4;
            difference += u64::from(
                luma(&left.rgba[offset..offset + 3])
                    .abs_diff(luma(&right.rgba[offset..offset + 3])),
            );
            samples += 1;
        }
    }
    if samples == 0 {
        f32::INFINITY
    } else {
        difference as f32 / samples as f32
    }
}

fn crop_rows(frame: &Frame, start: u32, height: u32) -> AppResult<Frame> {
    frame.validate()?;
    let end = start
        .checked_add(height)
        .filter(|end| *end <= frame.height)
        .ok_or_else(|| AppError::invalid("长截图片段裁剪范围无效"))?;
    let row_bytes = frame.width as usize * 4;
    let byte_start = start as usize * row_bytes;
    let byte_end = end as usize * row_bytes;
    Ok(Frame {
        width: frame.width,
        height,
        rgba: frame.rgba[byte_start..byte_end].to_vec(),
    })
}

fn compose_tile(
    directory: &Path,
    manifest: &LongCaptureManifest,
    y: u32,
    height: u32,
) -> AppResult<Frame> {
    compose_region(directory, manifest, y, height, MAX_TILE_PIXELS)
}

fn compose_region(
    directory: &Path,
    manifest: &LongCaptureManifest,
    y: u32,
    height: u32,
    max_pixels: u64,
) -> AppResult<Frame> {
    let byte_len = checked_rgba_len(manifest.width, height, max_pixels)?;
    let mut rgba = vec![0_u8; byte_len];
    let tile_end = y
        .checked_add(height)
        .ok_or_else(|| AppError::invalid("长截图瓦片范围溢出"))?;
    let row_bytes = manifest.width as usize * 4;
    let mut copied_rows = 0_u32;
    for segment in &manifest.segments {
        let segment_end = segment.output_y.saturating_add(segment.height);
        let copy_start = y.max(segment.output_y);
        let copy_end = tile_end.min(segment_end);
        if copy_end <= copy_start {
            continue;
        }
        let strip = decode_png(
            &std::fs::read(directory.join(&segment.strip_file))
                .map_err(|error| AppError::io("读取长截图瓦片条带", error))?,
        )?;
        if strip.width != manifest.width || strip.height != segment.height {
            return Err(AppError::new(
                "invalid_long_capture_cache",
                "长截图条带尺寸与清单不一致",
            ));
        }
        let rows = copy_end - copy_start;
        let source_row = copy_start - segment.output_y;
        let target_row = copy_start - y;
        let source_start = source_row as usize * row_bytes;
        let source_end = source_start + rows as usize * row_bytes;
        let target_start = target_row as usize * row_bytes;
        let target_end = target_start + rows as usize * row_bytes;
        rgba[target_start..target_end].copy_from_slice(&strip.rgba[source_start..source_end]);
        copied_rows = copied_rows.saturating_add(rows);
    }
    if let Some(fixed_bottom) = manifest.fixed_bottom.as_ref() {
        let footer_start = manifest.height.saturating_sub(fixed_bottom.height);
        let copy_start = y.max(footer_start);
        let copy_end = tile_end.min(manifest.height);
        if copy_end > copy_start {
            let footer = decode_png(
                &std::fs::read(directory.join(&fixed_bottom.file))
                    .map_err(|error| AppError::io("读取长截图固定底栏", error))?,
            )?;
            if footer.width != manifest.width || footer.height != fixed_bottom.height {
                return Err(AppError::new(
                    "invalid_long_capture_cache",
                    "长截图固定底栏尺寸与清单不一致",
                ));
            }
            let rows = copy_end - copy_start;
            let source_row = copy_start - footer_start;
            let target_row = copy_start - y;
            let source_start = source_row as usize * row_bytes;
            let source_end = source_start + rows as usize * row_bytes;
            let target_start = target_row as usize * row_bytes;
            let target_end = target_start + rows as usize * row_bytes;
            rgba[target_start..target_end].copy_from_slice(&footer.rgba[source_start..source_end]);
            copied_rows = copied_rows.saturating_add(rows);
        }
    }
    if copied_rows != height {
        return Err(AppError::new(
            "invalid_long_capture_cache",
            "长截图瓦片存在缺失条带",
        ));
    }
    Ok(Frame {
        width: manifest.width,
        height,
        rgba,
    })
}

fn encode_png(frame: &Frame) -> AppResult<Vec<u8>> {
    frame.validate()?;
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|error| AppError::new("png_error", format!("创建长截图 PNG 失败: {error}")))?;
        writer
            .write_image_data(&frame.rgba)
            .map_err(|error| AppError::new("png_error", format!("编码长截图 PNG 失败: {error}")))?;
    }
    Ok(png)
}

fn decode_png(bytes: &[u8]) -> AppResult<Frame> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| AppError::new("invalid_png", format!("长截图缓存 PNG 无效: {error}")))?;
    let width = reader.info().width;
    let height = reader.info().height;
    checked_rgba_len(width, height, MAX_FRAME_PIXELS.max(MAX_TILE_PIXELS))?;
    let mut output = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| AppError::new("invalid_png", format!("读取长截图 PNG 失败: {error}")))?;
    let source = &output[..info.buffer_size()];
    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(source),
        png::ColorType::Rgb => {
            for pixel in source.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in source.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for value in source {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        png::ColorType::Indexed => {
            return Err(AppError::new("invalid_png", "长截图 PNG 调色板未展开"));
        }
    }
    let frame = Frame {
        width,
        height,
        rgba,
    };
    frame.validate()?;
    Ok(frame)
}

fn checked_rgba_len(width: u32, height: u32, max_pixels: u64) -> AppResult<usize> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|pixels| *pixels > 0 && *pixels <= max_pixels)
        .ok_or_else(|| AppError::invalid("长截图图片尺寸为空或超过安全限制"))?;
    usize::try_from(pixels.saturating_mul(4)).map_err(|_| AppError::invalid("长截图像素缓冲区过大"))
}

#[cfg(windows)]
fn capture_roi(bounds: PhysicalRect) -> AppResult<Frame> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP,
        HDC, HGDIOBJ, SRCCOPY,
    };

    struct ScreenDc(HDC);
    impl Drop for ScreenDc {
        fn drop(&mut self) {
            unsafe {
                let _ = ReleaseDC(null_mut(), self.0);
            }
        }
    }
    struct MemoryDc(HDC);
    impl Drop for MemoryDc {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
    struct SelectedBitmap {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Drop for SelectedBitmap {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_null() {
                    let _ = SelectObject(self.dc, self.previous);
                }
                let _ = DeleteObject(self.bitmap);
            }
        }
    }

    let byte_len = checked_rgba_len(bounds.width, bounds.height, MAX_FRAME_PIXELS)?;
    let width = i32::try_from(bounds.width)
        .map_err(|_| AppError::invalid("长截图选区宽度超过 Windows 限制"))?;
    let height = i32::try_from(bounds.height)
        .map_err(|_| AppError::invalid("长截图选区高度超过 Windows 限制"))?;
    let screen_dc = ScreenDc(unsafe { GetDC(null_mut()) });
    if screen_dc.0.is_null() {
        return Err(last_windows_error("获取长截图桌面绘图上下文"));
    }
    let memory_dc = MemoryDc(unsafe { CreateCompatibleDC(screen_dc.0) });
    if memory_dc.0.is_null() {
        return Err(last_windows_error("创建长截图绘图上下文"));
    }
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: u32::try_from(byte_len).unwrap_or(0),
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut bits: *mut c_void = null_mut();
    let bitmap = unsafe {
        CreateDIBSection(
            screen_dc.0,
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        )
    };
    if bitmap.is_null() || bits.is_null() {
        return Err(last_windows_error("创建长截图像素缓冲区"));
    }
    let selected = SelectedBitmap {
        dc: memory_dc.0,
        bitmap,
        previous: unsafe { SelectObject(memory_dc.0, bitmap) },
    };
    if unsafe {
        BitBlt(
            memory_dc.0,
            0,
            0,
            width,
            height,
            screen_dc.0,
            bounds.x,
            bounds.y,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err(last_windows_error("复制长截图选区画面"));
    }
    let bgra = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_len) };
    let mut rgba = Vec::with_capacity(byte_len);
    for pixel in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    drop(selected);
    Ok(Frame {
        width: bounds.width,
        height: bounds.height,
        rgba,
    })
}

#[cfg(not(windows))]
fn capture_roi(_bounds: PhysicalRect) -> AppResult<Frame> {
    Err(AppError::new(
        "unsupported_platform",
        "长截图通用引擎仅支持 Windows 10/11",
    ))
}

#[cfg(windows)]
fn send_wheel_scroll(target: &CaptureTarget, delta_units: i32) -> AppResult<()> {
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };

    focus_scroll_target(target)?;
    let magnitude = delta_units.abs().clamp(30, 240);
    let wheel_delta = if delta_units < 0 {
        magnitude
    } else {
        -magnitude
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: wheel_delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(last_windows_error("发送长截图滚动输入"));
    }
    Ok(())
}

#[cfg(windows)]
fn send_scroll_ticks(target: &CaptureTarget, ticks: i32) -> AppResult<()> {
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };

    if ticks == 0 {
        return Ok(());
    }
    focus_scroll_target(target)?;
    let wheel_delta = if ticks < 0 { -120_i32 } else { 120_i32 };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: wheel_delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // Keep every gesture at the standard WHEEL_DELTA. Chromium-based apps,
    // including VS Code, can discard or over-accelerate one oversized delta.
    for _ in 0..ticks.unsigned_abs().min(64) {
        let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        if sent != 1 {
            return Err(last_windows_error("发送长截图滚动输入"));
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_wheel_scroll(_target: &CaptureTarget, _delta_units: i32) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "长截图滚动输入仅支持 Windows",
    ))
}

#[cfg(not(windows))]
fn send_scroll_ticks(_target: &CaptureTarget, _ticks: i32) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "长截图滚动输入仅支持 Windows",
    ))
}

#[cfg(windows)]
fn resolve_scroll_target_windows(anchor: PhysicalPoint) -> Option<ScrollTargetWindows> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};

    let child = unsafe {
        WindowFromPoint(POINT {
            x: anchor.x,
            y: anchor.y,
        })
    };
    if child.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(child, GA_ROOT) };
    Some(ScrollTargetWindows {
        root: (if root.is_null() { child } else { root }) as isize,
        child: child as isize,
    })
}

#[cfg(not(windows))]
fn resolve_scroll_target_windows(_anchor: PhysicalPoint) -> Option<ScrollTargetWindows> {
    None
}

#[cfg(windows)]
fn focus_scroll_target(target: &CaptureTarget) -> AppResult<()> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
        SetForegroundWindow,
    };

    struct ThreadInputAttachment {
        source: u32,
        target: u32,
    }

    impl ThreadInputAttachment {
        fn attach(source: u32, target: u32) -> AppResult<Option<Self>> {
            if source == 0 || target == 0 || source == target {
                return Ok(None);
            }
            if unsafe { AttachThreadInput(source, target, 1) } == 0 {
                return Err(last_windows_error("连接长截图目标输入线程"));
            }
            Ok(Some(Self { source, target }))
        }
    }

    impl Drop for ThreadInputAttachment {
        fn drop(&mut self) {
            unsafe {
                let _ = AttachThreadInput(self.source, self.target, 0);
            }
        }
    }

    set_cursor_position(target.scroll_anchor)?;
    let Some(windows) = target.scroll_windows else {
        return Err(AppError::new(
            "long_capture_target_missing",
            "没有找到滚动位置下方的目标窗口，请返回截图后重新选择滚动内容",
        ));
    };
    let root = windows.root as *mut std::ffi::c_void;
    let child = windows.child as *mut std::ffi::c_void;
    if unsafe { IsWindow(root) } == 0 || unsafe { IsWindow(child) } == 0 {
        return Err(AppError::new(
            "long_capture_target_closed",
            "长截图目标窗口已关闭，请重新截图",
        ));
    }

    let _ = unsafe { SetForegroundWindow(root) };
    std::thread::sleep(Duration::from_millis(60));

    let current_thread = unsafe { GetCurrentThreadId() };
    let foreground = unsafe { GetForegroundWindow() };
    let foreground_thread = if foreground.is_null() {
        0
    } else {
        unsafe { GetWindowThreadProcessId(foreground, std::ptr::null_mut()) }
    };
    let target_thread = unsafe { GetWindowThreadProcessId(child, std::ptr::null_mut()) };
    if target_thread == 0 {
        return Err(AppError::new(
            "long_capture_target_closed",
            "无法读取长截图目标输入线程，请重新截图",
        ));
    }

    let _foreground_attachment = ThreadInputAttachment::attach(current_thread, foreground_thread)?;
    let _target_attachment = if foreground_thread == target_thread {
        None
    } else {
        ThreadInputAttachment::attach(current_thread, target_thread)?
    };
    unsafe {
        let _ = BringWindowToTop(root);
        let _ = SetForegroundWindow(root);
        let _ = SetActiveWindow(root);
        let _ = SetFocus(child);
    }
    std::thread::sleep(Duration::from_millis(80));
    if !scroll_target_has_foreground_focus(target) {
        return Err(AppError::new(
            "long_capture_target_activation_failed",
            "Windows 未能把输入焦点交还给长截图目标。请重新点击选区内可滚动内容后重试",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn scroll_target_has_foreground_focus(target: &CaptureTarget) -> bool {
    use std::mem::size_of;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsChild,
        GA_ROOT, GUITHREADINFO,
    };

    let Some(windows) = target.scroll_windows else {
        return false;
    };
    let expected_root = windows.root as *mut std::ffi::c_void;
    let child = windows.child as *mut std::ffi::c_void;
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_null() {
        return false;
    }
    let root = unsafe { GetAncestor(foreground, GA_ROOT) };
    let foreground_root = if root.is_null() { foreground } else { root };
    if foreground_root != expected_root {
        return false;
    }

    let target_thread = unsafe { GetWindowThreadProcessId(child, std::ptr::null_mut()) };
    if target_thread == 0 {
        return false;
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..GUITHREADINFO::default()
    };
    if unsafe { GetGUIThreadInfo(target_thread, &mut info) } == 0 || info.hwndFocus.is_null() {
        return false;
    }
    info.hwndFocus == child || unsafe { IsChild(child, info.hwndFocus) } != 0
}

#[cfg(not(windows))]
fn focus_scroll_target(_target: &CaptureTarget) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "长截图滚动输入仅支持 Windows",
    ))
}

fn calibrated_wheel_delta_units(
    viewport_height: u32,
    displacement: u32,
    current_units: i32,
) -> i32 {
    if viewport_height == 0 || displacement == 0 {
        return current_units.clamp(30, 240);
    }
    let target_displacement = f64::from(viewport_height) * 0.5;
    let desired = (f64::from(current_units.abs()) * target_displacement / f64::from(displacement))
        .round() as i32;
    let desired = desired.clamp(30, 240);
    ((current_units.abs().clamp(30, 240) + desired) / 2).clamp(30, 240)
}

#[cfg(windows)]
fn current_cursor_position() -> Option<PhysicalPoint> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    (unsafe { GetCursorPos(&mut point) } != 0).then_some(PhysicalPoint {
        x: point.x,
        y: point.y,
    })
}

#[cfg(not(windows))]
fn current_cursor_position() -> Option<PhysicalPoint> {
    None
}

#[cfg(windows)]
fn set_cursor_position(position: PhysicalPoint) -> AppResult<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;
    if unsafe { SetCursorPos(position.x, position.y) } == 0 {
        return Err(last_windows_error("定位长截图滚动锚点"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_cursor_position(_position: PhysicalPoint) -> AppResult<()> {
    Ok(())
}

#[cfg(windows)]
fn last_windows_error(action: &str) -> AppError {
    AppError::new(
        "windows_error",
        format!("{action}失败: {}", std::io::Error::last_os_error()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use tempfile::tempdir;

    fn patterned_frame(width: u32, height: u32, document_y: u32) -> Frame {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in document_y..document_y + height {
            for x in 0..width {
                let mixed = x
                    .wrapping_mul(73)
                    .wrapping_add(y.wrapping_mul(151))
                    .wrapping_add(x.wrapping_mul(y).wrapping_mul(7));
                rgba.extend_from_slice(&[
                    (mixed & 0xff) as u8,
                    ((mixed >> 3).wrapping_add(y * 11) & 0xff) as u8,
                    ((mixed >> 5).wrapping_add(x * 19) & 0xff) as u8,
                    255,
                ]);
            }
        }
        Frame {
            width,
            height,
            rgba,
        }
    }

    fn solid_frame(width: u32, height: u32, value: u8) -> Frame {
        Frame {
            width,
            height,
            rgba: [value, value, value, 255].repeat(width as usize * height as usize),
        }
    }

    fn dark_editor_frame_with_columns(
        width: u32,
        height: u32,
        document_y: u32,
        code_columns: u32,
    ) -> Frame {
        let mut frame = solid_frame(width, height, 28);
        let header_height = 36_u32.min(height);
        let sidebar_width = 180_u32.min(width);
        for y in 0..height {
            for x in 0..width {
                let value = if y < header_height {
                    38 + ((x / 90) % 3) as u8 * 4
                } else if x < sidebar_width {
                    let row = (y - header_height) / 20;
                    if (x + row * 11) % 97 < 18 {
                        62
                    } else {
                        32
                    }
                } else {
                    let document_row = document_y + y - header_height;
                    let line = document_row / 18;
                    let glyph_y = document_row % 18;
                    let editor_x = x - sidebar_width;
                    let column = editor_x / 8;
                    let glyph_x = editor_x % 8;
                    let token = line.wrapping_mul(37).wrapping_add(column.wrapping_mul(19));
                    let glyph_visible = column < code_columns
                        && token % 7 != 0
                        && (4..14).contains(&glyph_y)
                        && (glyph_x == 1 || glyph_x == 2 || (glyph_y == 4 && glyph_x < 6));
                    if glyph_visible {
                        match token % 4 {
                            0 => 186,
                            1 => 132,
                            2 => 104,
                            _ => 156,
                        }
                    } else if column == 0 && glyph_y == 17 {
                        42
                    } else {
                        28
                    }
                };
                let offset = (y as usize * width as usize + x as usize) * 4;
                frame.rgba[offset..offset + 3].fill(value);
            }
        }
        frame
    }

    fn dark_editor_frame(width: u32, height: u32, document_y: u32) -> Frame {
        dark_editor_frame_with_columns(width, height, document_y, 82)
    }

    fn windows_list_frame(width: u32, height: u32, document_y: u32) -> Frame {
        let mut frame = Frame {
            width,
            height,
            rgba: [246, 247, 249, 255].repeat(width as usize * height as usize),
        };
        let header = 48_u32.min(height);
        let footer = 28_u32.min(height.saturating_sub(header));
        let sidebar = 148_u32.min(width);
        for y in 0..height {
            for x in 0..width {
                let rgb = if y < header {
                    let accent = ((x / 72) % 3) as u8 * 4;
                    [225 + accent, 230 + accent, 236 + accent]
                } else if y >= height.saturating_sub(footer) {
                    [233, 235, 239]
                } else if x < sidebar {
                    let selected = ((y - header) / 44) == 2;
                    if selected {
                        [214, 230, 248]
                    } else {
                        [238, 240, 244]
                    }
                } else {
                    let document_row = document_y + y - header;
                    let row = document_row / 34;
                    let row_y = document_row % 34;
                    let body_x = x - sidebar;
                    let row_seed = row.wrapping_mul(1_103).wrapping_add(97);
                    if row_y == 33 {
                        [222, 225, 230]
                    } else if (8..25).contains(&row_y) && (18..36).contains(&body_x) {
                        match row % 4 {
                            0 => [72, 132, 210],
                            1 => [83, 166, 112],
                            2 => [194, 112, 75],
                            _ => [133, 106, 194],
                        }
                    } else if (10..14).contains(&row_y)
                        && body_x > 50
                        && body_x < 130 + row_seed % 360
                    {
                        [70, 74, 82]
                    } else if (19..22).contains(&row_y)
                        && body_x > 50
                        && body_x < 96 + row_seed % 220
                    {
                        [145, 150, 160]
                    } else if row % 2 == 0 {
                        [250, 251, 252]
                    } else {
                        [244, 246, 249]
                    }
                };
                let offset = (y as usize * width as usize + x as usize) * 4;
                frame.rgba[offset..offset + 3].copy_from_slice(&rgb);
            }
        }
        frame
    }

    fn windows_settings_frame(width: u32, height: u32, document_y: u32) -> Frame {
        let mut frame = Frame {
            width,
            height,
            rgba: [250, 250, 250, 255].repeat(width as usize * height as usize),
        };
        let header = 56_u32.min(height);
        let content_left = width / 5;
        let content_right = width.saturating_mul(4) / 5;
        for y in 0..height {
            for x in 0..width {
                let rgb = if y < header {
                    if (18..34).contains(&y) && x > 24 && x < width / 3 {
                        [55, 58, 64]
                    } else {
                        [242, 243, 245]
                    }
                } else if x < content_left || x >= content_right {
                    [250, 250, 250]
                } else {
                    let document_row = document_y + y - header;
                    let section = document_row / 78;
                    let section_y = document_row % 78;
                    let section_x = x - content_left;
                    let section_width = content_right - content_left;
                    if section_y == 77 {
                        [226, 228, 232]
                    } else if (13..18).contains(&section_y)
                        && section_x > 16
                        && section_x < section_width / 2 + (section * 23) % 110
                    {
                        [50, 53, 60]
                    } else if (29..33).contains(&section_y)
                        && section_x > 16
                        && section_x < section_width / 3 + (section * 17) % 90
                    {
                        [137, 141, 149]
                    } else if (22..47).contains(&section_y)
                        && section_x > section_width.saturating_sub(58)
                        && section_x < section_width.saturating_sub(18)
                    {
                        if section % 2 == 0 {
                            [32, 120, 214]
                        } else {
                            [184, 188, 195]
                        }
                    } else {
                        [250, 250, 250]
                    }
                };
                let offset = (y as usize * width as usize + x as usize) * 4;
                frame.rgba[offset..offset + 3].copy_from_slice(&rgb);
            }
        }
        frame
    }

    fn runtime_for_state(state: LongCaptureState) -> LongCaptureRuntime {
        LongCaptureRuntime {
            manifest: LongCaptureManifest {
                schema_version: MANIFEST_SCHEMA_VERSION,
                job_id: "job".to_string(),
                session_id: "session".to_string(),
                state,
                engine: LongCaptureEngine::Wheel,
                selection: PhysicalRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                scroll_anchor: PhysicalPoint { x: 0, y: 0 },
                scope: LongCaptureScope::Selection,
                mode: LongCaptureMode::Current,
                width: 1,
                height: 0,
                message: String::new(),
                segments: Vec::new(),
                fixed_bottom: None,
            },
            pause_requested: false,
            cancel_requested: false,
            finish_requested: false,
            retry_current: false,
            generation: 0,
            worker_done: false,
            ready_emitted: false,
            browser_active: false,
            browser_restore_needed: false,
            browser_session: None,
        }
    }

    fn job_for_state(directory: PathBuf, state: LongCaptureState) -> LongCaptureJob {
        LongCaptureJob {
            directory,
            target: CaptureTarget {
                bounds: PhysicalRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                scroll_anchor: PhysicalPoint { x: 0, y: 0 },
                monitor: MonitorBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    scale_factor: 1.0,
                },
                mode: LongCaptureMode::Current,
                control_overlaps_roi: false,
                scroll_windows: None,
            },
            control_window_instance: None,
            hidden_pin_labels: Vec::new(),
            browser: None,
            operation_lock: Mutex::new(()),
            runtime: Mutex::new(runtime_for_state(state)),
            wake: Condvar::new(),
        }
    }

    fn test_store(cache_root: PathBuf) -> LongScreenshotStore {
        LongScreenshotStore {
            cache_root,
            browser_bridge: None,
            job: Mutex::new(None),
            annotation_exports: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            pending_start: Mutex::new(None),
            control_destroys: Mutex::new(ControlDestroyTracker::default()),
            control_destroyed: Condvar::new(),
        }
    }

    #[test]
    fn terminal_state_transitions_are_monotonic() {
        for state in [
            LongCaptureState::Ready,
            LongCaptureState::Failed,
            LongCaptureState::Canceled,
        ] {
            let mut failed = state;
            assert!(!transition_to_failed(&mut failed));
            assert_eq!(failed, state);

            let mut canceled = state;
            assert!(!transition_to_canceled(&mut canceled));
            assert_eq!(canceled, state);
        }

        let mut capturing = LongCaptureState::Capturing;
        assert!(transition_to_failed(&mut capturing));
        assert_eq!(capturing, LongCaptureState::Failed);

        let mut paused = LongCaptureState::Paused;
        assert!(transition_to_canceled(&mut paused));
        assert_eq!(paused, LongCaptureState::Canceled);
    }

    #[test]
    fn unexpected_control_destroy_never_cancels_a_terminal_capture() {
        assert!(should_cancel_after_control_destroy(
            LongCaptureState::Preparing
        ));
        assert!(should_cancel_after_control_destroy(
            LongCaptureState::Capturing
        ));
        assert!(should_cancel_after_control_destroy(
            LongCaptureState::Paused
        ));
        assert!(!should_cancel_after_control_destroy(
            LongCaptureState::Ready
        ));
        assert!(!should_cancel_after_control_destroy(
            LongCaptureState::Failed
        ));
        assert!(!should_cancel_after_control_destroy(
            LongCaptureState::Canceled
        ));
    }

    #[test]
    fn outline_window_border_stays_outside_the_selected_pixels() {
        let selection = PhysicalRect {
            x: -2_200,
            y: 120,
            width: 1_200,
            height: 800,
        };
        for scale_factor in [1.0, 1.5, 4.0] {
            let monitor = MonitorBounds {
                x: -2_560,
                y: 0,
                width: 2_560,
                height: 1_440,
                scale_factor,
            };
            let (position, size) = outline_window_geometry(&monitor, selection);
            let right = i64::from(position.x) + i64::from(size.width);
            let bottom = i64::from(position.y) + i64::from(size.height);
            let selection_right = i64::from(selection.x) + i64::from(selection.width);
            let selection_bottom = i64::from(selection.y) + i64::from(selection.height);
            let maximum_painted_extent = (5.0 * scale_factor).ceil() as i64;

            assert!(i64::from(selection.x - position.x) > maximum_painted_extent);
            assert!(i64::from(selection.y - position.y) > maximum_painted_extent);
            assert!(right - selection_right > maximum_painted_extent);
            assert!(bottom - selection_bottom > maximum_painted_extent);
        }
    }

    #[test]
    fn awaited_control_destroy_event_wakes_the_exact_waiter() {
        let root = tempdir().unwrap();
        let store = Arc::new(test_store(root.path().to_path_buf()));
        assert!(store.expect_control_destroy(100, true));
        assert!(!store.expect_control_destroy(100, true));
        let waiter_store = Arc::clone(&store);
        let waiter = std::thread::spawn(move || {
            waiter_store.wait_for_control_destroy(100, Duration::from_secs(1))
        });
        assert!(store.consume_expected_control_destroy(Some(100), Some(100)));
        assert!(waiter.join().unwrap());
        assert!(!store.consume_expected_control_destroy(Some(100), None));
        assert!(!store.consume_expected_control_destroy(None, None));
    }

    #[test]
    fn timed_out_control_destroy_still_suppresses_its_late_event() {
        let root = tempdir().unwrap();
        let store = test_store(root.path().to_path_buf());
        assert!(store.expect_control_destroy(200, true));
        assert!(!store.wait_for_control_destroy(200, Duration::from_millis(1)));
        {
            let tracker = lock_unpoisoned(&store.control_destroys);
            assert_eq!(tracker.expected.get(&200), Some(&false));
            assert!(!tracker.completed.contains(&200));
        }
        assert!(store.consume_expected_control_destroy(Some(200), None));
        let tracker = lock_unpoisoned(&store.control_destroys);
        assert!(!tracker.expected.contains_key(&200));
        assert!(!tracker.completed.contains(&200));
    }

    #[test]
    fn duplicate_destroy_request_does_not_erase_a_completed_event() {
        let root = tempdir().unwrap();
        let store = test_store(root.path().to_path_buf());
        assert!(store.expect_control_destroy(250, true));
        assert!(store.consume_expected_control_destroy(Some(250), None));
        assert!(!store.expect_control_destroy(250, false));
        assert!(store.wait_for_control_destroy(250, Duration::from_millis(1)));
    }

    #[test]
    fn non_awaited_control_destroy_does_not_retain_completion_state() {
        let root = tempdir().unwrap();
        let store = test_store(root.path().to_path_buf());
        assert!(store.expect_control_destroy(300, false));
        assert!(store.consume_expected_control_destroy(Some(300), None));
        let tracker = lock_unpoisoned(&store.control_destroys);
        assert!(!tracker.expected.contains_key(&300));
        assert!(!tracker.completed.contains(&300));
    }

    #[test]
    fn unidentified_destroy_event_only_consumes_expected_when_no_replacement_exists() {
        let root = tempdir().unwrap();
        let store = test_store(root.path().to_path_buf());
        assert!(store.expect_control_destroy(400, false));
        assert!(!store.consume_expected_control_destroy(None, Some(401)));
        assert!(lock_unpoisoned(&store.control_destroys)
            .expected
            .contains_key(&400));
        assert!(store.consume_expected_control_destroy(Some(400), Some(400)));

        assert!(store.expect_control_destroy(401, true));
        assert!(store.consume_expected_control_destroy(None, None));
        assert!(store.wait_for_control_destroy(401, Duration::from_millis(1)));
    }

    #[test]
    fn delayed_control_destroy_event_cannot_cancel_a_replacement_job() {
        assert!(destroyed_control_window_matches_job(
            Some(10),
            Some(10),
            Some(10)
        ));
        assert!(!destroyed_control_window_matches_job(
            Some(10),
            Some(11),
            Some(11)
        ));
        assert!(!destroyed_control_window_matches_job(
            None,
            Some(11),
            Some(11)
        ));
        assert!(destroyed_control_window_matches_job(None, Some(11), None));
    }

    #[test]
    fn orphaned_job_detection_requires_the_exact_screenshot_session() {
        assert!(screenshot_session_owns_job(Some("session-1"), "session-1"));
        assert!(!screenshot_session_owns_job(None, "session-1"));
        assert!(!screenshot_session_owns_job(
            Some("replacement-session"),
            "session-1"
        ));
    }

    #[test]
    fn cancel_wakes_a_paused_worker_even_when_manifest_persistence_fails() {
        let root = tempdir().unwrap();
        let blocked_directory = root.path().join("cache-parent-is-a-file");
        std::fs::write(&blocked_directory, b"not a directory").unwrap();
        let job = Arc::new(job_for_state(blocked_directory, LongCaptureState::Paused));
        {
            let mut runtime = lock_unpoisoned(&job.runtime);
            runtime.pause_requested = true;
        }

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter_job = Arc::clone(&job);
        let waiter = std::thread::spawn(move || {
            let runtime = lock_unpoisoned(&waiter_job.runtime);
            ready_tx.send(()).unwrap();
            let (runtime, timeout) = waiter_job
                .wake
                .wait_timeout_while(runtime, Duration::from_secs(2), |runtime| {
                    !runtime.cancel_requested
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            done_tx
                .send((runtime.cancel_requested, timeout.timed_out()))
                .unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let outcome = request_job_cancel(&job, "test cancellation");
        assert!(outcome.persistence_error.is_some());
        assert_eq!(outcome.status.state, LongCaptureState::Canceled);
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (true, false)
        );
        waiter.join().unwrap();
    }

    #[test]
    fn worker_done_wait_completes_after_notification() {
        let root = tempdir().unwrap();
        let job = Arc::new(job_for_state(
            root.path().join("worker-done"),
            LongCaptureState::Ready,
        ));
        let completed_job = Arc::clone(&job);
        let completed = std::thread::spawn(move || {
            let mut runtime = lock_unpoisoned(&completed_job.runtime);
            runtime.worker_done = true;
            completed_job.wake.notify_all();
        });
        assert!(wait_for_worker_done(&job, Duration::from_secs(1)));
        completed.join().unwrap();
    }

    #[test]
    fn worker_done_is_not_observable_until_cleanup_finishes() {
        let root = tempdir().unwrap();
        let job = Arc::new(job_for_state(
            root.path().join("worker-cleanup"),
            LongCaptureState::Ready,
        ));
        let worker_job = Arc::clone(&job);
        let (cleanup_started_tx, cleanup_started_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            complete_worker_after_cleanup(&worker_job, || {
                cleanup_started_tx.send(()).unwrap();
                continue_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            });
        });

        cleanup_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(job.operation_lock.try_lock().is_err());
        assert!(!wait_for_worker_done(&job, Duration::from_millis(10)));
        continue_tx.send(()).unwrap();
        assert!(wait_for_worker_done(&job, Duration::from_secs(1)));
        worker.join().unwrap();
    }

    #[test]
    fn worker_done_timeout_leaves_the_job_in_the_store() {
        let root = tempdir().unwrap();
        let job = Arc::new(job_for_state(
            root.path().join("worker-timeout"),
            LongCaptureState::Ready,
        ));
        let store = test_store(root.path().to_path_buf());
        *lock_unpoisoned(&store.job) = Some(Arc::clone(&job));

        assert!(!wait_for_worker_done(&job, Duration::from_millis(1)));
        let stored = lock_unpoisoned(&store.job).as_ref().cloned().unwrap();
        assert!(Arc::ptr_eq(&stored, &job));
    }

    #[test]
    fn control_surface_is_not_restored_after_terminal_or_requested_stop() {
        let mut runtime = runtime_for_state(LongCaptureState::Capturing);
        assert!(control_surface_needed(&runtime));

        runtime.manifest.state = LongCaptureState::Paused;
        assert!(!control_surface_needed(&runtime));
        runtime.manifest.state = LongCaptureState::Ready;
        assert!(!control_surface_needed(&runtime));
        runtime.manifest.state = LongCaptureState::Canceled;
        assert!(!control_surface_needed(&runtime));

        runtime.manifest.state = LongCaptureState::Capturing;
        runtime.pause_requested = true;
        assert!(!control_surface_needed(&runtime));
        runtime.pause_requested = false;
        runtime.finish_requested = true;
        assert!(!control_surface_needed(&runtime));
        runtime.finish_requested = false;
        runtime.cancel_requested = true;
        assert!(!control_surface_needed(&runtime));
    }

    #[test]
    fn worker_cleanup_honors_cancel_requested_even_if_state_stays_ready() {
        let mut runtime = runtime_for_state(LongCaptureState::Ready);
        runtime.cancel_requested = true;
        assert!(should_cleanup_after_worker(&runtime));

        runtime.cancel_requested = false;
        assert!(!should_cleanup_after_worker(&runtime));
        runtime.manifest.state = LongCaptureState::Canceled;
        assert!(should_cleanup_after_worker(&runtime));
    }

    #[test]
    fn cancel_stop_takes_precedence_over_a_late_finish_request() {
        let mut runtime = runtime_for_state(LongCaptureState::Capturing);
        runtime.finish_requested = true;
        let job = LongCaptureJob {
            directory: PathBuf::new(),
            target: CaptureTarget {
                bounds: PhysicalRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                scroll_anchor: PhysicalPoint { x: 0, y: 0 },
                monitor: MonitorBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    scale_factor: 1.0,
                },
                mode: LongCaptureMode::Current,
                control_overlaps_roi: false,
                scroll_windows: None,
            },
            control_window_instance: None,
            hidden_pin_labels: Vec::new(),
            browser: None,
            operation_lock: Mutex::new(()),
            runtime: Mutex::new(runtime),
            wake: Condvar::new(),
        };
        assert_eq!(requested_worker_stop(&job), Some(WorkerStop::Finish));
        {
            let mut runtime = lock_unpoisoned(&job.runtime);
            runtime.cancel_requested = true;
        }
        assert_eq!(requested_worker_stop(&job), Some(WorkerStop::Cancel));
    }

    #[test]
    fn visible_surface_switch_keeps_source_when_target_fails() {
        let calls = RefCell::new(Vec::new());
        let result = switch_visible_surface(
            || {
                calls.borrow_mut().push("target");
                Err(AppError::new("target", "target failed"))
            },
            || {
                calls.borrow_mut().push("source");
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(&*calls.borrow(), &["target"]);
    }

    #[test]
    fn visible_surface_switch_reports_source_failure_after_target_is_visible() {
        let target_visible = Cell::new(false);
        let result = switch_visible_surface(
            || {
                target_visible.set(true);
                Ok(())
            },
            || Err(AppError::new("source", "source failed")),
        );

        assert!(result.is_err());
        assert!(target_visible.get());
    }

    #[test]
    fn status_and_capability_use_frontend_camel_case_contract() {
        let status = LongCaptureStatus {
            job_id: "job-1".to_string(),
            session_id: "session-1".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Wheel,
            frame_count: 3,
            width: 800,
            height: 2_400,
            message: "done".to_string(),
            can_undo: true,
        };
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["jobId"], "job-1");
        assert_eq!(json["sessionId"], "session-1");
        assert_eq!(json["state"], "ready");
        assert_eq!(json["frameCount"], 3);
        assert_eq!(json["canUndo"], true);
        let capability = serde_json::to_value(long_capture_capability(None)).unwrap();
        assert!(capability.get("available").is_some());
        assert!(capability.get("supported").is_some());
        assert_eq!(capability["preferredEngine"], "manual");
    }

    #[test]
    fn manual_engine_never_uses_automatic_scroll_input() {
        assert!(engine_uses_manual_scrolling(LongCaptureEngine::Manual));
        assert!(!engine_uses_manual_scrolling(LongCaptureEngine::Wheel));
        assert!(!engine_uses_manual_scrolling(
            LongCaptureEngine::BrowserEnhanced
        ));
    }

    #[test]
    fn manual_capture_schedule_throttles_active_frames_and_adds_a_settled_frame() {
        let started = Instant::now();
        let mut schedule = ManualCaptureSchedule::new(started);

        schedule.observe_scroll(started);
        assert_eq!(
            schedule.next_trigger(started),
            Some(ManualCaptureTrigger::ActiveScroll)
        );
        schedule.observe_scroll(started + Duration::from_millis(40));
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(59)),
            None
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(60)),
            Some(ManualCaptureTrigger::ActiveScroll)
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(120)),
            Some(ManualCaptureTrigger::ActiveScroll)
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(289)),
            Some(ManualCaptureTrigger::ActiveScroll)
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(290)),
            Some(ManualCaptureTrigger::Settled)
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(349)),
            None
        );
    }

    #[test]
    fn manual_capture_schedule_polls_for_keyboard_and_scrollbar_changes() {
        let started = Instant::now();
        let mut schedule = ManualCaptureSchedule::new(started);
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(59)),
            None
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(60)),
            Some(ManualCaptureTrigger::FallbackPoll)
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(119)),
            None
        );
        assert_eq!(
            schedule.next_trigger(started + Duration::from_millis(120)),
            Some(ManualCaptureTrigger::FallbackPoll)
        );
    }

    #[test]
    fn sparse_cross_viewport_change_can_probe_without_claiming_distributed_motion() {
        let sparse = SparseFrameChange {
            changed_rows: 20,
            sampled_rows: 720,
            changed_samples: 12,
            strong_samples: 2,
            total_samples: 10_000,
            changed_groups: 1,
            sampled_groups: 32,
            changed_vertical_bands: 2,
        };
        assert!(sparse.is_match_probe_candidate());
        assert!(!sparse.is_distributed_change_candidate());
        assert!(sparse.is_visible_movement_candidate());

        let localized = SparseFrameChange {
            changed_vertical_bands: 1,
            ..sparse
        };
        assert!(!localized.is_match_probe_candidate());
        assert!(!localized.is_visible_movement_candidate());
    }

    #[test]
    fn unmatched_manual_motion_pauses_instead_of_waiting_forever() {
        let started = Instant::now();
        let mut unmatched = ManualUnmatchedMotion::default();

        assert!(!unmatched.observe(started, true));
        assert!(!unmatched.observe(
            started + MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER - Duration::from_millis(1),
            true
        ));
        assert!(unmatched.observe(started + MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER, true));

        assert!(!unmatched.observe(
            started + MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER + Duration::from_millis(1),
            false
        ));
        assert!(!unmatched.observe(
            started + MANUAL_SCROLL_UNMATCHED_PAUSE_AFTER + Duration::from_secs(30),
            true
        ));
    }

    #[test]
    fn screenshot_reentry_keeps_the_existing_long_capture_surface() {
        assert_eq!(
            long_capture_reentry_surface(true, None, None),
            Some(LongCaptureReentrySurface::Pending)
        );
        assert_eq!(
            long_capture_reentry_surface(
                false,
                Some(LongCaptureState::Capturing),
                Some(LongCaptureEngine::Wheel),
            ),
            Some(LongCaptureReentrySurface::Control)
        );
        assert_eq!(
            long_capture_reentry_surface(
                false,
                Some(LongCaptureState::Paused),
                Some(LongCaptureEngine::Manual),
            ),
            Some(LongCaptureReentrySurface::Control)
        );
        assert_eq!(
            long_capture_reentry_surface(
                false,
                Some(LongCaptureState::Paused),
                Some(LongCaptureEngine::Wheel),
            ),
            Some(LongCaptureReentrySurface::Overlay)
        );
        assert_eq!(
            long_capture_reentry_surface(
                false,
                Some(LongCaptureState::Ready),
                Some(LongCaptureEngine::Manual),
            ),
            Some(LongCaptureReentrySurface::Overlay)
        );
        assert_eq!(
            long_capture_reentry_surface(
                false,
                Some(LongCaptureState::Canceled),
                Some(LongCaptureEngine::Wheel),
            ),
            None
        );
        assert_eq!(long_capture_reentry_surface(false, None, None), None);
    }

    #[cfg(windows)]
    #[test]
    fn control_window_style_preserves_flags_and_disables_activation() {
        use windows_sys::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE;

        let existing = 0x0000_0080_isize;
        let configured = control_window_ex_style(existing);
        assert_eq!(configured & existing, existing);
        assert_ne!(configured & WS_EX_NOACTIVATE as isize, 0);
    }

    #[test]
    fn validates_relative_physical_coordinates_on_negative_monitor() {
        let monitor = MonitorBounds {
            x: -1_920,
            y: -200,
            width: 1_920,
            height: 1_080,
            scale_factor: 1.5,
        };
        let target = validate_capture_target(
            &monitor,
            PhysicalRect {
                x: 100,
                y: 80,
                width: 800,
                height: 700,
            },
            PhysicalPoint { x: 400, y: 300 },
            LongCaptureMode::Current,
        )
        .unwrap();
        assert_eq!(target.bounds.x, -1_820);
        assert_eq!(target.bounds.y, -120);
        assert_eq!(target.scroll_anchor.x, -1_520);
        assert_eq!(target.scroll_anchor.y, 100);
        assert!(validate_capture_target(
            &monitor,
            PhysicalRect {
                x: 1_800,
                y: 0,
                width: 200,
                height: 100,
            },
            PhysicalPoint { x: 1_850, y: 50 },
            LongCaptureMode::Current,
        )
        .is_err());
    }

    #[test]
    fn grayscale_multi_strip_match_finds_exact_vertical_displacement() {
        let previous = patterned_frame(240, 180, 0);
        let current = patterned_frame(240, 180, 67);
        let matched = find_vertical_overlap(&previous, &current).unwrap();
        assert_eq!(matched.displacement, 67);
        assert!(
            matched.confidence > 0.7,
            "confidence={}",
            matched.confidence
        );
    }

    #[test]
    fn phase_overlap_hint_is_verified_at_the_exact_scroll_displacement() {
        let previous = patterned_frame(640, 540, 0);
        let current = patterned_frame(640, 540, 137);
        let matched = phase_vertical_overlap(&previous, &current)
            .expect("phase hint should survive full-resolution seam verification");
        assert_eq!(matched.displacement, 137);
        assert!(
            matched.confidence >= 0.5,
            "confidence={}",
            matched.confidence
        );
    }

    #[test]
    fn block_match_detects_windows_list_with_fixed_chrome() {
        let previous = windows_list_frame(1_080, 680, 0);
        for displacement in [17_u32, 53, 141] {
            let current = windows_list_frame(1_080, 680, displacement);
            let matched = reliable_vertical_overlap(&previous, &current)
                .expect("Windows-style list scroll should have a reliable overlap");
            assert_eq!(matched.displacement, displacement);
            assert!(
                matched.confidence >= 0.65,
                "confidence={}",
                matched.confidence
            );
            let matched = find_vertical_overlap(&previous, &current)
                .expect("verified matcher should preserve fixed chrome metadata");
            assert_eq!(matched.static_bottom, 28);
        }
    }

    #[test]
    fn fixed_bottom_requires_two_consistent_frames() {
        let mut tracker = FixedBottomTracker::default();
        assert_eq!(tracker.observe(28), None);
        assert_eq!(tracker.observe(27), Some(27));

        let mut reset = FixedBottomTracker::default();
        assert_eq!(reset.observe(28), None);
        assert_eq!(reset.observe(0), None);
        assert_eq!(reset.observe(28), None);
        assert_eq!(reset.observe(40), None);
    }

    #[test]
    fn fixed_bottom_rebuild_keeps_body_continuous_and_appends_footer_once() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("frames")).unwrap();
        std::fs::create_dir_all(root.path().join("strips")).unwrap();
        let width = 420_u32;
        let height = 260_u32;
        let displacement = 53_u32;
        let footer_height = 28_u32;
        let frames = [
            windows_list_frame(width, height, 0),
            windows_list_frame(width, height, displacement),
            windows_list_frame(width, height, displacement * 2),
        ];
        for (index, frame) in frames.iter().enumerate() {
            atomic_write(
                &root.path().join(format!("frames/{index:06}.png")),
                &encode_png(frame).unwrap(),
            )
            .unwrap();
        }
        let job = job_for_state(root.path().to_path_buf(), LongCaptureState::Capturing);
        let mut manifest = runtime_for_state(LongCaptureState::Capturing).manifest;
        manifest.selection.width = width;
        manifest.selection.height = height;
        manifest.width = width;
        manifest.height = height + displacement * 2;
        manifest.segments = (0..3)
            .map(|index| LongCaptureSegment {
                index,
                output_y: if index == 0 {
                    0
                } else {
                    height + displacement * (index - 1)
                },
                height: if index == 0 { height } else { displacement },
                displacement: if index == 0 { height } else { displacement },
                confidence: 1.0,
                frame_file: format!("frames/{index:06}.png"),
                strip_file: format!("strips/{index:06}.png"),
            })
            .collect();

        rebuild_strips_with_fixed_bottom(&job, &mut manifest, footer_height).unwrap();
        assert_eq!(manifest.height, height + displacement * 2);
        assert_eq!(
            manifest
                .segments
                .iter()
                .map(|segment| (segment.output_y, segment.height))
                .collect::<Vec<_>>(),
            vec![
                (0, height - footer_height),
                (height - footer_height, displacement),
                (height - footer_height + displacement, displacement),
            ]
        );

        let composed =
            compose_region(root.path(), &manifest, 0, manifest.height, MAX_LONG_PIXELS).unwrap();
        let expected_parts = [
            crop_rows(&frames[0], 0, height - footer_height).unwrap(),
            crop_rows(
                &frames[1],
                height - footer_height - displacement,
                displacement,
            )
            .unwrap(),
            crop_rows(
                &frames[2],
                height - footer_height - displacement,
                displacement,
            )
            .unwrap(),
            crop_rows(&frames[2], height - footer_height, footer_height).unwrap(),
        ];
        let expected = expected_parts
            .into_iter()
            .flat_map(|part| part.rgba)
            .collect::<Vec<_>>();
        assert_eq!(composed.rgba, expected);

        let output = root.path().join("fixed-bottom-export.png");
        stream_manifest_png(root.path(), &manifest, &output).unwrap();
        let exported = decode_png(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(exported.rgba, expected);
    }

    #[test]
    fn block_match_detects_sparse_windows_settings_content() {
        let previous = windows_settings_frame(1_200, 720, 0);
        let current = windows_settings_frame(1_200, 720, 39);
        let matched = reliable_vertical_overlap(&previous, &current)
            .expect("sparse settings content should have a reliable overlap");
        assert_eq!(matched.displacement, 39);
        assert!(
            matched.confidence >= 0.6,
            "confidence={}",
            matched.confidence
        );
    }

    #[test]
    fn overlap_match_rejects_ambiguous_low_texture_frames() {
        let previous = solid_frame(160, 120, 245);
        let current = solid_frame(160, 120, 245);
        assert!(find_vertical_overlap(&previous, &current).is_none());
        let (previous_strips, current_strips) = gray_strips_for_pair(&previous, &current).unwrap();
        assert_eq!(
            alignment_score(&previous_strips, &current_strips, 0, 1),
            Some(0.0)
        );
    }

    #[test]
    fn manual_scroll_detects_sparse_dark_editor_text() {
        let previous = dark_editor_frame(960, 540, 0);
        let current = dark_editor_frame(960, 540, 54);
        assert_eq!(
            manual_scroll_evidence(&previous, &current),
            Some(ManualScrollEvidence::SignificantVisualChange)
        );
        let overlap = find_vertical_overlap(&previous, &current).unwrap();
        assert_eq!(overlap.displacement, 54);
    }

    #[test]
    fn manual_scroll_rejects_a_step_with_too_little_overlap() {
        let previous = patterned_frame(640, 540, 0);
        let current = patterned_frame(640, 540, 450);
        assert!(find_vertical_overlap(&previous, &current).is_none());
    }

    #[test]
    fn manual_scroll_detects_narrow_short_code_lines() {
        for displacement in [18, 54, 126] {
            for code_columns in [8, 12, 20] {
                let previous = dark_editor_frame_with_columns(960, 540, 0, code_columns);
                let current = dark_editor_frame_with_columns(960, 540, displacement, code_columns);
                assert_eq!(
                    manual_scroll_evidence(&previous, &current),
                    Some(ManualScrollEvidence::SignificantVisualChange),
                    "displacement={displacement}, code_columns={code_columns}"
                );
                assert_eq!(
                    find_vertical_overlap(&previous, &current)
                        .expect("narrow code scroll should have a reliable seam")
                        .displacement,
                    displacement,
                    "displacement={displacement}, code_columns={code_columns}"
                );
            }
        }
    }

    #[test]
    fn manual_scroll_ignores_static_frame_and_small_caret_change() {
        let previous = dark_editor_frame(960, 540, 0);
        assert_eq!(manual_scroll_evidence(&previous, &previous), None);

        let mut caret = previous.clone();
        for y in 210..234_usize {
            for x in 620..622_usize {
                let offset = (y * caret.width as usize + x) * 4;
                caret.rgba[offset..offset + 3].fill(220);
            }
        }
        assert_eq!(manual_scroll_evidence(&previous, &caret), None);
    }

    #[test]
    fn manual_scroll_fails_closed_for_invalid_frame_geometry() {
        let previous = dark_editor_frame(320, 240, 0);
        let different_size = dark_editor_frame(319, 240, 24);
        assert_eq!(manual_scroll_evidence(&previous, &different_size), None);
        assert!(find_vertical_overlap(&previous, &different_size).is_none());

        let mut truncated = previous.clone();
        truncated.rgba.truncate(truncated.rgba.len() - 4);
        assert_eq!(manual_scroll_evidence(&previous, &truncated), None);
        assert!(gray_strips_for_pair(&previous, &truncated).is_none());
        assert!(find_vertical_overlap(&previous, &truncated).is_none());
    }

    #[test]
    fn manual_scroll_ignores_local_animation() {
        let previous = dark_editor_frame(960, 540, 0);
        let mut animated = previous.clone();
        for y in 120..300_usize {
            for x in 610..690_usize {
                let offset = (y * animated.width as usize + x) * 4;
                let value = if (x / 6 + y / 6) % 2 == 0 { 225 } else { 48 };
                animated.rgba[offset..offset + 3].fill(value);
            }
        }
        assert_eq!(manual_scroll_evidence(&previous, &animated), None);
        assert!(find_vertical_overlap(&previous, &animated).is_none());
    }

    #[test]
    fn manual_scroll_rejects_tall_narrow_animation_without_global_displacement() {
        let previous = dark_editor_frame(960, 540, 0);
        let mut animated = previous.clone();
        for y in 40..520_usize {
            for x in 610..690_usize {
                let offset = (y * animated.width as usize + x) * 4;
                let value = if (x / 6 + y / 6) % 2 == 0 { 225 } else { 48 };
                animated.rgba[offset..offset + 3].fill(value);
            }
        }
        let change = sparse_frame_change(&previous, &animated).unwrap();
        assert!(change.is_distributed_change_candidate());
        assert!(find_vertical_overlap(&previous, &animated).is_none());
        assert_eq!(manual_scroll_evidence(&previous, &animated), None);
    }

    #[test]
    fn overlap_rejects_repeated_rows_with_local_animation() {
        let width = 960_u32;
        let height = 540_u32;
        let mut previous = solid_frame(width, height, 24);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let stripe = ((y % 24) * 9 + (x % 37) * 3) as u8;
                let value = 36_u8.saturating_add(stripe % 150);
                let offset = (y * width as usize + x) * 4;
                previous.rgba[offset..offset + 3].fill(value);
            }
        }
        let mut animated = previous.clone();
        for y in 90..330_usize {
            for x in 40..180_usize {
                let offset = (y * width as usize + x) * 4;
                let value = if (x + y) % 11 < 5 { 230 } else { 18 };
                animated.rgba[offset..offset + 3].fill(value);
            }
        }
        assert!(find_vertical_overlap(&previous, &animated).is_none());
    }

    #[test]
    fn overlap_search_candidate_budget_is_bounded_at_4k_heights() {
        for height in [2_160_usize, 3_840_usize] {
            let minimum = 2_usize;
            let maximum = maximum_legacy_displacement(height);
            let (coarse, step) = coarse_displacement_candidates(minimum, maximum);
            assert!(coarse.len() <= MATCH_MAX_COARSE_CANDIDATES);

            let scores = coarse
                .iter()
                .enumerate()
                .map(|(index, displacement)| ScoredDisplacement {
                    displacement: *displacement,
                    score: index as f32,
                })
                .collect::<Vec<_>>();
            let seeds = select_refine_seeds(&scores, step);
            let fine = refinement_candidates(&seeds, minimum, maximum, step);
            assert!(seeds.len() <= MATCH_MAX_REFINE_SEEDS);
            assert!(fine.len() <= MATCH_MAX_REFINE_SEEDS * (step * 2 + 1));

            let sampled_row_budget =
                coarse.len() * MATCH_MAX_COARSE_ROWS + fine.len() * MATCH_MAX_FINE_ROWS;
            assert!(sampled_row_budget < 100_000, "height={height}");
        }
    }

    #[test]
    fn control_overlay_mask_copies_only_the_covered_pixels() {
        let source = solid_frame(4, 3, 25);
        let mut destination = solid_frame(4, 3, 220);
        copy_frame_region(
            &mut destination,
            &source,
            PhysicalRect {
                x: 1,
                y: 1,
                width: 2,
                height: 1,
            },
        )
        .unwrap();
        for y in 0..3_usize {
            for x in 0..4_usize {
                let offset = (y * 4 + x) * 4;
                let expected = if y == 1 && (1..3).contains(&x) {
                    25
                } else {
                    220
                };
                assert_eq!(destination.rgba[offset], expected);
            }
        }
    }

    #[test]
    fn tile_composition_reads_only_intersecting_strips() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("frames")).unwrap();
        std::fs::create_dir_all(root.path().join("strips")).unwrap();
        let first = Frame {
            width: 3,
            height: 3,
            rgba: [
                10, 0, 0, 255, 10, 0, 0, 255, 10, 0, 0, 255, 20, 0, 0, 255, 20, 0, 0, 255, 20, 0,
                0, 255, 30, 0, 0, 255, 30, 0, 0, 255, 30, 0, 0, 255,
            ]
            .to_vec(),
        };
        let second = Frame {
            width: 3,
            height: 2,
            rgba: [
                40, 0, 0, 255, 40, 0, 0, 255, 40, 0, 0, 255, 50, 0, 0, 255, 50, 0, 0, 255, 50, 0,
                0, 255,
            ]
            .to_vec(),
        };
        std::fs::write(
            root.path().join("frames/000000.png"),
            encode_png(&first).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("strips/000001.png"),
            encode_png(&second).unwrap(),
        )
        .unwrap();
        let manifest = LongCaptureManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            job_id: "job".to_string(),
            session_id: "session".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Wheel,
            selection: PhysicalRect {
                x: 0,
                y: 0,
                width: 3,
                height: 3,
            },
            scroll_anchor: PhysicalPoint { x: 1, y: 1 },
            scope: LongCaptureScope::Selection,
            mode: LongCaptureMode::Current,
            width: 3,
            height: 5,
            message: "ready".to_string(),
            segments: vec![
                LongCaptureSegment {
                    index: 0,
                    output_y: 0,
                    height: 3,
                    displacement: 3,
                    confidence: 1.0,
                    frame_file: "frames/000000.png".to_string(),
                    strip_file: "frames/000000.png".to_string(),
                },
                LongCaptureSegment {
                    index: 1,
                    output_y: 3,
                    height: 2,
                    displacement: 2,
                    confidence: 0.9,
                    frame_file: "frames/000001.png".to_string(),
                    strip_file: "strips/000001.png".to_string(),
                },
            ],
            fixed_bottom: None,
        };
        let tile = compose_tile(root.path(), &manifest, 2, 3).unwrap();
        assert_eq!((tile.width, tile.height), (3, 3));
        assert_eq!(tile.rgba[0], 30);
        assert_eq!(tile.rgba[3 * 4], 40);
        assert_eq!(tile.rgba[6 * 4], 50);
    }

    #[test]
    fn fixed_bottom_is_appended_once_in_tiles_and_streamed_exports() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("strips")).unwrap();
        for (name, frame) in [
            ("body.png", solid_frame(2, 2, 20)),
            ("tail.png", solid_frame(2, 1, 90)),
            (FIXED_BOTTOM_FILE, solid_frame(2, 1, 220)),
        ] {
            std::fs::write(
                root.path().join("strips").join(name),
                encode_png(&frame).unwrap(),
            )
            .unwrap();
        }
        let manifest = LongCaptureManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            job_id: "job".to_string(),
            session_id: "session".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Manual,
            selection: PhysicalRect {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            },
            scroll_anchor: PhysicalPoint { x: 1, y: 1 },
            scope: LongCaptureScope::Selection,
            mode: LongCaptureMode::Manual,
            width: 2,
            height: 4,
            message: "ready".to_string(),
            segments: vec![
                LongCaptureSegment {
                    index: 0,
                    output_y: 0,
                    height: 2,
                    displacement: 2,
                    confidence: 1.0,
                    frame_file: "unused.png".to_string(),
                    strip_file: "strips/body.png".to_string(),
                },
                LongCaptureSegment {
                    index: 1,
                    output_y: 2,
                    height: 1,
                    displacement: 1,
                    confidence: 0.9,
                    frame_file: "unused.png".to_string(),
                    strip_file: "strips/tail.png".to_string(),
                },
            ],
            fixed_bottom: Some(LongCaptureFixedBottom {
                height: 1,
                file: format!("strips/{FIXED_BOTTOM_FILE}"),
            }),
        };

        let tile = compose_tile(root.path(), &manifest, 1, 3).unwrap();
        assert_eq!((tile.width, tile.height), (2, 3));
        assert_eq!(tile.rgba[0], 20);
        assert_eq!(tile.rgba[2 * 4], 90);
        assert_eq!(tile.rgba[4 * 4], 220);

        let output = root.path().join("fixed-bottom-result.png");
        stream_manifest_png(root.path(), &manifest, &output).unwrap();
        let decoded = decode_png(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 4));
        assert_eq!(decoded.rgba[0], 20);
        assert_eq!(decoded.rgba[4 * 4], 90);
        assert_eq!(decoded.rgba[6 * 4], 220);
    }

    #[test]
    fn control_window_prefers_space_outside_the_capture_roi() {
        let monitor = MonitorBounds {
            x: 0,
            y: 0,
            width: 1_920,
            height: 1_080,
            scale_factor: 1.0,
        };
        let (position, _, overlaps) = control_window_geometry(
            &monitor,
            PhysicalRect {
                x: 100,
                y: 200,
                width: 1_400,
                height: 700,
            },
        );
        assert!(!overlaps);
        assert!(position.y + CONTROL_WINDOW_HEIGHT as i32 <= 200);

        let (_, _, overlaps) = control_window_geometry(
            &monitor,
            PhysicalRect {
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
            },
        );
        assert!(overlaps);
        let full_screen_target = CaptureTarget {
            bounds: PhysicalRect {
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
            },
            scroll_anchor: PhysicalPoint { x: 960, y: 540 },
            monitor: monitor.clone(),
            mode: LongCaptureMode::Manual,
            control_overlaps_roi: true,
            scroll_windows: None,
        };
        let overlap = control_overlap_in_roi(&full_screen_target).unwrap();
        assert_eq!(overlap.y, 8);
        assert_eq!(overlap.width, CONTROL_WINDOW_MAX_WIDTH);
        assert_eq!(overlap.height, CONTROL_WINDOW_HEIGHT);

        let scaled_monitor = MonitorBounds {
            scale_factor: 1.5,
            ..monitor
        };
        let (_, size, _) = control_window_geometry(
            &scaled_monitor,
            PhysicalRect {
                x: 100,
                y: 300,
                width: 1_200,
                height: 500,
            },
        );
        assert_eq!(size.height, 102);
    }

    #[test]
    fn resource_limits_and_adaptive_wheel_delta_stay_bounded() {
        assert_eq!(maximum_height_for_width(1_000), 100_000);
        assert_eq!(maximum_height_for_width(4_000), 50_000);
        assert_eq!(maximum_height_for_width(0), 0);
        assert_eq!(calibrated_wheel_delta_units(1_000, 500, 120), 120);
        assert_eq!(calibrated_wheel_delta_units(1_000, 100, 120), 180);
        assert_eq!(calibrated_wheel_delta_units(100, 100, 120), 90);
        assert_eq!(calibrated_wheel_delta_units(1_000, 0, 400), 240);
        assert_eq!(annotation_export_strip_height(7_680), 1_024);
        assert_eq!(annotation_export_strip_height(16_000), 744);
        assert_eq!(annotation_export_strip_height(0), 1);
    }

    #[test]
    fn start_request_accepts_scope_and_all_capture_modes() {
        for mode in ["current", "top", "manual"] {
            let request: StartLongCaptureRequest = serde_json::from_value(serde_json::json!({
                "sessionId": "session",
                "selection": { "x": 10, "y": 20, "width": 640, "height": 480 },
                "scrollAnchor": { "x": 320, "y": 240 },
                "scope": "selection",
                "mode": mode,
            }))
            .unwrap();
            assert_eq!(request.session_id, "session");
            assert_eq!(request.scope, LongCaptureScope::Selection);
        }
    }

    #[test]
    fn streamed_export_writes_segments_in_manifest_order() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("strips")).unwrap();
        let first = solid_frame(2, 2, 20);
        let second = solid_frame(2, 1, 90);
        std::fs::write(
            root.path().join("strips/0.png"),
            encode_png(&first).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("strips/1.png"),
            encode_png(&second).unwrap(),
        )
        .unwrap();
        let manifest = LongCaptureManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            job_id: "job".to_string(),
            session_id: "session".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Wheel,
            selection: PhysicalRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            scroll_anchor: PhysicalPoint { x: 1, y: 1 },
            scope: LongCaptureScope::Selection,
            mode: LongCaptureMode::Current,
            width: 2,
            height: 3,
            message: "ready".to_string(),
            segments: vec![
                LongCaptureSegment {
                    index: 0,
                    output_y: 0,
                    height: 2,
                    displacement: 2,
                    confidence: 1.0,
                    frame_file: "unused.png".to_string(),
                    strip_file: "strips/0.png".to_string(),
                },
                LongCaptureSegment {
                    index: 1,
                    output_y: 2,
                    height: 1,
                    displacement: 1,
                    confidence: 0.9,
                    frame_file: "unused.png".to_string(),
                    strip_file: "strips/1.png".to_string(),
                },
            ],
            fixed_bottom: None,
        };
        let output = root.path().join("result.png");
        stream_manifest_png(root.path(), &manifest, &output).unwrap();
        let decoded = decode_png(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 3));
        assert_eq!(decoded.rgba[0], 20);
        assert_eq!(decoded.rgba[4 * 4], 90);
    }

    #[test]
    fn annotation_export_streams_bounded_uploaded_strips_without_full_save_buffer() {
        let root = tempdir().unwrap();
        let directory = root.path().join("annotation");
        std::fs::create_dir_all(&directory).unwrap();
        let ticket = LongCaptureAnnotationExportTicket {
            job: Arc::new(job_for_state(
                root.path().join("annotation-job"),
                LongCaptureState::Ready,
            )),
            job_id: "job".to_string(),
            session_id: "session".to_string(),
            action: ScreenshotExportAction::Save,
            save_path: None,
            directory,
            width: 2,
            height: ANNOTATION_EXPORT_STRIP_HEIGHT + 1,
            strip_height: ANNOTATION_EXPORT_STRIP_HEIGHT,
            next_y: ANNOTATION_EXPORT_STRIP_HEIGHT + 1,
            issued_at: Instant::now(),
        };
        atomic_write(
            &annotation_strip_path(&ticket, 0),
            &encode_png(&solid_frame(2, ANNOTATION_EXPORT_STRIP_HEIGHT, 18)).unwrap(),
        )
        .unwrap();
        atomic_write(
            &annotation_strip_path(&ticket, ANNOTATION_EXPORT_STRIP_HEIGHT),
            &encode_png(&solid_frame(2, 1, 91)).unwrap(),
        )
        .unwrap();

        let composed = compose_annotation_export_frame(&ticket).unwrap();
        assert_eq!((composed.width, composed.height), (2, 1_025));
        assert_eq!(composed.rgba[0], 18);
        assert_eq!(composed.rgba[2 * 1_024 * 4], 91);

        let output = root.path().join("annotated.png");
        stream_annotation_export_png(&ticket, &output).unwrap();
        let decoded = decode_png(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1_025));
        assert_eq!(decoded.rgba[0], 18);
        assert_eq!(decoded.rgba[2 * 1_024 * 4], 91);
    }

    #[test]
    fn annotation_payload_must_match_dimensions_and_never_drops_annotations_silently() {
        let manifest = LongCaptureManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            job_id: "job".to_string(),
            session_id: "session".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Wheel,
            selection: PhysicalRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            scroll_anchor: PhysicalPoint { x: 100, y: 100 },
            scope: LongCaptureScope::Selection,
            mode: LongCaptureMode::Current,
            width: 800,
            height: 2_400,
            message: "ready".to_string(),
            segments: Vec::new(),
            fixed_bottom: None,
        };
        let empty = serde_json::json!({
            "version": 1,
            "coordinateSpace": "longImagePixels",
            "width": 800,
            "height": 2400,
            "annotations": [],
            "strips": [],
        });
        assert!(validate_annotation_payload(Some(&empty), &manifest).is_ok());
        let annotated = serde_json::json!({
            "coordinateSpace": "longImagePixels",
            "width": 800,
            "height": 2400,
            "annotations": [{ "id": "shape-1", "kind": "shape" }],
        });
        let error = validate_annotation_payload(Some(&annotated), &manifest).unwrap_err();
        assert_eq!(error.code, "long_capture_annotation_export_unsupported");
    }

    #[test]
    fn browser_process_detection_accepts_only_supported_executables() {
        assert_eq!(
            classify_browser_executable(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            Some(BrowserFamily::Chrome)
        );
        assert_eq!(
            classify_browser_executable(r"C:\Program Files (x86)\Microsoft\Edge\msedge.exe"),
            Some(BrowserFamily::Edge)
        );
        assert_eq!(
            classify_browser_executable(r"C:\Program Files\Mozilla Firefox\FIREFOX.EXE"),
            Some(BrowserFamily::Firefox)
        );
        assert_eq!(
            classify_browser_executable(r"C:\Windows\explorer.exe"),
            None
        );
    }

    #[test]
    fn browser_step_response_preserves_bottom_and_actual_movement() {
        let step = parse_browser_step(&serde_json::json!({
            "state": "capturing",
            "moved": true,
            "actualDistance": 416.5,
            "atBottom": true,
        }));
        assert_eq!(
            step,
            BrowserStepResult {
                moved: true,
                at_bottom: true,
                actual_distance: 416.5,
            }
        );
    }

    #[test]
    fn browser_binding_and_payload_keep_tab_frame_session_and_dpr_fixed() {
        let response = serde_json::json!({
            "tabId": 17,
            "frameId": 3,
            "sessionId": "capture-session-1",
            "devicePixelRatio": 1.5,
            "state": "prepared",
        });
        let binding = parse_browser_session_binding(&response).unwrap();
        assert_eq!(
            binding,
            BrowserSessionBinding {
                tab_id: 17,
                frame_id: 3,
                session_id: "capture-session-1".to_string(),
                device_pixel_ratio: 1.5,
            }
        );
        let payload = browser_payload(&binding, serde_json::json!({ "distancePx": 260 })).unwrap();
        assert_eq!(payload["tabId"], 17);
        assert_eq!(payload["frameId"], 3);
        assert_eq!(payload["sessionId"], "capture-session-1");
        assert_eq!(payload["distancePx"], 260);
        validate_browser_response_binding(&response, &binding).unwrap();

        let changed = serde_json::json!({
            "tabId": 18,
            "frameId": 3,
            "sessionId": "capture-session-1",
            "devicePixelRatio": 1.5,
        });
        assert!(validate_browser_response_binding(&changed, &binding).is_err());
    }

    #[test]
    fn browser_step_distance_uses_physical_selection_and_preserves_overlap() {
        let distance = browser_step_distance_css(600, 1.5).unwrap();
        assert_eq!(distance, 260);
        let physical_movement = f64::from(distance) * 1.5;
        assert!(physical_movement <= 600.0 * (1.0 - BROWSER_MIN_OVERLAP_RATIO));
        assert!(browser_step_distance_css(1, 16.0).is_err());
        assert!(browser_step_distance_css(600, f64::NAN).is_err());
    }

    #[test]
    fn top_scroll_bound_never_silently_accepts_continued_motion() {
        assert!(!top_scroll_attempt_complete(TOP_SCROLL_MAX_ATTEMPTS - 1, 0).unwrap());
        assert!(top_scroll_attempt_complete(3, TOP_SCROLL_NO_MOTION_CONFIRMATIONS).unwrap());
        let error = top_scroll_attempt_complete(TOP_SCROLL_MAX_ATTEMPTS, 0).unwrap_err();
        assert_eq!(error.code, "long_capture_top_not_reached");
    }

    #[test]
    fn copy_and_pin_limit_is_bounded_but_streamed_save_limit_is_independent() {
        validate_copy_pin_dimensions(4_000, 4_000).unwrap();
        let too_large = validate_copy_pin_dimensions(4_000, 4_001).unwrap_err();
        assert_eq!(too_large.code, "long_capture_clipboard_limit");
        assert!(MAX_LONG_PIXELS > MAX_COPY_PIN_PIXELS);
    }

    #[test]
    fn startup_and_shutdown_cache_cleanup_respect_live_process_owners() {
        let root = tempdir().unwrap();
        let orphan = root.path().join("orphan-job");
        let live = root.path().join("live-job");
        let orphan_export = root
            .path()
            .join(".annotation-exports")
            .join("orphan-export");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&orphan_export).unwrap();
        write_cache_owner(&live).unwrap();

        cleanup_orphaned_capture_directories(root.path());
        assert!(!orphan.exists());
        assert!(!orphan_export.exists());
        assert!(live.exists());

        cleanup_cache_directories_owned_by(root.path(), std::process::id());
        assert!(!live.exists());
    }

    #[test]
    fn clearing_a_completed_job_removes_job_and_annotation_caches() {
        let root = tempdir().unwrap();
        let job_directory = root.path().join("job-1");
        let export_directory = root.path().join(".annotation-exports").join("export-1");
        std::fs::create_dir_all(&job_directory).unwrap();
        std::fs::create_dir_all(&export_directory).unwrap();
        let manifest = LongCaptureManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            job_id: "job-1".to_string(),
            session_id: "session-1".to_string(),
            state: LongCaptureState::Ready,
            engine: LongCaptureEngine::Wheel,
            selection: PhysicalRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            scroll_anchor: PhysicalPoint { x: 5, y: 5 },
            scope: LongCaptureScope::Selection,
            mode: LongCaptureMode::Current,
            width: 10,
            height: 20,
            message: "ready".to_string(),
            segments: Vec::new(),
            fixed_bottom: None,
        };
        let restore_binding = BrowserSessionBinding {
            tab_id: 17,
            frame_id: 0,
            session_id: "browser-session".to_string(),
            device_pixel_ratio: 1.25,
        };
        let mut runtime = LongCaptureRuntime {
            manifest,
            pause_requested: false,
            cancel_requested: false,
            finish_requested: true,
            retry_current: false,
            generation: 0,
            worker_done: true,
            ready_emitted: true,
            browser_active: true,
            browser_restore_needed: true,
            browser_session: Some(restore_binding.clone()),
        };
        assert_eq!(claim_browser_restore(&mut runtime), Some(restore_binding));
        assert_eq!(claim_browser_restore(&mut runtime), None);
        let job = Arc::new(LongCaptureJob {
            directory: job_directory.clone(),
            target: CaptureTarget {
                bounds: PhysicalRect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                scroll_anchor: PhysicalPoint { x: 5, y: 5 },
                monitor: MonitorBounds {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                    scale_factor: 1.0,
                },
                mode: LongCaptureMode::Current,
                control_overlaps_roi: false,
                scroll_windows: None,
            },
            control_window_instance: None,
            hidden_pin_labels: Vec::new(),
            browser: None,
            operation_lock: Mutex::new(()),
            runtime: Mutex::new(runtime),
            wake: Condvar::new(),
        });
        let store = test_store(root.path().to_path_buf());
        *lock_unpoisoned(&store.job) = Some(Arc::clone(&job));
        *lock_unpoisoned(&store.annotation_exports) = HashMap::from([(
            "export-1".to_string(),
            LongCaptureAnnotationExportTicket {
                job,
                job_id: "job-1".to_string(),
                session_id: "session-1".to_string(),
                action: ScreenshotExportAction::Save,
                save_path: None,
                directory: export_directory.clone(),
                width: 10,
                height: 20,
                strip_height: 10,
                next_y: 0,
                issued_at: Instant::now(),
            },
        )]);

        clear_job_cache(&store, "job-1");
        assert!(lock_unpoisoned(&store.job).is_none());
        assert!(lock_unpoisoned(&store.annotation_exports).is_empty());
        assert!(!job_directory.exists());
        assert!(!export_directory.exists());
    }

    #[test]
    fn pending_start_cancellation_is_scoped_to_its_screenshot_session() {
        let root = tempdir().unwrap();
        let store = test_store(root.path().to_path_buf());

        store.begin_pending_start("session-1").unwrap();
        let duplicate = store.begin_pending_start("session-2").unwrap_err();
        assert_eq!(duplicate.code, "long_capture_busy");
        assert!(!store.request_pending_start_cancel("session-2"));
        assert!(!store.pending_start_cancel_requested("session-1"));
        assert!(store.request_pending_start_cancel("session-1"));
        assert!(store.pending_start_cancel_requested("session-1"));

        store.finish_pending_start("session-2");
        assert!(store.pending_start_cancel_requested("session-1"));
        store.finish_pending_start("session-1");
        assert!(!store.pending_start_cancel_requested("session-1"));
    }

    #[test]
    fn stale_session_and_job_ids_cannot_remove_the_current_job() {
        let root = tempdir().unwrap();
        let job_directory = root.path().join("current-job");
        let export_directory = root.path().join("current-export");
        std::fs::create_dir_all(&job_directory).unwrap();
        std::fs::create_dir_all(&export_directory).unwrap();
        let job = Arc::new(job_for_state(
            job_directory.clone(),
            LongCaptureState::Ready,
        ));
        let store = test_store(root.path().to_path_buf());
        *lock_unpoisoned(&store.job) = Some(Arc::clone(&job));
        lock_unpoisoned(&store.annotation_exports).insert(
            "current-export".to_string(),
            LongCaptureAnnotationExportTicket {
                job: Arc::clone(&job),
                job_id: "job".to_string(),
                session_id: "session".to_string(),
                action: ScreenshotExportAction::Save,
                save_path: None,
                directory: export_directory.clone(),
                width: 1,
                height: 1,
                strip_height: 1,
                next_y: 0,
                issued_at: Instant::now(),
            },
        );

        assert!(store.job_for_session("session-old").is_none());
        assert!(Arc::ptr_eq(
            &store.job_for_session("session").unwrap(),
            &job
        ));
        clear_job_cache(&store, "job-old");
        assert!(Arc::ptr_eq(
            lock_unpoisoned(&store.job).as_ref().unwrap(),
            &job
        ));
        assert!(lock_unpoisoned(&store.annotation_exports).contains_key("current-export"));
        assert!(job_directory.is_dir());
        assert!(export_directory.is_dir());
    }

    #[test]
    fn stale_job_reference_is_rejected_after_replacement() {
        let root = tempdir().unwrap();
        let old_job = Arc::new(job_for_state(
            root.path().join("old-job"),
            LongCaptureState::Ready,
        ));
        let new_job = Arc::new(job_for_state(
            root.path().join("new-job"),
            LongCaptureState::Capturing,
        ));
        let store = test_store(root.path().to_path_buf());

        *lock_unpoisoned(&store.job) = Some(Arc::clone(&old_job));
        assert!(store.ensure_current_job(&old_job).is_ok());
        *lock_unpoisoned(&store.job) = Some(Arc::clone(&new_job));

        assert_eq!(
            store.ensure_current_job(&old_job).unwrap_err().code,
            "not_found"
        );
        assert!(store.ensure_current_job(&new_job).is_ok());
    }

    #[test]
    fn expired_annotation_export_is_removed_before_job_replacement() {
        let root = tempdir().unwrap();
        let export_directory = root.path().join("expired-export");
        std::fs::create_dir_all(&export_directory).unwrap();
        let job = Arc::new(job_for_state(
            root.path().join("ready-job"),
            LongCaptureState::Ready,
        ));
        let store = test_store(root.path().to_path_buf());
        lock_unpoisoned(&store.annotation_exports).insert(
            "expired".to_string(),
            LongCaptureAnnotationExportTicket {
                job,
                job_id: "ready-job".to_string(),
                session_id: "session".to_string(),
                action: ScreenshotExportAction::Save,
                save_path: None,
                directory: export_directory.clone(),
                width: 1,
                height: 1,
                strip_height: 1,
                next_y: 0,
                issued_at: Instant::now() - ANNOTATION_EXPORT_TICKET_TTL - Duration::from_secs(1),
            },
        );

        purge_expired_annotation_exports(&store);

        assert!(lock_unpoisoned(&store.annotation_exports).is_empty());
        assert!(!export_directory.exists());
    }
}

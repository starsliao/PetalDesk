use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

const LOG_DIRECTORY: &str = "logs";
const LOG_FILE: &str = "petaldesk-diagnostics.log";
const MAX_LOG_BYTES: u64 = 1024 * 1024;
const LOG_BACKUPS: usize = 3;
const LOG_QUEUE_CAPACITY: usize = 256;
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(5);
const STALL_THRESHOLD_MS: u64 = 15_000;
const STALL_REPORT_INTERVAL_MS: u64 = 60_000;
const PROCESS_SAMPLE_INTERVAL_MS: u64 = 60_000;
const WAIT_CHAIN_TIMEOUT_MS: u64 = 5_000;

static LOGGER: OnceLock<DiagnosticsLogger> = OnceLock::new();
static DROPPED_RECORDS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_STARTED: AtomicBool = AtomicBool::new(false);
static WATCHDOG_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
static MAIN_LOOP_PROGRESS_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static MAIN_LOOP_PROBE_PROGRESS_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static MAIN_LOOP_PROBE_SENT_MS: AtomicU64 = AtomicU64::new(0);
static MAIN_LOOP_PROBE_PENDING: AtomicBool = AtomicBool::new(false);
static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static MAIN_PHASE: AtomicU8 = AtomicU8::new(MainPhase::Startup as u8);

#[cfg(windows)]
static WAIT_CHAIN_SENDER: OnceLock<Option<SyncSender<WaitChainRequest>>> = OnceLock::new();
#[cfg(windows)]
static WAIT_CHAIN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(windows)]
static WAIT_CHAIN_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static WAIT_CHAIN_STARTED_MS: AtomicU64 = AtomicU64::new(0);
#[cfg(windows)]
static WAIT_CHAIN_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(0);
#[cfg(windows)]
static WAIT_CHAIN_TIMEOUT_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticEvent {
    LoggerStarted,
    PrimaryInstanceConfirmed,
    StoresLoaded,
    AppBuilt,
    SetupStarted,
    BrowserServiceStarted,
    TrayBuilt,
    SetupCompleted,
    RunReady,
    RunResumed,
    ExitRequested,
    Exit,
    MainPageLoaded,
    ActivationRequested,
    ActivationCoalesced,
    ActivationFallback,
    ActivationWorkerStarted,
    ActivationWorkerFinished,
    ActivationAutostartSuppressed,
    NoteWindowCreationRequested,
    NoteWindowCreationLockAcquired,
    NoteWindowExisting,
    NoteWindowExistingShown,
    NoteWindowSnapshotStarted,
    NoteWindowSnapshotLoaded,
    NoteWindowBuildStarted,
    NoteWindowBuilt,
    NoteWindowStateSaved,
    TrayDoubleClick,
    TrayMenuShow,
    TrayMenuNewNote,
    TrayMenuAbout,
    TrayMenuQuit,
    TrayMenuOpenTool,
    TrayMenuOpenNote,
    TrayRefreshStarted,
    TrayRefreshSkipped,
    TrayRefreshCompleted,
    TrayRefreshFailed,
    WindowCloseRequested,
    WindowDestroyed,
    MainLoopStall,
    MainLoopStallSnapshot,
    MainLoopWaitChain,
    MainLoopRecovered,
    MainLoopProbeDispatchFailed,
    WaitChainCaptureTimedOut,
    WaitChainWorkerUnavailable,
    ProcessSample,
    Panic,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub(crate) enum MainPhase {
    Startup = 0,
    Setup = 1,
    Idle = 2,
    PageLoad = 3,
    TrayMenu = 4,
    TrayIcon = 5,
    WindowEvent = 6,
    Exit = 7,
    SingleInstance = 8,
}

impl MainPhase {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Setup,
            2 => Self::Idle,
            3 => Self::PageLoad,
            4 => Self::TrayMenu,
            5 => Self::TrayIcon,
            6 => Self::WindowEvent,
            7 => Self::Exit,
            8 => Self::SingleInstance,
            _ => Self::Startup,
        }
    }
}

pub(crate) struct MainPhaseGuard {
    previous: u8,
}

impl Drop for MainPhaseGuard {
    fn drop(&mut self) {
        MAIN_PHASE.store(self.previous, Ordering::Release);
    }
}

#[derive(Debug, Default, Serialize)]
struct DiagnosticFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stall_age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<MainPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<RuntimeSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_chain: Option<WaitChainSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic: Option<PanicSnapshot>,
}

#[derive(Debug, Serialize)]
struct PanicSnapshot {
    thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_column: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
struct RuntimeSnapshot {
    pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_console_session_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_session: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gdi_objects: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_objects: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_set_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    private_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_cpu_ms: Option<u64>,
    main_thread_id: u32,
}

#[derive(Debug, Serialize)]
struct WaitChainSnapshot {
    available: bool,
    cycle: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<u32>,
    nodes: Vec<WaitChainNode>,
}

#[cfg(windows)]
struct WaitChainRequest {
    snapshot_id: u64,
    thread_id: u32,
    stall_age_ms: u64,
    phase: MainPhase,
    started_ms: u64,
}

#[derive(Debug, Serialize)]
struct WaitChainNode {
    object_type: &'static str,
    object_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<u32>,
}

struct DiagnosticRecord {
    timestamp: SystemTime,
    level: DiagnosticLevel,
    event: DiagnosticEvent,
    fields: DiagnosticFields,
    dropped_records: u64,
}

struct LogMessage {
    record: DiagnosticRecord,
    acknowledge: Option<mpsc::Sender<bool>>,
}

struct DiagnosticsLogger {
    sender: SyncSender<LogMessage>,
    path: PathBuf,
}

#[derive(Serialize)]
struct SerializableRecord<'a> {
    timestamp: String,
    level: DiagnosticLevel,
    event: DiagnosticEvent,
    version: &'static str,
    run_id: &'a str,
    pid: u32,
    #[serde(skip_serializing_if = "is_zero")]
    dropped_records: u64,
    #[serde(skip_serializing_if = "is_zero")]
    write_failures: u64,
    #[serde(flatten)]
    fields: &'a DiagnosticFields,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn init() -> Option<PathBuf> {
    if let Some(logger) = LOGGER.get() {
        return Some(logger.path.clone());
    }

    #[cfg(windows)]
    MAIN_THREAD_ID.store(
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
        Ordering::Release,
    );

    let directory = dirs::data_local_dir()?
        .join("PetalDesk")
        .join(LOG_DIRECTORY);
    let path = directory.join(LOG_FILE);
    let (sender, receiver) = mpsc::sync_channel(LOG_QUEUE_CAPACITY);
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let run_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let writer_path = path.clone();
    let writer_run_id = run_id.clone();
    std::thread::Builder::new()
        .name("petaldesk-diagnostics-writer".to_string())
        .spawn(move || writer_loop(receiver, writer_path, writer_run_id, ready_sender))
        .ok()?;
    match ready_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return None,
    }

    let logger = DiagnosticsLogger {
        sender,
        path: path.clone(),
    };
    if LOGGER.set(logger).is_err() {
        return LOGGER.get().map(|logger| logger.path.clone());
    }

    install_panic_hook();
    Some(path)
}

pub(crate) fn record(event: DiagnosticEvent) {
    enqueue(
        DiagnosticLevel::Info,
        event,
        DiagnosticFields::default(),
        false,
    );
}

pub(crate) fn record_warning(event: DiagnosticEvent) {
    enqueue(
        DiagnosticLevel::Warning,
        event,
        DiagnosticFields::default(),
        false,
    );
}

pub(crate) fn record_urgent(event: DiagnosticEvent) {
    enqueue(
        DiagnosticLevel::Info,
        event,
        DiagnosticFields::default(),
        true,
    );
}

pub(crate) fn record_duration(event: DiagnosticEvent, duration: Duration) {
    enqueue(
        DiagnosticLevel::Info,
        event,
        DiagnosticFields {
            duration_ms: Some(duration.as_millis().min(u128::from(u64::MAX)) as u64),
            ..DiagnosticFields::default()
        },
        false,
    );
}

pub(crate) fn enter_main_phase(phase: MainPhase) -> MainPhaseGuard {
    let previous = MAIN_PHASE.swap(phase as u8, Ordering::AcqRel);
    MainPhaseGuard { previous }
}

pub(crate) fn set_main_phase(phase: MainPhase) {
    MAIN_PHASE.store(phase as u8, Ordering::Release);
}

pub(crate) fn mark_main_loop_progress() {
    MAIN_LOOP_PROGRESS_SEQUENCE.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn start_main_loop_watchdog(app: AppHandle) {
    if WATCHDOG_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    mark_main_loop_progress();
    initialize_wait_chain_worker();
    if std::thread::Builder::new()
        .name("petaldesk-main-loop-watchdog".to_string())
        .spawn(move || watchdog_loop(app))
        .is_err()
    {
        WATCHDOG_STARTED.store(false, Ordering::Release);
        record_warning(DiagnosticEvent::MainLoopProbeDispatchFailed);
    }
}

fn watchdog_loop(app: AppHandle) {
    let mut state = WatchdogState::default();
    let mut last_process_sample_ms = monotonic_millis();
    record_process_sample();
    loop {
        std::thread::sleep(WATCHDOG_INTERVAL);
        let now_ms = monotonic_millis();
        let probe_pending = MAIN_LOOP_PROBE_PENDING.load(Ordering::Acquire);
        if probe_pending {
            let current_progress = MAIN_LOOP_PROGRESS_SEQUENCE.load(Ordering::Acquire);
            let probe_progress = MAIN_LOOP_PROBE_PROGRESS_SEQUENCE.load(Ordering::Acquire);
            if current_progress != probe_progress {
                // Any Tauri callback proves that the message pump is still
                // moving, even if a queued probe is delayed by message volume.
                MAIN_LOOP_PROBE_PROGRESS_SEQUENCE.store(current_progress, Ordering::Release);
                MAIN_LOOP_PROBE_SENT_MS.store(now_ms, Ordering::Release);
            }
        }
        let probe_sent_ms = MAIN_LOOP_PROBE_SENT_MS.load(Ordering::Acquire);

        match state.evaluate(now_ms, probe_sent_ms, probe_pending) {
            WatchdogAction::None => {}
            WatchdogAction::ReportStall { age_ms } => record_main_loop_stall(age_ms),
            WatchdogAction::Recovered { duration_ms } => enqueue(
                DiagnosticLevel::Info,
                DiagnosticEvent::MainLoopRecovered,
                DiagnosticFields {
                    duration_ms: Some(duration_ms),
                    runtime: Some(runtime_snapshot()),
                    ..DiagnosticFields::default()
                },
                false,
            ),
        }

        if now_ms.saturating_sub(last_process_sample_ms) >= PROCESS_SAMPLE_INTERVAL_MS {
            record_process_sample();
            last_process_sample_ms = now_ms;
        }

        maybe_record_wait_chain_timeout(now_ms);

        if MAIN_LOOP_PROBE_PENDING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            MAIN_LOOP_PROBE_SENT_MS.store(now_ms, Ordering::Release);
            MAIN_LOOP_PROBE_PROGRESS_SEQUENCE.store(
                MAIN_LOOP_PROGRESS_SEQUENCE.load(Ordering::Acquire),
                Ordering::Release,
            );
            if app
                .run_on_main_thread(|| {
                    mark_main_loop_progress();
                    MAIN_LOOP_PROBE_PENDING.store(false, Ordering::Release);
                })
                .is_err()
            {
                MAIN_LOOP_PROBE_PENDING.store(false, Ordering::Release);
                record_warning(DiagnosticEvent::MainLoopProbeDispatchFailed);
            }
        }
    }
}

fn record_main_loop_stall(age_ms: u64) {
    let main_thread_id = MAIN_THREAD_ID.load(Ordering::Acquire);
    let phase = MainPhase::from_u8(MAIN_PHASE.load(Ordering::Acquire));
    enqueue(
        DiagnosticLevel::Error,
        DiagnosticEvent::MainLoopStall,
        DiagnosticFields {
            stall_age_ms: Some(age_ms),
            phase: Some(phase),
            ..DiagnosticFields::default()
        },
        true,
    );
    enqueue(
        DiagnosticLevel::Error,
        DiagnosticEvent::MainLoopStallSnapshot,
        DiagnosticFields {
            stall_age_ms: Some(age_ms),
            phase: Some(phase),
            runtime: Some(runtime_snapshot()),
            ..DiagnosticFields::default()
        },
        false,
    );
    request_wait_chain_snapshot(main_thread_id, age_ms, phase);
}

#[cfg(windows)]
fn initialize_wait_chain_worker() {
    let sender = WAIT_CHAIN_SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("petaldesk-wct-worker".to_string())
            .spawn(move || wait_chain_worker_loop(receiver))
        {
            Ok(worker) => {
                // A broken WCT provider must never make application shutdown
                // wait for this diagnostic-only thread.
                drop(worker);
                Some(sender)
            }
            Err(_) => None,
        }
    });
    if sender.is_none() {
        record_warning(DiagnosticEvent::WaitChainWorkerUnavailable);
    }
}

#[cfg(not(windows))]
fn initialize_wait_chain_worker() {}

#[cfg(windows)]
fn request_wait_chain_snapshot(thread_id: u32, stall_age_ms: u64, phase: MainPhase) {
    let Some(sender) = WAIT_CHAIN_SENDER.get().and_then(Option::as_ref) else {
        return;
    };
    if WAIT_CHAIN_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let snapshot_id = WAIT_CHAIN_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1;
    let started_ms = monotonic_millis();
    WAIT_CHAIN_STARTED_MS.store(started_ms, Ordering::Release);
    WAIT_CHAIN_SNAPSHOT_ID.store(snapshot_id, Ordering::Release);
    WAIT_CHAIN_TIMEOUT_REPORTED.store(false, Ordering::Release);
    let request = WaitChainRequest {
        snapshot_id,
        thread_id,
        stall_age_ms,
        phase,
        started_ms,
    };
    if sender.try_send(request).is_err() {
        WAIT_CHAIN_IN_FLIGHT.store(false, Ordering::Release);
        record_warning(DiagnosticEvent::WaitChainWorkerUnavailable);
    }
}

#[cfg(not(windows))]
fn request_wait_chain_snapshot(_thread_id: u32, _stall_age_ms: u64, _phase: MainPhase) {}

#[cfg(windows)]
fn wait_chain_worker_loop(receiver: Receiver<WaitChainRequest>) {
    while let Ok(request) = receiver.recv() {
        let wait_chain = wait_chain_snapshot(request.thread_id);
        let duration_ms = monotonic_millis().saturating_sub(request.started_ms);
        enqueue(
            DiagnosticLevel::Error,
            DiagnosticEvent::MainLoopWaitChain,
            DiagnosticFields {
                duration_ms: Some(duration_ms),
                stall_age_ms: Some(request.stall_age_ms),
                snapshot_id: Some(request.snapshot_id),
                phase: Some(request.phase),
                wait_chain: Some(wait_chain),
                ..DiagnosticFields::default()
            },
            false,
        );
        WAIT_CHAIN_IN_FLIGHT.store(false, Ordering::Release);
    }
}

#[cfg(windows)]
fn maybe_record_wait_chain_timeout(now_ms: u64) {
    let started_ms = WAIT_CHAIN_STARTED_MS.load(Ordering::Acquire);
    if wait_chain_timeout_due(
        now_ms,
        started_ms,
        WAIT_CHAIN_IN_FLIGHT.load(Ordering::Acquire),
        WAIT_CHAIN_TIMEOUT_REPORTED.load(Ordering::Acquire),
    ) && WAIT_CHAIN_TIMEOUT_REPORTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        enqueue(
            DiagnosticLevel::Warning,
            DiagnosticEvent::WaitChainCaptureTimedOut,
            DiagnosticFields {
                duration_ms: Some(now_ms.saturating_sub(started_ms)),
                snapshot_id: Some(WAIT_CHAIN_SNAPSHOT_ID.load(Ordering::Acquire)),
                ..DiagnosticFields::default()
            },
            false,
        );
    }
}

#[cfg(not(windows))]
fn maybe_record_wait_chain_timeout(_now_ms: u64) {}

fn wait_chain_timeout_due(
    now_ms: u64,
    started_ms: u64,
    in_flight: bool,
    already_reported: bool,
) -> bool {
    in_flight && !already_reported && now_ms.saturating_sub(started_ms) >= WAIT_CHAIN_TIMEOUT_MS
}

fn record_process_sample() {
    enqueue(
        DiagnosticLevel::Info,
        DiagnosticEvent::ProcessSample,
        DiagnosticFields {
            runtime: Some(runtime_snapshot()),
            ..DiagnosticFields::default()
        },
        false,
    );
}

fn monotonic_millis() -> u64 {
    WATCHDOG_EPOCH
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Default)]
struct WatchdogState {
    stalled_since_ms: Option<u64>,
    last_report_ms: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum WatchdogAction {
    None,
    ReportStall { age_ms: u64 },
    Recovered { duration_ms: u64 },
}

impl WatchdogState {
    fn evaluate(&mut self, now_ms: u64, probe_sent_ms: u64, probe_pending: bool) -> WatchdogAction {
        let age_ms = now_ms.saturating_sub(probe_sent_ms);
        if probe_pending && age_ms >= STALL_THRESHOLD_MS {
            let stalled_since_ms = *self.stalled_since_ms.get_or_insert(probe_sent_ms);
            let should_report = self
                .last_report_ms
                .is_none_or(|reported| now_ms.saturating_sub(reported) >= STALL_REPORT_INTERVAL_MS);
            if should_report {
                self.last_report_ms = Some(now_ms);
                return WatchdogAction::ReportStall {
                    age_ms: now_ms.saturating_sub(stalled_since_ms),
                };
            }
            return WatchdogAction::None;
        }

        if let Some(stalled_since_ms) = self.stalled_since_ms.take() {
            self.last_report_ms = None;
            return WatchdogAction::Recovered {
                duration_ms: now_ms.saturating_sub(stalled_since_ms),
            };
        }
        WatchdogAction::None
    }
}

fn enqueue(
    level: DiagnosticLevel,
    event: DiagnosticEvent,
    fields: DiagnosticFields,
    wait_for_write: bool,
) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let dropped_records = DROPPED_RECORDS.swap(0, Ordering::AcqRel);
    let (acknowledge, wait) = if wait_for_write {
        let (sender, receiver) = mpsc::channel();
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let message = LogMessage {
        record: DiagnosticRecord {
            timestamp: SystemTime::now(),
            level,
            event,
            fields,
            dropped_records,
        },
        acknowledge,
    };
    match logger.sender.try_send(message) {
        Ok(()) => {
            if let Some(wait) = wait {
                let _ = wait.recv_timeout(Duration::from_millis(250));
            }
        }
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            DROPPED_RECORDS.fetch_add(dropped_records.saturating_add(1), Ordering::AcqRel);
        }
    }
}

fn writer_loop(
    receiver: Receiver<LogMessage>,
    path: PathBuf,
    run_id: String,
    ready: SyncSender<io::Result<()>>,
) {
    let mut writer = match RotatingWriter::new(path, MAX_LOG_BYTES, LOG_BACKUPS) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let startup_fields = DiagnosticFields::default();
    let startup = SerializableRecord {
        timestamp: timestamp_string(SystemTime::now()),
        level: DiagnosticLevel::Info,
        event: DiagnosticEvent::LoggerStarted,
        version: env!("CARGO_PKG_VERSION"),
        run_id: &run_id,
        pid: std::process::id(),
        dropped_records: 0,
        write_failures: 0,
        fields: &startup_fields,
    };
    if let Err(error) = writer.write_json_line(&startup) {
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(()));

    let mut write_failures = 0_u64;
    while let Ok(message) = receiver.recv() {
        let record = SerializableRecord {
            timestamp: timestamp_string(message.record.timestamp),
            level: message.record.level,
            event: message.record.event,
            version: env!("CARGO_PKG_VERSION"),
            run_id: &run_id,
            pid: std::process::id(),
            dropped_records: message.record.dropped_records,
            write_failures,
            fields: &message.record.fields,
        };
        let written = writer.write_json_line(&record).is_ok();
        if !written {
            write_failures = write_failures.saturating_add(1);
            let _ = writer.reopen();
        } else {
            write_failures = 0;
        }
        if let Some(acknowledge) = message.acknowledge {
            let _ = acknowledge.send(written);
        }
    }
}

fn timestamp_string(timestamp: SystemTime) -> String {
    let timestamp: chrono::DateTime<chrono::Utc> = timestamp.into();
    timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location();
        let source_file = location.and_then(|location| source_file_name(location.file()));
        let panic = PanicSnapshot {
            thread_id: format!("{:?}", std::thread::current().id()),
            source_file,
            source_line: location.map(|location| location.line()),
            source_column: location.map(|location| location.column()),
        };
        enqueue(
            DiagnosticLevel::Error,
            DiagnosticEvent::Panic,
            DiagnosticFields {
                panic: Some(panic),
                ..DiagnosticFields::default()
            },
            true,
        );
        previous(info);
    }));
}

fn source_file_name(path: &str) -> Option<String> {
    path.rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .map(ToOwned::to_owned)
}

struct RotatingWriter {
    path: PathBuf,
    max_bytes: u64,
    backups: usize,
    file: Option<File>,
    bytes_written: u64,
}

impl RotatingWriter {
    fn new(path: PathBuf, max_bytes: u64, backups: usize) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= max_bytes) {
            rotate_files(&path, backups)?;
        }
        let file = open_log_file(&path)?;
        let bytes_written = file.metadata()?.len();
        Ok(Self {
            path,
            max_bytes,
            backups,
            file: Some(file),
            bytes_written,
        })
    }

    fn write_json_line<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
        line.push(b'\n');
        if self.bytes_written > 0
            && self.bytes_written.saturating_add(line.len() as u64) > self.max_bytes
        {
            self.file.take();
            if let Err(error) = rotate_files(&self.path, self.backups) {
                self.reopen()?;
                return Err(error);
            }
            self.file = Some(open_log_file(&self.path)?);
            self.bytes_written = 0;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("diagnostics log is not open"))?;
        if let Err(error) = file.write_all(&line).and_then(|()| file.flush()) {
            // A failed append may leave a partial JSON object at EOF. Restore
            // the last committed length before the next record is attempted.
            let _ = file.set_len(self.bytes_written);
            return Err(error);
        }
        self.bytes_written = self.bytes_written.saturating_add(line.len() as u64);
        Ok(())
    }

    fn reopen(&mut self) -> io::Result<()> {
        self.file.take();
        let file = open_log_file(&self.path)?;
        self.bytes_written = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }
}

fn open_log_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotate_files(path: &Path, backups: usize) -> io::Result<()> {
    if backups == 0 {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    for index in (1..=backups).rev() {
        let target = rotated_log_path(path, index);
        if target.exists() {
            fs::remove_file(&target)?;
        }
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            rotated_log_path(path, index - 1)
        };
        if source.exists() {
            fs::rename(source, target)?;
        }
    }
    Ok(())
}

fn rotated_log_path(path: &Path, index: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("petaldesk-diagnostics");
    let extension = path.extension().and_then(|value| value.to_str());
    let name = match extension {
        Some(extension) => format!("{stem}.{index}.{extension}"),
        None => format!("{stem}.{index}"),
    };
    path.with_file_name(name)
}

#[cfg(windows)]
fn runtime_snapshot() -> RuntimeSnapshot {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSGetActiveConsoleSessionId,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GetProcessHandleCount, GetProcessTimes, GR_GDIOBJECTS,
        GR_USEROBJECTS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};

    let pid = std::process::id();
    let process = unsafe { GetCurrentProcess() };
    let mut session_id = 0_u32;
    let session_id =
        (unsafe { ProcessIdToSessionId(pid, &mut session_id) } != 0).then_some(session_id);
    let active_console = unsafe { WTSGetActiveConsoleSessionId() };
    let active_console_session_id = (active_console != u32::MAX).then_some(active_console);
    let remote_session = Some(unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0);

    let mut handle_count = 0_u32;
    let handle_count =
        (unsafe { GetProcessHandleCount(process, &mut handle_count) } != 0).then_some(handle_count);
    let gdi_objects = Some(unsafe { GetGuiResources(process, GR_GDIOBJECTS) });
    let user_objects = Some(unsafe { GetGuiResources(process, GR_USEROBJECTS) });

    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..PROCESS_MEMORY_COUNTERS_EX::default()
    };
    let memory_available = unsafe {
        K32GetProcessMemoryInfo(
            process,
            &mut memory as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    } != 0;

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let times_available =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    let process_cpu_ms = times_available
        .then(|| (filetime_ticks(kernel).saturating_add(filetime_ticks(user))) / 10_000);

    let mut thread_count = None;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot != INVALID_HANDLE_VALUE {
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut count = 0_u32;
        let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while has_entry {
            if entry.th32OwnerProcessID == pid {
                count = count.saturating_add(1);
            }
            has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        thread_count = Some(count);
    }

    RuntimeSnapshot {
        pid,
        session_id,
        active_console_session_id,
        remote_session,
        handle_count,
        thread_count,
        gdi_objects,
        user_objects,
        working_set_bytes: memory_available.then_some(memory.WorkingSetSize as u64),
        private_bytes: memory_available.then_some(memory.PrivateUsage as u64),
        process_cpu_ms,
        main_thread_id: MAIN_THREAD_ID.load(Ordering::Acquire),
    }
}

#[cfg(windows)]
fn filetime_ticks(value: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(not(windows))]
fn runtime_snapshot() -> RuntimeSnapshot {
    RuntimeSnapshot {
        pid: std::process::id(),
        main_thread_id: 0,
        ..RuntimeSnapshot::default()
    }
}

#[cfg(windows)]
fn wait_chain_snapshot(thread_id: u32) -> WaitChainSnapshot {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        CloseThreadWaitChainSession, GetThreadWaitChain, OpenThreadWaitChainSession, WctThreadType,
        WAITCHAIN_NODE_INFO, WCT_MAX_NODE_COUNT, WCT_OUT_OF_PROC_FLAG,
    };

    if thread_id == 0 {
        return WaitChainSnapshot {
            available: false,
            cycle: false,
            error_code: None,
            nodes: Vec::new(),
        };
    }
    let session = unsafe { OpenThreadWaitChainSession(0, None) };
    if session.is_null() {
        let error_code = unsafe { GetLastError() };
        return WaitChainSnapshot {
            available: false,
            cycle: false,
            error_code: (error_code != 0).then_some(error_code),
            nodes: Vec::new(),
        };
    }

    let mut count = WCT_MAX_NODE_COUNT;
    let mut raw_nodes = [WAITCHAIN_NODE_INFO::default(); WCT_MAX_NODE_COUNT as usize];
    let mut cycle = 0;
    let available = unsafe {
        GetThreadWaitChain(
            session,
            0,
            WCT_OUT_OF_PROC_FLAG,
            thread_id,
            &mut count,
            raw_nodes.as_mut_ptr(),
            &mut cycle,
        )
    } != 0;
    let error_code = (!available).then(|| unsafe { GetLastError() });
    unsafe { CloseThreadWaitChainSession(session) };

    if !available {
        return WaitChainSnapshot {
            available: false,
            cycle: false,
            error_code: error_code.filter(|code| *code != 0),
            nodes: Vec::new(),
        };
    }
    let nodes = raw_nodes
        .iter()
        .take(count as usize)
        .map(|node| {
            let (process_id, thread_id) = if node.ObjectType == WctThreadType {
                let thread = unsafe { node.Anonymous.ThreadObject };
                (Some(thread.ProcessId), Some(thread.ThreadId))
            } else {
                (None, None)
            };
            WaitChainNode {
                object_type: wait_object_type(node.ObjectType),
                object_status: wait_object_status(node.ObjectStatus),
                process_id,
                thread_id,
            }
        })
        .collect();
    WaitChainSnapshot {
        available: true,
        cycle: cycle != 0,
        error_code: None,
        nodes,
    }
}

#[cfg(windows)]
#[allow(non_upper_case_globals)]
fn wait_object_type(value: i32) -> &'static str {
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    match value {
        WctCriticalSectionType => "critical_section",
        WctSendMessageType => "send_message",
        WctMutexType => "mutex",
        WctAlpcType => "alpc",
        WctComType => "com",
        WctThreadWaitType => "thread_wait",
        WctProcessWaitType => "process_wait",
        WctThreadType => "thread",
        WctComActivationType => "com_activation",
        WctSocketIoType => "socket_io",
        WctSmbIoType => "smb_io",
        _ => "unknown",
    }
}

#[cfg(windows)]
#[allow(non_upper_case_globals)]
fn wait_object_status(value: i32) -> &'static str {
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    match value {
        WctStatusNoAccess => "no_access",
        WctStatusRunning => "running",
        WctStatusBlocked => "blocked",
        WctStatusPidOnly => "pid_only",
        WctStatusPidOnlyRpcss => "pid_only_rpcss",
        WctStatusOwned => "owned",
        WctStatusNotOwned => "not_owned",
        WctStatusAbandoned => "abandoned",
        WctStatusError => "error",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_reports_once_throttles_and_recovers() {
        let mut state = WatchdogState::default();
        assert_eq!(state.evaluate(10_000, 0, true), WatchdogAction::None);
        assert_eq!(
            state.evaluate(15_000, 0, true),
            WatchdogAction::ReportStall { age_ms: 15_000 }
        );
        assert_eq!(state.evaluate(70_000, 0, true), WatchdogAction::None);
        assert_eq!(
            state.evaluate(75_000, 0, true),
            WatchdogAction::ReportStall { age_ms: 75_000 }
        );
        assert_eq!(
            state.evaluate(80_000, 79_500, false),
            WatchdogAction::Recovered {
                duration_ms: 80_000
            }
        );
        assert_eq!(state.evaluate(81_000, 80_500, false), WatchdogAction::None);
    }

    #[test]
    fn watchdog_requires_an_outstanding_probe_before_declaring_a_stall() {
        let mut state = WatchdogState::default();
        assert_eq!(state.evaluate(60_000, 0, false), WatchdogAction::None);
    }

    #[test]
    fn wait_chain_timeout_is_reported_once_only_for_an_in_flight_capture() {
        assert!(!wait_chain_timeout_due(10_000, 0, false, false));
        assert!(!wait_chain_timeout_due(4_999, 0, true, false));
        assert!(wait_chain_timeout_due(5_000, 0, true, false));
        assert!(!wait_chain_timeout_due(10_000, 0, true, true));
    }

    #[test]
    fn rotating_writer_bounds_and_preserves_complete_json_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(LOG_FILE);
        let mut writer = RotatingWriter::new(path.clone(), 180, 3).unwrap();
        for sequence in 0..30 {
            writer
                .write_json_line(&serde_json::json!({
                    "event": "test",
                    "sequence": sequence,
                    "padding": "abcdefghijklmnopqrstuvwxyz"
                }))
                .unwrap();
        }
        drop(writer);

        let files = std::iter::once(path.clone())
            .chain((1..=3).map(|index| rotated_log_path(&path, index)))
            .filter(|candidate| candidate.exists())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 4);
        for file in files {
            let bytes = fs::read(&file).unwrap();
            assert!(bytes.len() <= 180);
            for line in bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                serde_json::from_slice::<serde_json::Value>(line).unwrap();
            }
        }
    }

    #[test]
    fn rotating_writer_creates_its_log_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join(LOG_FILE);
        let _writer = RotatingWriter::new(path.clone(), 180, 3).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn panic_source_keeps_only_the_file_name() {
        assert_eq!(
            source_file_name(r"C:\private\workspace\src\passwords.rs"),
            Some("passwords.rs".to_string())
        );
        assert_eq!(
            source_file_name("/private/src/lib.rs"),
            Some("lib.rs".to_string())
        );
    }

    #[test]
    fn rotated_log_names_are_stable() {
        let path = PathBuf::from("petaldesk-diagnostics.log");
        assert_eq!(
            rotated_log_path(&path, 2),
            PathBuf::from("petaldesk-diagnostics.2.log")
        );
    }
}

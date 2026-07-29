use crate::browser_bridge::{
    BrowserBridge, BrowserBridgeStatus, BrowserConnectionStatus, BrowserFamily,
};
use crate::error::{AppError, AppResult};
use crate::screenshot::{
    self, MonitorBounds, ScreenshotExportAction, ScreenshotStore, CAPTURE_WINDOW_LABEL,
};
use crate::storage::{atomic_write, atomic_write_json, INTERNAL_DATA_DIR};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
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
const CONTROL_WINDOW_HEIGHT: u32 = 68;
const CONTROL_WINDOW_MAX_WIDTH: u32 = 680;
const MANUAL_SCROLL_POLL_INTERVAL: Duration = Duration::from_millis(500);

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
    let job = lock_unpoisoned(&store.job).take();
    if let Some(job) = job {
        {
            let mut runtime = lock_unpoisoned(&job.runtime);
            runtime.cancel_requested = true;
            runtime.pause_requested = false;
            runtime.generation = runtime.generation.wrapping_add(1);
            if transition_to_canceled(&mut runtime.manifest.state) {
                runtime.manifest.message = "应用退出，长截图已取消".to_string();
            }
        }
        job.wake.notify_all();
        let _ = restore_browser_session(&job);
        restore_hidden_pin_windows(app, &job.hidden_pin_labels);
        let _ = std::fs::remove_dir_all(&job.directory);
    }
    let _ = close_control_window(app);
    clear_all_annotation_exports(&store);
    cleanup_cache_directories_owned_by(&store.cache_root, std::process::id());
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
    let session = screenshot_store
        .active_session()
        .filter(|session| session.id == request.session_id)
        .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?;
    let session_id = session.id.clone();
    store.begin_pending_start(&session_id)?;
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

    let previous_job_id = {
        let current = lock_unpoisoned(&store.job);
        if let Some(job) = current.as_ref() {
            let runtime = lock_unpoisoned(&job.runtime);
            if !runtime.worker_done {
                return Err(AppError::new(
                    "long_capture_busy",
                    "已有长截图任务正在运行，请先完成或取消",
                ));
            }
            Some(runtime.manifest.job_id.clone())
        } else {
            None
        }
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
    if store.pending_start_cancel_requested(&session.id) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(pending_start_canceled_error());
    }
    let hidden_pin_labels = hide_visible_pin_windows(&app);
    if let Err(error) = hide_capture_overlay(&app) {
        restore_hidden_pin_windows(&app, &hidden_pin_labels);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    flush_desktop_compositor();
    std::thread::sleep(Duration::from_millis(60));
    if store.pending_start_cancel_requested(&session.id) {
        restore_hidden_pin_windows(app, &hidden_pin_labels);
        let error = recover_capture_overlay(app, &session.monitor, pending_start_canceled_error());
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
    };
    if let Err(error) = atomic_write_json(&directory.join("manifest.json"), &manifest) {
        restore_hidden_pin_windows(&app, &hidden_pin_labels);
        let error = recover_capture_overlay(&app, &session.monitor, error);
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    let job = Arc::new(LongCaptureJob {
        directory,
        target,
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
) -> Option<LongCaptureReentrySurface> {
    if pending_start {
        return Some(LongCaptureReentrySurface::Pending);
    }
    match state? {
        LongCaptureState::Preparing | LongCaptureState::Capturing => {
            Some(LongCaptureReentrySurface::Control)
        }
        LongCaptureState::Paused | LongCaptureState::Ready | LongCaptureState::Failed => {
            Some(LongCaptureReentrySurface::Overlay)
        }
        LongCaptureState::Canceled => None,
    }
}

pub(crate) fn restore_active_long_capture_surface(app: &AppHandle) -> AppResult<bool> {
    let Some(store) = app.try_state::<LongScreenshotStore>() else {
        return Ok(false);
    };
    if lock_unpoisoned(&store.pending_start).is_some() {
        return Ok(true);
    }
    let Some(job) = lock_unpoisoned(&store.job).as_ref().cloned() else {
        return Ok(false);
    };
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    let Some(surface) = long_capture_reentry_surface(false, Some(job.status().state)) else {
        return Ok(false);
    };
    match surface {
        LongCaptureReentrySurface::Pending => {}
        LongCaptureReentrySurface::Control => {
            // A control window overlapping the ROI is hidden briefly around
            // each frame. Forcing it visible there would capture the controls.
            if !job.target.control_overlaps_roi {
                show_control_window(app, &job)?;
            }
        }
        LongCaptureReentrySurface::Overlay => {
            show_capture_overlay(app, &job.target.monitor)?;
        }
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
    switch_visible_surface(
        || show_capture_overlay(&app, &job.target.monitor),
        || hide_control_window(&app),
    )?;
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
    resume_or_retry(app, &store.job(job_id)?, explicit_retry)
}

fn resume_or_retry(
    app: &AppHandle,
    job: &Arc<LongCaptureJob>,
    explicit_retry: bool,
) -> AppResult<LongCaptureStatus> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
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
        runtime.manifest.height = removed.output_y;
        persist_runtime(&job, &runtime)?;
        removed
    };
    remove_segment_files(&job.directory, &removed);
    let status = job.status();
    switch_visible_surface(
        || show_capture_overlay(&app, &job.target.monitor),
        || hide_control_window(&app),
    )?;
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
    let store = app.state::<LongScreenshotStore>();
    let pending_canceled = store.request_pending_start_cancel(session_id);
    let Some(job) = store.job_for_session(session_id) else {
        if pending_canceled {
            let screenshot_store = app.state::<ScreenshotStore>();
            if let Some(session) = screenshot_store
                .active_session()
                .filter(|session| session.id == session_id)
            {
                show_capture_overlay(app, &session.monitor)?;
            }
        }
        return Ok(None);
    };
    cancel_long_capture_job_inner(app, &store, &job).map(Some)
}

fn cancel_long_capture_job_inner(
    app: &AppHandle,
    store: &LongScreenshotStore,
    job: &Arc<LongCaptureJob>,
) -> AppResult<LongCaptureStatus> {
    let _operation_guard = lock_unpoisoned(&job.operation_lock);
    let job_id = job.status().job_id;
    let (status, cleanup_now) = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        let cleanup_now = runtime.worker_done;
        runtime.cancel_requested = true;
        runtime.pause_requested = false;
        runtime.generation = runtime.generation.wrapping_add(1);
        if transition_to_canceled(&mut runtime.manifest.state) {
            runtime.manifest.message = "长截图已取消".to_string();
            persist_runtime(&job, &runtime)?;
        }
        (status_from_runtime(&runtime), cleanup_now)
    };
    job.wake.notify_all();
    let surface_result = switch_visible_surface(
        || show_capture_overlay(app, &job.target.monitor),
        || close_control_window(app),
    );
    if cleanup_now {
        clear_job_cache(store, &job_id);
    }
    if let Err(surface_error) = surface_result {
        return Err(recover_capture_surface(&app, &job, surface_error));
    }
    Ok(status)
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
    screenshot::finish_capture(app, &app.state::<ScreenshotStore>(), &ticket.session_id);
    clear_job_cache(&app.state::<LongScreenshotStore>(), &ticket.job_id);
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
    job: &LongCaptureJob,
    action: ScreenshotExportAction,
    annotation_payload: Option<&Value>,
) -> AppResult<LongCaptureExportResult> {
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
    screenshot::finish_capture(app, &app.state::<ScreenshotStore>(), &manifest.session_id);
    clear_job_cache(&app.state::<LongScreenshotStore>(), &manifest.job_id);
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

fn show_control_window(app: &AppHandle, job: &LongCaptureJob) -> AppResult<()> {
    let (position, size, _) = control_window_geometry(&job.target.monitor, job.target.bounds);
    let window = if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        window
    } else {
        let _creation_guard = lock_unpoisoned(&CONTROL_WINDOW_CREATION_LOCK);
        if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
            window
        } else {
            WebviewWindowBuilder::new(
                app,
                CONTROL_WINDOW_LABEL,
                WebviewUrl::App(
                    format!("?tool=screenshot&longControl={}", job.status().job_id).into(),
                ),
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
            .map_err(|error| {
                AppError::new("window_error", format!("创建长截图控制窗口失败: {error}"))
            })?
        }
    };
    configure_control_window_no_activate(&window)?;
    window
        .set_position(position)
        .and_then(|_| window.set_size(size))
        .map_err(|error| {
            AppError::new("window_error", format!("定位长截图控制窗口失败: {error}"))
        })?;
    show_control_window_no_activate(&window)?;
    Ok(())
}

fn hide_control_window(app: &AppHandle) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        window.hide().map_err(|error| {
            AppError::new("window_error", format!("隐藏长截图控制窗口失败: {error}"))
        })?;
    }
    Ok(())
}

fn close_control_window(app: &AppHandle) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        window.destroy().map_err(|error| {
            AppError::new("window_error", format!("关闭长截图控制窗口失败: {error}"))
        })?;
    }
    Ok(())
}

pub(crate) fn handle_control_window_close_requested(app: &AppHandle, label: &str) -> bool {
    if label != CONTROL_WINDOW_LABEL {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(store) = app.try_state::<LongScreenshotStore>() else {
            return;
        };
        let job = lock_unpoisoned(&store.job).as_ref().cloned();
        if let Some(job) = job {
            let _ = cancel_long_capture_job_inner(&app, &store, &job);
        } else {
            let _ = close_control_window(&app);
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
    let result = run_capture_worker(&app, &job);
    let canceled = matches!(&result, Ok(WorkerStop::Cancel));
    browser_restore.restore_now();
    if let Some(position) = original_cursor {
        let _ = set_cursor_position(position);
    }

    match result {
        Ok(WorkerStop::Cancel) => {
            let _ = std::fs::remove_dir_all(&job.directory);
        }
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

    let (cleanup_requested, job_id) = {
        let mut runtime = lock_unpoisoned(&job.runtime);
        runtime.worker_done = true;
        let cleanup_requested = should_cleanup_after_worker(&runtime);
        let job_id = runtime.manifest.job_id.clone();
        job.wake.notify_all();
        (cleanup_requested, job_id)
    };
    restore_hidden_pin_windows(&app, &job.hidden_pin_labels);
    if canceled || cleanup_requested {
        clear_job_cache(&app.state::<LongScreenshotStore>(), &job_id);
    }
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
    let mut needs_scroll = true;
    let mut no_motion_count = 0_u8;
    let mut low_confidence_count = 0_u8;

    loop {
        let checkpoint = match wait_for_worker(job) {
            Ok(checkpoint) => checkpoint,
            Err(stop) => return Ok(stop),
        };
        if checkpoint.generation != known_generation {
            previous = load_latest_accepted_frame(job)?;
            known_generation = checkpoint.generation;
            needs_scroll = !checkpoint.retry_current;
            no_motion_count = 0;
            low_confidence_count = 0;
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
                if job.target.control_overlaps_roi {
                    hide_control_window(app)?;
                    flush_desktop_compositor();
                }
                if no_motion_count == 0 {
                    send_wheel_scroll(&job.target)?;
                } else {
                    update_message(
                        job,
                        checkpoint.generation,
                        "滚轮未推动目标，正在尝试 PageDown",
                    )?;
                    emit_progress(app, job);
                    send_page_down(&job.target)?;
                }
            }
        }
        let current = if manual_scrolling && needs_scroll {
            wait_for_manual_scroll(app, job, &previous, checkpoint.generation)?
        } else {
            capture_job_roi(app, job, true)?
        };
        if !iteration_is_current(job, checkpoint.generation) {
            continue;
        }

        let previous_strips = GrayStrips::from_frame(&previous);
        let current_strips = GrayStrips::from_frame(&current);
        let stationary_score =
            alignment_score(&previous_strips, &current_strips, 0, 2).unwrap_or(f32::INFINITY);
        if stationary_score <= 2.25 {
            no_motion_count = if browser_step.is_some_and(|step| step.at_bottom && !step.moved) {
                2
            } else {
                no_motion_count.saturating_add(1)
            };
            needs_scroll = true;
            update_message(
                job,
                checkpoint.generation,
                if no_motion_count >= 2 {
                    "已检测到滚动区域底部"
                } else {
                    "未检测到新内容，正在确认是否到底"
                },
            )?;
            emit_progress(app, job);
            if no_motion_count >= 2 {
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

        let Some(overlap) = find_vertical_overlap(&previous, &current) else {
            low_confidence_count = low_confidence_count.saturating_add(1);
            needs_scroll = false;
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

        let reached_limit = accept_scrolled_frame(job, &current, overlap, checkpoint.generation)?;
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
    if job.target.control_overlaps_roi {
        hide_control_window(app)?;
        flush_desktop_compositor();
    }
    let result = if wait_for_settle {
        capture_settled_roi(job.target.bounds)
    } else {
        capture_roi(job.target.bounds)
    };
    if job.target.control_overlaps_roi {
        let _operation_guard = lock_unpoisoned(&job.operation_lock);
        let should_restore_control = {
            let runtime = lock_unpoisoned(&job.runtime);
            control_surface_needed(&runtime)
        };
        if !should_restore_control {
            return result;
        }
        if let Err(restore_error) = show_control_window(app, job) {
            let failure = match result {
                Ok(_) => restore_error,
                Err(capture_error) => {
                    append_recovery_error(capture_error, "恢复长截图控制窗口失败", &restore_error)
                }
            };
            return Err(recover_capture_surface(app, job, failure));
        }
    }
    result
}

fn wait_for_manual_scroll(
    app: &AppHandle,
    job: &LongCaptureJob,
    previous: &Frame,
    generation: u64,
) -> AppResult<Frame> {
    update_message(job, generation, "等待手动滚动")?;
    emit_progress(app, job);
    loop {
        std::thread::sleep(MANUAL_SCROLL_POLL_INTERVAL);
        let candidate = capture_manual_candidate(app, job, previous)?;
        if !iteration_is_current(job, generation) {
            return Ok(candidate);
        }
        if sampled_frame_difference(previous, &candidate) > 2.5 {
            // Polling masks the visible control strip so it can remain stable.
            // Once movement is detected, hide it once and acquire a clean
            // settled frame; only clean frames are ever stitched or exported.
            return capture_job_roi(app, job, true);
        }
    }
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

fn accept_scrolled_frame(
    job: &LongCaptureJob,
    frame: &Frame,
    overlap: OverlapMatch,
    generation: u64,
) -> AppResult<bool> {
    let (index, output_y, allowed_rows) = {
        let runtime = lock_unpoisoned(&job.runtime);
        if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing
        {
            return Ok(false);
        }
        let maximum_height = maximum_height_for_width(runtime.manifest.width);
        let remaining = maximum_height.saturating_sub(runtime.manifest.height);
        (
            runtime.manifest.segments.len() as u32,
            runtime.manifest.height,
            overlap.displacement.min(remaining),
        )
    };
    if allowed_rows == 0 {
        return Ok(true);
    }

    let frame_file = format!("frames/{index:06}.png");
    let strip_file = format!("strips/{index:06}.png");
    let strip_start = frame.height.saturating_sub(overlap.displacement);
    let strip = crop_rows(frame, strip_start, allowed_rows)?;
    atomic_write(&job.directory.join(&frame_file), &encode_png(frame)?)?;
    atomic_write(&job.directory.join(&strip_file), &encode_png(&strip)?)?;

    let mut runtime = lock_unpoisoned(&job.runtime);
    if runtime.generation != generation || runtime.manifest.state != LongCaptureState::Capturing {
        let _ = std::fs::remove_file(job.directory.join(&frame_file));
        let _ = std::fs::remove_file(job.directory.join(&strip_file));
        return Ok(false);
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
    runtime.manifest.height = output_y.saturating_add(allowed_rows);
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
    switch_visible_surface(
        || show_capture_overlay(app, &job.target.monitor),
        || hide_control_window(app),
    )
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
    fn from_frame(frame: &Frame) -> Self {
        let width = frame.width as usize;
        let height = frame.height as usize;
        let strip_count = width.clamp(1, 5);
        let samples_per_strip = if width >= 24 { 3 } else { 1 };
        let samples_per_row = strip_count * samples_per_strip;
        let mut x_positions = Vec::with_capacity(samples_per_row);
        let half_span = (width / 80).clamp(1, 12);
        for strip in 0..strip_count {
            let center = ((strip + 1) * width / (strip_count + 1)).min(width - 1);
            if samples_per_strip == 1 {
                x_positions.push(center);
            } else {
                x_positions.push(center.saturating_sub(half_span));
                x_positions.push(center);
                x_positions.push((center + half_span).min(width - 1));
            }
        }

        let mut values = Vec::with_capacity(height * samples_per_row);
        for y in 0..height {
            for &x in &x_positions {
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

#[derive(Debug, Clone, Copy)]
struct OverlapMatch {
    displacement: u32,
    confidence: f32,
}

fn find_vertical_overlap(previous: &Frame, current: &Frame) -> Option<OverlapMatch> {
    if previous.width != current.width || previous.height != current.height || previous.height < 24
    {
        return None;
    }
    let previous = GrayStrips::from_frame(previous);
    let current = GrayStrips::from_frame(current);
    if previous.texture.min(current.texture) < 1.1 {
        return None;
    }
    let height = previous.height;
    let minimum = 2_usize;
    let maximum = (height.saturating_mul(9) / 10).min(height.saturating_sub(8));
    if maximum <= minimum {
        return None;
    }

    let mut best_displacement = 0_usize;
    let mut best_score = f32::INFINITY;
    let mut candidates = Vec::with_capacity(maximum - minimum + 1);
    for displacement in minimum..=maximum {
        let Some(score) = alignment_score(&previous, &current, displacement, 4) else {
            continue;
        };
        candidates.push((displacement, score));
        if score < best_score {
            best_score = score;
            best_displacement = displacement;
        }
    }
    if !best_score.is_finite() {
        return None;
    }

    let refine_start = best_displacement.saturating_sub(3).max(minimum);
    let refine_end = (best_displacement + 3).min(maximum);
    for displacement in refine_start..=refine_end {
        if let Some(score) = alignment_score(&previous, &current, displacement, 1) {
            if score < best_score {
                best_score = score;
                best_displacement = displacement;
            }
        }
    }

    let exclusion = (height / 100).clamp(4, 16);
    let second_score = candidates
        .into_iter()
        .filter(|(displacement, _)| displacement.abs_diff(best_displacement) > exclusion)
        .map(|(_, score)| score)
        .fold(f32::INFINITY, f32::min);
    let gap = second_score - best_score;
    let required_gap = (best_score * 0.10).max(0.55);
    if best_score > 9.5 || !second_score.is_finite() || gap < required_gap {
        return None;
    }
    let quality = (1.0 - best_score / 14.0).clamp(0.0, 1.0);
    let separation = (gap / (best_score + gap).max(0.1)).clamp(0.0, 1.0);
    Some(OverlapMatch {
        displacement: best_displacement as u32,
        confidence: (quality * 0.72 + separation * 0.28).clamp(0.0, 1.0),
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
fn send_wheel_scroll(target: &CaptureTarget) -> AppResult<()> {
    send_scroll_ticks(
        target,
        -(wheel_ticks_for_height(target.bounds.height) as i32),
    )
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

#[cfg(windows)]
fn send_page_down(target: &CaptureTarget) -> AppResult<()> {
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_NEXT,
    };

    focus_scroll_target(target)?;
    // Unlike wheel input, PageDown is delivered to the foreground keyboard
    // target. Refuse to send it if focus changed after activation.
    if !scroll_target_has_foreground_focus(target) {
        return Err(AppError::new(
            "long_capture_target_focus_lost",
            "长截图目标已失去输入焦点，未发送 PageDown。请重新点击目标内容后继续",
        ));
    }
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_NEXT,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_NEXT,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        return Err(last_windows_error("发送长截图 PageDown 输入"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_wheel_scroll(_target: &CaptureTarget) -> AppResult<()> {
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

#[cfg(not(windows))]
fn send_page_down(_target: &CaptureTarget) -> AppResult<()> {
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

fn wheel_ticks_for_height(viewport_height: u32) -> u32 {
    // Windows and applications apply their own line/page settings. A bounded
    // batch gives browsers roughly 55-70% movement while retaining overlap.
    (viewport_height / 140).clamp(3, 10)
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
    fn screenshot_reentry_keeps_the_existing_long_capture_surface() {
        assert_eq!(
            long_capture_reentry_surface(true, None),
            Some(LongCaptureReentrySurface::Pending)
        );
        assert_eq!(
            long_capture_reentry_surface(false, Some(LongCaptureState::Capturing)),
            Some(LongCaptureReentrySurface::Control)
        );
        assert_eq!(
            long_capture_reentry_surface(false, Some(LongCaptureState::Paused)),
            Some(LongCaptureReentrySurface::Overlay)
        );
        assert_eq!(
            long_capture_reentry_surface(false, Some(LongCaptureState::Ready)),
            Some(LongCaptureReentrySurface::Overlay)
        );
        assert_eq!(
            long_capture_reentry_surface(false, Some(LongCaptureState::Canceled)),
            None
        );
        assert_eq!(long_capture_reentry_surface(false, None), None);
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
    fn overlap_match_rejects_ambiguous_low_texture_frames() {
        let previous = solid_frame(160, 120, 245);
        let current = solid_frame(160, 120, 245);
        assert!(find_vertical_overlap(&previous, &current).is_none());
        let strips = GrayStrips::from_frame(&previous);
        assert_eq!(alignment_score(&strips, &strips, 0, 1), Some(0.0));
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
        };
        let tile = compose_tile(root.path(), &manifest, 2, 3).unwrap();
        assert_eq!((tile.width, tile.height), (3, 3));
        assert_eq!(tile.rgba[0], 30);
        assert_eq!(tile.rgba[3 * 4], 40);
        assert_eq!(tile.rgba[6 * 4], 50);
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
    fn resource_limits_bound_height_and_wheel_batch() {
        assert_eq!(maximum_height_for_width(1_000), 100_000);
        assert_eq!(maximum_height_for_width(4_000), 50_000);
        assert_eq!(maximum_height_for_width(0), 0);
        assert_eq!(wheel_ticks_for_height(100), 3);
        assert_eq!(wheel_ticks_for_height(4_000), 10);
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
            hidden_pin_labels: Vec::new(),
            browser: None,
            operation_lock: Mutex::new(()),
            runtime: Mutex::new(runtime),
            wake: Condvar::new(),
        });
        let store = LongScreenshotStore {
            cache_root: root.path().to_path_buf(),
            browser_bridge: None,
            job: Mutex::new(Some(job)),
            annotation_exports: Mutex::new(HashMap::from([(
                "export-1".to_string(),
                LongCaptureAnnotationExportTicket {
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
            )])),
            start_lock: Mutex::new(()),
            pending_start: Mutex::new(None),
        };

        clear_job_cache(&store, "job-1");
        assert!(lock_unpoisoned(&store.job).is_none());
        assert!(lock_unpoisoned(&store.annotation_exports).is_empty());
        assert!(!job_directory.exists());
        assert!(!export_directory.exists());
    }

    #[test]
    fn pending_start_cancellation_is_scoped_to_its_screenshot_session() {
        let root = tempdir().unwrap();
        let store = LongScreenshotStore {
            cache_root: root.path().to_path_buf(),
            browser_bridge: None,
            job: Mutex::new(None),
            annotation_exports: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            pending_start: Mutex::new(None),
        };

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
}

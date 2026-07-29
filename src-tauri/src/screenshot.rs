use crate::commands::lock_window_creation;
use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write, atomic_write_json, INTERNAL_DATA_DIR};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl,
    WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use uuid::Uuid;

pub(crate) const CAPTURE_WINDOW_LABEL: &str = "screenshot-capture";
const PIN_WINDOW_PREFIX: &str = "screenshot-pin-";
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const DEFAULT_SHORTCUT: &str = "F1";
const EXPORT_TOKEN_HEADER: &str = "x-petaldesk-export-token";
const MAX_PNG_BYTES: usize = 128 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const EXPORT_TICKET_TTL: Duration = Duration::from_secs(5 * 60);

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSettings {
    #[serde(default = "settings_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_shortcut")]
    pub shortcut: String,
    #[serde(default)]
    pub last_save_directory: Option<String>,
    #[serde(default = "default_color_format")]
    pub color_format: String,
    #[serde(default)]
    pub tool_parameters: Map<String, Value>,
}

impl Default for ScreenshotSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            shortcut: default_shortcut(),
            last_save_directory: None,
            color_format: default_color_format(),
            tool_parameters: Map::new(),
        }
    }
}

fn settings_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

fn default_shortcut() -> String {
    DEFAULT_SHORTCUT.to_string()
}

fn default_color_format() -> String {
    "hex".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSettingsPatch {
    pub color_format: Option<String>,
    pub tool_parameters: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSession {
    pub id: String,
    pub monitor: MonitorBounds,
    pub frame_width: u32,
    pub frame_height: u32,
    pub captured_at: String,
}

#[derive(Debug, Clone)]
struct ActiveSession {
    meta: ScreenshotSession,
    /// Shared so handing the frame out does not duplicate several megabytes,
    /// and so no copy happens while the session mutex is held.
    png: Arc<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScreenshotExportAction {
    Copy,
    Save,
    Pin,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareScreenshotExportRequest {
    pub session_id: String,
    pub action: ScreenshotExportAction,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareScreenshotExportResult {
    pub canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotExportResult {
    pub action: ScreenshotExportAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePinnedScreenshotResult {
    pub canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}

#[derive(Debug, Clone)]
struct ExportTicket {
    session_id: String,
    action: ScreenshotExportAction,
    save_path: Option<PathBuf>,
    issued_at: Instant,
}

#[derive(Debug, Clone)]
struct PinnedScreenshot {
    png: Arc<Vec<u8>>,
    width: u32,
    height: u32,
}

pub struct ScreenshotStore {
    settings_path: PathBuf,
    settings: RwLock<ScreenshotSettings>,
    session: Mutex<Option<ActiveSession>>,
    pins: Mutex<HashMap<String, PinnedScreenshot>>,
    export_tickets: Mutex<HashMap<String, ExportTicket>>,
    start_lock: Mutex<()>,
    shortcut_change_lock: Mutex<()>,
}

impl ScreenshotStore {
    pub fn load(data_storage_path: &Path) -> AppResult<Self> {
        let settings_path = data_storage_path
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("screenshot.json");
        let (settings, recovered_from_corruption) = match std::fs::read(&settings_path) {
            Ok(bytes) => match serde_json::from_slice::<ScreenshotSettings>(&bytes)
                .map_err(AppError::from)
                .and_then(|mut settings| {
                    normalize_settings(&mut settings)?;
                    Ok(settings)
                }) {
                Ok(settings) => (settings, false),
                Err(_) => {
                    let _ = preserve_corrupt_settings_file(&settings_path);
                    (ScreenshotSettings::default(), true)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (ScreenshotSettings::default(), false)
            }
            Err(error) => return Err(AppError::io("读取截图设置", error)),
        };
        // A damaged preference file must never prevent the whole notes app
        // from starting. Preserve it for diagnosis and replace it best-effort.
        if recovered_from_corruption {
            let _ = atomic_write_json(&settings_path, &settings);
        } else {
            atomic_write_json(&settings_path, &settings)?;
        }
        Ok(Self {
            settings_path,
            settings: RwLock::new(settings),
            session: Mutex::new(None),
            pins: Mutex::new(HashMap::new()),
            export_tickets: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            shortcut_change_lock: Mutex::new(()),
        })
    }

    pub(crate) fn settings(&self) -> ScreenshotSettings {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn persist_settings(&self, settings: &ScreenshotSettings) -> AppResult<()> {
        atomic_write_json(&self.settings_path, settings)
    }

    fn replace_settings(&self, settings: ScreenshotSettings) {
        *self
            .settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
    }

    pub(crate) fn update_last_save_directory(&self, path: &Path) -> AppResult<()> {
        let Some(parent) = path.parent() else {
            return Err(AppError::invalid("截图保存路径没有父目录"));
        };
        let mut settings = self.settings();
        settings.last_save_directory = Some(parent.to_string_lossy().into_owned());
        self.persist_settings(&settings)?;
        self.replace_settings(settings);
        Ok(())
    }

    pub(crate) fn active_session(&self) -> Option<ScreenshotSession> {
        lock_unpoisoned(&self.session)
            .as_ref()
            .map(|session| session.meta.clone())
    }

    /// Lock order is screenshot lifecycle, then long-capture pending/job and
    /// operation locks. Long-capture code must never acquire this in reverse.
    pub(crate) fn lock_start(&self) -> MutexGuard<'_, ()> {
        lock_unpoisoned(&self.start_lock)
    }

    fn session_png(&self, session_id: &str) -> AppResult<Arc<Vec<u8>>> {
        let session = lock_unpoisoned(&self.session);
        let active = session
            .as_ref()
            .filter(|active| active.meta.id == session_id)
            .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?;
        Ok(Arc::clone(&active.png))
    }

    fn clear_session(&self, expected_id: Option<&str>) -> bool {
        let mut session = lock_unpoisoned(&self.session);
        let should_clear = match (expected_id, session.as_ref()) {
            (Some(expected), Some(active)) => active.meta.id == expected,
            (Some(_), None) => false,
            (None, _) => true,
        };
        if should_clear {
            *session = None;
        }
        should_clear
    }

    fn prepare_ticket(
        &self,
        session_id: String,
        action: ScreenshotExportAction,
        save_path: Option<PathBuf>,
    ) -> String {
        let token = Uuid::new_v4().to_string();
        let mut tickets = lock_unpoisoned(&self.export_tickets);
        tickets.retain(|_, ticket| ticket.issued_at.elapsed() <= EXPORT_TICKET_TTL);
        tickets.insert(
            token.clone(),
            ExportTicket {
                session_id,
                action,
                save_path,
                issued_at: Instant::now(),
            },
        );
        token
    }

    fn consume_ticket(&self, token: &str) -> AppResult<ExportTicket> {
        let ticket = lock_unpoisoned(&self.export_tickets)
            .remove(token)
            .ok_or_else(|| AppError::new("invalid_export_ticket", "截图导出凭证无效或已使用"))?;
        if ticket.issued_at.elapsed() > EXPORT_TICKET_TTL {
            return Err(AppError::new(
                "expired_export_ticket",
                "截图导出凭证已过期，请重新执行导出",
            ));
        }
        Ok(ticket)
    }
}

fn preserve_corrupt_settings_file(path: &Path) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid("截图设置文件没有父目录"))?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let backup = parent.join(format!(
        "screenshot.corrupt-{timestamp}-{}.json",
        Uuid::new_v4()
    ));
    if std::fs::rename(path, &backup).is_ok() {
        return Ok(());
    }
    std::fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|error| AppError::io("备份损坏的截图设置", error))
}

fn normalize_settings(settings: &mut ScreenshotSettings) -> AppResult<()> {
    settings.schema_version = SETTINGS_SCHEMA_VERSION;
    settings.shortcut = normalize_shortcut(&settings.shortcut)?;
    settings.color_format = normalize_color_format(&settings.color_format)?;
    if settings
        .last_save_directory
        .as_deref()
        .is_some_and(str::is_empty)
    {
        settings.last_save_directory = None;
    }
    Ok(())
}

fn normalize_shortcut(value: &str) -> AppResult<String> {
    let shortcut = value.trim();
    if shortcut.is_empty() || shortcut.len() > 96 {
        return Err(AppError::invalid(
            "截图快捷键不能为空且长度不能超过 96 个字符",
        ));
    }
    Shortcut::from_str(shortcut)
        .map(Shortcut::into_string)
        .map_err(|error| AppError::invalid(format!("截图快捷键无效: {error}")))
}

fn normalize_color_format(value: &str) -> AppResult<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "hex" => Ok("hex".to_string()),
        "rgb" => Ok("rgb".to_string()),
        _ => Err(AppError::invalid("取色格式只能是 hex 或 rgb")),
    }
}

pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // Both WebView construction and the global-shortcut manager synchronously
    // dispatch work to Tauri's event loop. Setup itself runs on that loop, so
    // prewarm them from a worker after setup returns to avoid a channel deadlock.
    let app = app.handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = ensure_capture_window(&app) {
            eprintln!("无法预创建截图窗口: {error}");
            let _ = app.emit("screenshot_capture_error", error);
        }
        let shortcut = app.state::<ScreenshotStore>().settings().shortcut;
        if let Err(error) = register_screenshot_shortcut(&app, &shortcut) {
            eprintln!("无法注册截图快捷键 {shortcut}: {error}");
            let _ = app.emit(
                "screenshot_shortcut_error",
                serde_json::json!({ "shortcut": shortcut, "message": error.message }),
            );
        }
    });
    Ok(())
}

fn register_screenshot_shortcut(app: &AppHandle, shortcut: &str) -> AppResult<()> {
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                spawn_start_capture(app);
            }
        })
        .map_err(|error| {
            AppError::new(
                "shortcut_conflict",
                format!("快捷键 {shortcut} 无法注册，可能已被其他程序占用: {error}"),
            )
        })
}

pub(crate) fn spawn_start_capture(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = start_capture_inner(&app) {
            let _ = app.emit("screenshot_capture_error", error);
        }
    });
}

#[tauri::command]
pub fn get_screenshot_settings(store: State<'_, ScreenshotStore>) -> ScreenshotSettings {
    store.settings()
}

#[tauri::command]
pub fn set_screenshot_shortcut(
    app: AppHandle,
    store: State<'_, ScreenshotStore>,
    shortcut: String,
) -> AppResult<ScreenshotSettings> {
    let shortcut = normalize_shortcut(&shortcut)?;
    let _change_guard = lock_unpoisoned(&store.shortcut_change_lock);
    let previous = store.settings();
    if shortcut.eq_ignore_ascii_case(&previous.shortcut) {
        return Ok(previous);
    }

    // Keep the old registration alive until the new shortcut is known-good.
    register_screenshot_shortcut(&app, &shortcut)?;
    if let Err(error) = app.global_shortcut().unregister(previous.shortcut.as_str()) {
        let _ = app.global_shortcut().unregister(shortcut.as_str());
        return Err(AppError::new(
            "shortcut_update_failed",
            format!("无法停用原截图快捷键，已保留原设置: {error}"),
        ));
    }
    let mut updated = previous.clone();
    updated.shortcut = shortcut.clone();
    if let Err(error) = store.persist_settings(&updated) {
        let rollback = register_screenshot_shortcut(&app, &previous.shortcut);
        let _ = app.global_shortcut().unregister(shortcut.as_str());
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(AppError::new(
                "shortcut_rollback_failed",
                format!("保存快捷键失败，且恢复原快捷键失败: {error}; {rollback_error}"),
            )),
        };
    }
    store.replace_settings(updated.clone());
    crate::refresh_tray_menu(&app);
    let _ = app.emit(
        "screenshot_settings_changed",
        serde_json::json!({ "settings": updated.clone() }),
    );
    Ok(updated)
}

#[tauri::command]
pub fn update_screenshot_settings(
    app: AppHandle,
    store: State<'_, ScreenshotStore>,
    patch: ScreenshotSettingsPatch,
) -> AppResult<ScreenshotSettings> {
    let mut settings = store.settings();
    if let Some(color_format) = patch.color_format {
        settings.color_format = normalize_color_format(&color_format)?;
    }
    if let Some(tool_parameters) = patch.tool_parameters {
        settings.tool_parameters = tool_parameters;
    }
    store.persist_settings(&settings)?;
    store.replace_settings(settings.clone());
    let _ = app.emit(
        "screenshot_settings_changed",
        serde_json::json!({ "settings": settings.clone() }),
    );
    Ok(settings)
}

#[tauri::command]
pub async fn start_screenshot_capture(app: AppHandle) -> AppResult<ScreenshotSession> {
    tauri::async_runtime::spawn_blocking(move || start_capture_inner(&app))
        .await
        .map_err(|error| AppError::new("capture_error", format!("截图任务异常结束: {error}")))?
}

pub(crate) fn start_capture_inner(app: &AppHandle) -> AppResult<ScreenshotSession> {
    let store = app.state::<ScreenshotStore>();
    let _start_guard = lock_unpoisoned(&store.start_lock);
    if crate::long_screenshot::restore_active_long_capture_surface(app)? {
        return store.active_session().ok_or_else(|| {
            AppError::new(
                "long_capture_busy",
                "长截图仍在运行，但原截图会话已结束；请先取消长截图后重试",
            )
        });
    }
    if let Some(session) = store.active_session() {
        if capture_window_is_visible(app) {
            present_capture_window(app)?;
        } else {
            prepare_capture_window(app, &session.monitor)?;
            let _ = app.emit("screenshot_session_ready", &session);
        }
        return Ok(session);
    }

    let hidden_pins = hide_visible_pin_windows(app);
    if let Some(window) = app.get_webview_window(CAPTURE_WINDOW_LABEL) {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        }
    }
    flush_desktop_compositor();
    let frame_result = capture_cursor_monitor(app);
    restore_windows(&hidden_pins);
    let (monitor, png) = frame_result?;
    let session = ScreenshotSession {
        id: Uuid::new_v4().to_string(),
        frame_width: monitor.width,
        frame_height: monitor.height,
        monitor,
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    *lock_unpoisoned(&store.session) = Some(ActiveSession {
        meta: session.clone(),
        png: Arc::new(png),
    });
    if let Err(error) = prepare_capture_window(app, &session.monitor) {
        store.clear_session(Some(&session.id));
        return Err(error);
    }
    let _ = app.emit("screenshot_session_ready", &session);
    Ok(session)
}

#[tauri::command]
pub fn get_screenshot_session(store: State<'_, ScreenshotStore>) -> Option<ScreenshotSession> {
    store.active_session()
}

#[tauri::command]
pub fn get_screenshot_frame(
    store: State<'_, ScreenshotStore>,
    session_id: String,
) -> AppResult<Response> {
    // `Response` owns its body, so unwrap the Arc when we hold the only
    // reference and fall back to a copy otherwise.
    Ok(Response::new(
        Arc::try_unwrap(store.session_png(&session_id)?).unwrap_or_else(|shared| (*shared).clone()),
    ))
}

#[tauri::command]
pub fn present_screenshot_capture(
    app: AppHandle,
    store: State<'_, ScreenshotStore>,
    session_id: String,
) -> AppResult<()> {
    let monitor = {
        let session = lock_unpoisoned(&store.session);
        session
            .as_ref()
            .filter(|active| active.meta.id == session_id)
            .map(|active| active.meta.monitor.clone())
            .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?
    };
    prepare_capture_window(&app, &monitor)?;
    present_capture_window(&app)
}

#[tauri::command]
pub async fn cancel_screenshot_capture(
    app: AppHandle,
    session_id: Option<String>,
) -> AppResult<bool> {
    tauri::async_runtime::spawn_blocking(move || {
        cancel_screenshot_capture_inner(&app, session_id.as_deref())
    })
    .await
    .map_err(|error| {
        AppError::new(
            "capture_cancel_error",
            format!("取消截图任务异常结束: {error}"),
        )
    })
}

fn cancel_screenshot_capture_inner(app: &AppHandle, session_id: Option<&str>) -> bool {
    let store = app.state::<ScreenshotStore>();
    // Keep ordinary screenshot startup from observing a long-capture job after
    // its owning session has already disappeared. Do not hold the session lock
    // while canceling: closing the control window can dispatch callbacks that
    // read the active session.
    let _start_guard = store.lock_start();
    let Some(closing_id) = store.active_session().map(|session| session.id) else {
        return false;
    };
    if session_id.is_some_and(|expected_id| expected_id != closing_id) {
        return false;
    }

    let long_capture_error =
        crate::long_screenshot::cancel_for_screenshot_session_end(&app, &closing_id).err();
    // Cancellation is requested before the fallible persistence/window-close
    // work. Even when that work reports an error, clear the ordinary owner so
    // the screenshot UI and shortcut cannot remain stuck.
    let cleared = store.clear_session(Some(&closing_id));
    if cleared {
        if let Some(window) = app.get_webview_window(CAPTURE_WINDOW_LABEL) {
            let _ = window.hide();
        }
        let _ = app.emit(
            "screenshot_session_closed",
            serde_json::json!({ "id": &closing_id }),
        );
    }
    if let Some(error) = long_capture_error {
        eprintln!("取消截图时清理长截图失败: {error}");
        let _ = app.emit("screenshot_capture_error", error);
    }
    cleared
}

#[tauri::command]
pub async fn prepare_screenshot_export(
    app: AppHandle,
    request: PrepareScreenshotExportRequest,
) -> AppResult<PrepareScreenshotExportResult> {
    let app_for_dialog = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        prepare_screenshot_export_inner(&app_for_dialog, request)
    })
    .await
    .map_err(|error| AppError::new("export_error", format!("准备截图导出失败: {error}")))?
}

fn prepare_screenshot_export_inner(
    app: &AppHandle,
    request: PrepareScreenshotExportRequest,
) -> AppResult<PrepareScreenshotExportResult> {
    let store = app.state::<ScreenshotStore>();
    let active = store
        .active_session()
        .filter(|session| session.id == request.session_id)
        .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?;
    let save_path = if request.action == ScreenshotExportAction::Save {
        let path = choose_png_save_path(app, &store.settings())?;
        let Some(path) = path else {
            return Ok(PrepareScreenshotExportResult {
                canceled: true,
                ticket: None,
            });
        };
        store.update_last_save_directory(&path)?;
        Some(path)
    } else {
        None
    };
    let token = store.prepare_ticket(active.id, request.action, save_path);
    Ok(PrepareScreenshotExportResult {
        canceled: false,
        ticket: Some(token),
    })
}

#[tauri::command]
pub fn commit_screenshot_export(
    app: AppHandle,
    store: State<'_, ScreenshotStore>,
    request: Request<'_>,
) -> AppResult<ScreenshotExportResult> {
    let token = request
        .headers()
        .get(EXPORT_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::invalid("缺少截图导出凭证"))?;
    // One copy out of the IPC body is unavoidable; wrapping it here keeps the
    // pin path from making a second one.
    let png = match request.body() {
        InvokeBody::Raw(bytes) => Arc::new(bytes.clone()),
        InvokeBody::Json(_) => {
            return Err(AppError::invalid(
                "截图必须以 Uint8Array 原始二进制提交，不能使用 JSON 或 Base64",
            ))
        }
    };
    let ticket = store.consume_ticket(token)?;
    let session = store
        .active_session()
        .filter(|session| session.id == ticket.session_id)
        .ok_or_else(|| AppError::not_found("截图会话已结束或已被替换"))?;
    let decoded = decode_png(&png)?;
    if decoded.width > session.monitor.width || decoded.height > session.monitor.height {
        return Err(AppError::invalid("导出图片尺寸超出当前截图显示器范围"));
    }

    let mut result = ScreenshotExportResult {
        action: ticket.action,
        saved_path: None,
        pin_id: None,
    };
    match ticket.action {
        ScreenshotExportAction::Copy => write_png_to_clipboard(&png, &decoded)?,
        ScreenshotExportAction::Save => {
            let path = ticket
                .save_path
                .ok_or_else(|| AppError::invalid("保存导出缺少目标路径"))?;
            atomic_write(&path, &png)?;
            result.saved_path = Some(path.to_string_lossy().into_owned());
        }
        ScreenshotExportAction::Pin => {
            let pin_id = Uuid::new_v4().to_string();
            lock_unpoisoned(&store.pins).insert(
                pin_id.clone(),
                PinnedScreenshot {
                    png,
                    width: decoded.width,
                    height: decoded.height,
                },
            );
            result.pin_id = Some(pin_id.clone());
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = open_pin_window(app.clone(), &pin_id) {
                    lock_unpoisoned(&app.state::<ScreenshotStore>().pins).remove(&pin_id);
                    let _ = app.emit(
                        "screenshot_pin_error",
                        serde_json::json!({ "pinId": pin_id, "error": error }),
                    );
                }
            });
        }
    }
    // Export ends the ordinary screenshot session. Reuse the owner-aware
    // teardown so a concurrently starting long capture is canceled before the
    // session disappears, and a stale export cannot affect a newer session.
    let _ = cancel_screenshot_capture_inner(&app, Some(&ticket.session_id));
    Ok(result)
}

#[tauri::command]
pub fn get_pinned_screenshot(
    store: State<'_, ScreenshotStore>,
    pin_id: String,
) -> AppResult<Response> {
    // Copy outside the lock: pins stay resident, so the Arc is always shared and
    // a copy is unavoidable here, but it must not block other pin windows.
    let png = {
        let pins = lock_unpoisoned(&store.pins);
        Arc::clone(
            &pins
                .get(&pin_id)
                .ok_or_else(|| AppError::not_found("置顶截图不存在或已经关闭"))?
                .png,
        )
    };
    Ok(Response::new((*png).clone()))
}

#[tauri::command]
pub fn copy_pinned_screenshot(store: State<'_, ScreenshotStore>, pin_id: String) -> AppResult<()> {
    let png = {
        let pins = lock_unpoisoned(&store.pins);
        Arc::clone(
            &pins
                .get(&pin_id)
                .ok_or_else(|| AppError::not_found("置顶截图不存在或已经关闭"))?
                .png,
        )
    };
    let decoded = decode_png(&png)?;
    write_png_to_clipboard(&png, &decoded)
}

pub(crate) fn copy_png_bytes(png: &[u8]) -> AppResult<()> {
    let decoded = decode_png(png)?;
    write_png_to_clipboard(png, &decoded)
}

pub(crate) fn pin_png_bytes(
    app: AppHandle,
    png: Vec<u8>,
    width: u32,
    height: u32,
) -> AppResult<String> {
    if width > 32_767 || height > 32_767 {
        return Err(AppError::new(
            "long_capture_pin_limit",
            "长图边长超过置顶贴图限制，请保存原图",
        ));
    }
    checked_rgba_len(width, height)?;
    let pin_id = Uuid::new_v4().to_string();
    lock_unpoisoned(&app.state::<ScreenshotStore>().pins).insert(
        pin_id.clone(),
        PinnedScreenshot {
            png: Arc::new(png),
            width,
            height,
        },
    );
    if let Err(error) = open_pin_window(app.clone(), &pin_id) {
        lock_unpoisoned(&app.state::<ScreenshotStore>().pins).remove(&pin_id);
        return Err(error);
    }
    Ok(pin_id)
}

#[tauri::command]
pub async fn save_pinned_screenshot(
    app: AppHandle,
    pin_id: String,
) -> AppResult<SavePinnedScreenshotResult> {
    tauri::async_runtime::spawn_blocking(move || save_pinned_screenshot_inner(&app, &pin_id))
        .await
        .map_err(|error| AppError::new("save_error", format!("保存置顶截图失败: {error}")))?
}

fn save_pinned_screenshot_inner(
    app: &AppHandle,
    pin_id: &str,
) -> AppResult<SavePinnedScreenshotResult> {
    let store = app.state::<ScreenshotStore>();
    let png = {
        let pins = lock_unpoisoned(&store.pins);
        Arc::clone(
            &pins
                .get(pin_id)
                .ok_or_else(|| AppError::not_found("置顶截图不存在或已经关闭"))?
                .png,
        )
    };
    let Some(path) = choose_png_save_path(app, &store.settings())? else {
        return Ok(SavePinnedScreenshotResult {
            canceled: true,
            saved_path: None,
        });
    };
    atomic_write(&path, &png)?;
    store.update_last_save_directory(&path)?;
    Ok(SavePinnedScreenshotResult {
        canceled: false,
        saved_path: Some(path.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub fn close_pinned_screenshot(
    app: AppHandle,
    store: State<'_, ScreenshotStore>,
    pin_id: String,
) -> bool {
    let removed = lock_unpoisoned(&store.pins).remove(&pin_id).is_some();
    if let Some(window) = app.get_webview_window(&format!("{PIN_WINDOW_PREFIX}{pin_id}")) {
        let _ = window.destroy();
    }
    removed
}

fn spawn_capture_window_cleanup(app: &AppHandle, session_id: Option<String>, destroy_window: bool) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(session_id) = session_id {
            let _ = cancel_screenshot_capture_inner(&app, Some(&session_id));
        }
        if destroy_window {
            if let Some(window) = app.get_webview_window(CAPTURE_WINDOW_LABEL) {
                let _ = window.destroy();
            }
        }
    });
}

pub(crate) fn handle_window_close_requested(app: &AppHandle, label: &str) -> bool {
    let store = app.state::<ScreenshotStore>();
    if label == CAPTURE_WINDOW_LABEL {
        let session_id = store.active_session().map(|session| session.id);
        let Some(session_id) = session_id else {
            return false;
        };
        // Keep the window alive until its long-capture owner has observed
        // cancellation. The caller prevents this close request; the worker
        // performs the final destroy after clearing the ordinary session.
        spawn_capture_window_cleanup(app, Some(session_id), true);
        true
    } else if let Some(pin_id) = label.strip_prefix(PIN_WINDOW_PREFIX) {
        lock_unpoisoned(&store.pins).remove(pin_id);
        false
    } else {
        false
    }
}

pub(crate) fn handle_window_destroyed(app: &AppHandle, label: &str) {
    let store = app.state::<ScreenshotStore>();
    if label == CAPTURE_WINDOW_LABEL {
        let session_id = store.active_session().map(|session| session.id);
        // `destroy()` bypasses CloseRequested. Run the same owner-first cleanup
        // so a WebView crash or frontend fallback cannot orphan a long job.
        if session_id.is_some() {
            spawn_capture_window_cleanup(app, session_id, false);
        }
    } else if let Some(pin_id) = label.strip_prefix(PIN_WINDOW_PREFIX) {
        lock_unpoisoned(&store.pins).remove(pin_id);
    }
}

fn finish_capture_unlocked(app: &AppHandle, store: &ScreenshotStore, session_id: &str) {
    if !store.clear_session(Some(session_id)) {
        return;
    }
    if let Some(window) = app.get_webview_window(CAPTURE_WINDOW_LABEL) {
        let _ = window.hide();
    }
    let _ = app.emit(
        "screenshot_session_closed",
        serde_json::json!({ "id": session_id }),
    );
}

pub(crate) fn finish_capture_after_cleanup(
    app: &AppHandle,
    store: &ScreenshotStore,
    session_id: &str,
    cleanup: impl FnOnce(),
) {
    // Screenshot startup uses the same lock. Keep long-job cleanup and session
    // teardown indivisible so a shortcut cannot observe only one side removed.
    let _start_guard = lock_unpoisoned(&store.start_lock);
    cleanup();
    finish_capture_unlocked(app, store, session_id);
}

fn choose_png_save_path(
    app: &AppHandle,
    settings: &ScreenshotSettings,
) -> AppResult<Option<PathBuf>> {
    let file_name = format!("PetalDesk截图-{}.png", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut dialog = app
        .dialog()
        .file()
        .add_filter("PNG 图片", &["png"])
        .set_title("保存截图 - 飞花 - PetalDesk")
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
        .map_err(|error| AppError::invalid(format!("截图保存路径无效: {error}")))?;
    if path.extension().is_none() {
        path.set_extension("png");
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err(AppError::invalid("截图只能保存为 PNG 文件"));
    }
    Ok(Some(path))
}

fn ensure_capture_window(app: &AppHandle) -> AppResult<()> {
    if app.get_webview_window(CAPTURE_WINDOW_LABEL).is_some() {
        return Ok(());
    }
    let _creation_guard = lock_window_creation();
    if app.get_webview_window(CAPTURE_WINDOW_LABEL).is_some() {
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        CAPTURE_WINDOW_LABEL,
        WebviewUrl::App("?tool=screenshot".into()),
    )
    .title("截图 - 飞花 - PetalDesk")
    .decorations(false)
    .shadow(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .inner_size(320.0, 200.0)
    .build()
    .map_err(|error| AppError::new("window_error", format!("预创建截图窗口失败: {error}")))?;
    Ok(())
}

fn capture_window_is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(CAPTURE_WINDOW_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

pub(crate) fn prepare_capture_window(app: &AppHandle, monitor: &MonitorBounds) -> AppResult<()> {
    ensure_capture_window(app)?;
    let window = app
        .get_webview_window(CAPTURE_WINDOW_LABEL)
        .ok_or_else(|| AppError::new("window_error", "截图窗口创建后不可用"))?;
    window
        .set_position(PhysicalPosition::new(monitor.x, monitor.y))
        .map_err(|error| AppError::new("window_error", format!("定位截图窗口失败: {error}")))?;
    window
        .set_size(PhysicalSize::new(monitor.width, monitor.height))
        .map_err(|error| AppError::new("window_error", format!("调整截图窗口失败: {error}")))?;
    let _ = window.set_always_on_top(true);
    Ok(())
}

pub(crate) fn present_capture_window(app: &AppHandle) -> AppResult<()> {
    let window = app
        .get_webview_window(CAPTURE_WINDOW_LABEL)
        .ok_or_else(|| AppError::new("window_error", "截图窗口尚未准备完成"))?;
    window
        .show()
        .map_err(|error| AppError::new("window_error", format!("显示截图窗口失败: {error}")))?;
    let _ = window.unminimize();
    let _ = window.set_focus();
    Ok(())
}

fn open_pin_window(app: AppHandle, pin_id: &str) -> AppResult<String> {
    let _creation_guard = lock_window_creation();
    let label = format!("{PIN_WINDOW_PREFIX}{pin_id}");
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(label);
    }
    let pin = {
        let store = app.state::<ScreenshotStore>();
        let pins = lock_unpoisoned(&store.pins);
        pins.get(pin_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("置顶截图不存在"))?
    };
    let (width, height) = initial_pin_size(pin.width, pin.height);
    WebviewWindowBuilder::new(
        &app,
        &label,
        WebviewUrl::App(format!("?screenshotPin={pin_id}").into()),
    )
    .title("贴图 - 飞花 - PetalDesk")
    .decorations(false)
    .shadow(false)
    .transparent(true)
    .resizable(true)
    .inner_size(width, height)
    .always_on_top(true)
    .skip_taskbar(true)
    .center()
    .build()
    .map_err(|error| AppError::new("window_error", format!("创建置顶截图失败: {error}")))?;
    Ok(label)
}

fn initial_pin_size(width: u32, height: u32) -> (f64, f64) {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let scale = (720.0 / width).min(520.0 / height).min(1.0);
    (width * scale, height * scale)
}

fn hide_visible_pin_windows(app: &AppHandle) -> Vec<tauri::WebviewWindow> {
    let mut hidden = Vec::new();
    for (label, window) in app.webview_windows() {
        if label.starts_with(PIN_WINDOW_PREFIX) && window.is_visible().unwrap_or(false) {
            if window.hide().is_ok() {
                hidden.push(window);
            }
        }
    }
    hidden
}

fn restore_windows(windows: &[tauri::WebviewWindow]) {
    for window in windows {
        let _ = window.show();
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

fn monitor_scale_factor(app: &AppHandle, bounds: &MonitorBounds) -> f64 {
    app.available_monitors()
        .unwrap_or_default()
        .into_iter()
        .find(|monitor| {
            monitor.position().x == bounds.x
                && monitor.position().y == bounds.y
                && monitor.size().width == bounds.width
                && monitor.size().height == bounds.height
        })
        .map(|monitor| monitor.scale_factor())
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(1.0)
}

fn capture_cursor_monitor(app: &AppHandle) -> AppResult<(MonitorBounds, Vec<u8>)> {
    let (mut bounds, bgra) = capture_cursor_monitor_bgra()?;
    bounds.scale_factor = monitor_scale_factor(app, &bounds);
    let rgba = bgra_to_rgba(&bgra)?;
    let png = encode_rgba_png(bounds.width, bounds.height, &rgba)?;
    Ok((bounds, png))
}

fn bgra_to_rgba(bgra: &[u8]) -> AppResult<Vec<u8>> {
    if bgra.len() % 4 != 0 {
        return Err(AppError::new(
            "capture_error",
            "Windows 截图像素缓冲区长度无效",
        ));
    }
    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    Ok(rgba)
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> AppResult<Vec<u8>> {
    let expected = checked_rgba_len(width, height)?;
    if rgba.len() != expected {
        return Err(AppError::invalid("PNG 像素长度与图片尺寸不匹配"));
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|error| AppError::new("png_error", format!("创建 PNG 失败: {error}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| AppError::new("png_error", format!("编码 PNG 失败: {error}")))?;
    }
    Ok(png)
}

#[derive(Debug)]
struct DecodedPng {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_png(bytes: &[u8]) -> AppResult<DecodedPng> {
    if bytes.is_empty() || bytes.len() > MAX_PNG_BYTES {
        return Err(AppError::invalid(format!(
            "PNG 大小必须在 1 到 {} MB 之间",
            MAX_PNG_BYTES / 1024 / 1024
        )));
    }
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| AppError::new("invalid_png", format!("PNG 文件无效: {error}")))?;
    let width = reader.info().width;
    let height = reader.info().height;
    checked_rgba_len(width, height)?;
    let mut output = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| AppError::new("invalid_png", format!("PNG 像素数据无效: {error}")))?;
    let source = &output[..info.buffer_size()];
    let pixel_count = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| AppError::invalid("PNG 图片尺寸过大"))?;
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
            return Err(AppError::new("invalid_png", "PNG 调色板数据未正确展开"))
        }
    }
    if rgba.len() != pixel_count * 4 {
        return Err(AppError::new("invalid_png", "PNG 解码后的像素长度无效"));
    }
    Ok(DecodedPng {
        width,
        height,
        rgba,
    })
}

fn checked_rgba_len(width: u32, height: u32) -> AppResult<usize> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|pixels| *pixels > 0 && *pixels <= MAX_IMAGE_PIXELS)
        .ok_or_else(|| AppError::invalid("图片尺寸为空或超过安全限制"))?;
    usize::try_from(pixels * 4).map_err(|_| AppError::invalid("图片像素缓冲区过大"))
}

#[cfg(windows)]
fn capture_cursor_monitor_bgra() -> AppResult<(MonitorBounds, Vec<u8>)> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO,
        MONITOR_DEFAULTTONEAREST, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

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

    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return Err(last_windows_error("读取鼠标位置"));
    }
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return Err(last_windows_error("定位鼠标所在显示器"));
    }
    let mut monitor_info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut monitor_info) } == 0 {
        return Err(last_windows_error("读取显示器范围"));
    }
    let bounds = bounds_from_rect(monitor_info.rcMonitor)?;
    let byte_len = checked_rgba_len(bounds.width, bounds.height)?;

    let screen_dc = ScreenDc(unsafe { GetDC(null_mut()) });
    if screen_dc.0.is_null() {
        return Err(last_windows_error("获取桌面绘图上下文"));
    }
    let memory_dc = MemoryDc(unsafe { CreateCompatibleDC(screen_dc.0) });
    if memory_dc.0.is_null() {
        return Err(last_windows_error("创建截图绘图上下文"));
    }
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: i32::try_from(bounds.width)
                .map_err(|_| AppError::invalid("显示器宽度超过 Windows 限制"))?,
            biHeight: -i32::try_from(bounds.height)
                .map_err(|_| AppError::invalid("显示器高度超过 Windows 限制"))?,
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
        return Err(last_windows_error("创建截图像素缓冲区"));
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
            bounds.width as i32,
            bounds.height as i32,
            screen_dc.0,
            bounds.x,
            bounds.y,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err(last_windows_error("复制显示器画面"));
    }
    let pixels = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_len) }.to_vec();
    drop(selected);
    Ok((bounds, pixels))
}

#[cfg(windows)]
fn bounds_from_rect(rect: windows_sys::Win32::Foundation::RECT) -> AppResult<MonitorBounds> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::new("capture_error", "显示器宽度无效"))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::new("capture_error", "显示器高度无效"))?;
    Ok(MonitorBounds {
        x: rect.left,
        y: rect.top,
        width,
        height,
        scale_factor: 1.0,
    })
}

#[cfg(not(windows))]
fn capture_cursor_monitor_bgra() -> AppResult<(MonitorBounds, Vec<u8>)> {
    Err(AppError::new(
        "unsupported_platform",
        "截图第一阶段仅支持 Windows 10/11",
    ))
}

#[cfg(windows)]
fn last_windows_error(action: &str) -> AppError {
    AppError::new(
        "windows_error",
        format!("{action}失败: {}", std::io::Error::last_os_error()),
    )
}

#[cfg(windows)]
fn write_png_to_clipboard(png: &[u8], decoded: &DecodedPng) -> AppResult<()> {
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows_sys::Win32::Graphics::Gdi::{BITMAPV5HEADER, BI_BITFIELDS};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_DIBV5: u32 = 17;
    const LCS_SRGB: u32 = 0x7352_4742;
    const LCS_GM_IMAGES: u32 = 4;

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    struct GlobalMemory(HGLOBAL);
    impl GlobalMemory {
        fn from_bytes(bytes: &[u8]) -> AppResult<Self> {
            let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
            if handle.is_null() {
                return Err(last_windows_error("分配剪贴板内存"));
            }
            let destination = unsafe { GlobalLock(handle) };
            if destination.is_null() {
                unsafe {
                    let _ = GlobalFree(handle);
                }
                return Err(last_windows_error("锁定剪贴板内存"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    destination.cast::<u8>(),
                    bytes.len(),
                );
                let _ = GlobalUnlock(handle);
            }
            Ok(Self(handle))
        }

        fn transfer(mut self) -> HGLOBAL {
            let handle = self.0;
            self.0 = null_mut();
            handle
        }
    }
    impl Drop for GlobalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = GlobalFree(self.0);
                }
            }
        }
    }

    let expected = checked_rgba_len(decoded.width, decoded.height)?;
    if decoded.rgba.len() != expected {
        return Err(AppError::new("invalid_png", "剪贴板图片像素长度无效"));
    }
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: decoded.width as i32,
        bV5Height: -(decoded.height as i32),
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: expected as u32,
        bV5RedMask: 0x00ff_0000,
        bV5GreenMask: 0x0000_ff00,
        bV5BlueMask: 0x0000_00ff,
        bV5AlphaMask: 0xff00_0000,
        bV5CSType: LCS_SRGB,
        bV5Intent: LCS_GM_IMAGES,
        ..BITMAPV5HEADER::default()
    };
    let mut dib = Vec::with_capacity(size_of::<BITMAPV5HEADER>() + expected);
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&header as *const BITMAPV5HEADER).cast::<u8>(),
            size_of::<BITMAPV5HEADER>(),
        )
    };
    dib.extend_from_slice(header_bytes);
    for pixel in decoded.rgba.chunks_exact(4) {
        dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    let dib_memory = GlobalMemory::from_bytes(&dib)?;
    let png_memory = GlobalMemory::from_bytes(png)?;
    let png_format_name: Vec<u16> = "PNG\0".encode_utf16().collect();
    let png_format = unsafe { RegisterClipboardFormatW(png_format_name.as_ptr()) };
    if png_format == 0 {
        return Err(last_windows_error("注册 PNG 剪贴板格式"));
    }

    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened {
        return Err(AppError::new(
            "clipboard_busy",
            "剪贴板正被其他程序占用，请稍后重试",
        ));
    }
    let _clipboard = ClipboardGuard;
    if unsafe { EmptyClipboard() } == 0 {
        return Err(last_windows_error("清空剪贴板"));
    }
    let dib_handle = dib_memory.transfer();
    if unsafe { SetClipboardData(CF_DIBV5, dib_handle) }.is_null() {
        unsafe {
            let _ = GlobalFree(dib_handle);
        }
        return Err(last_windows_error("写入 CF_DIBV5 剪贴板图片"));
    }
    let png_handle = png_memory.transfer();
    if unsafe { SetClipboardData(png_format, png_handle) }.is_null() {
        unsafe {
            let _ = GlobalFree(png_handle);
            let _ = EmptyClipboard();
        }
        return Err(last_windows_error("写入 PNG 剪贴板图片"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn write_png_to_clipboard(_png: &[u8], _decoded: &DecodedPng) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "截图剪贴板第一阶段仅支持 Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_and_persists_settings_under_the_data_root() {
        let root = tempdir().unwrap();
        let store = ScreenshotStore::load(root.path()).unwrap();
        assert_eq!(store.settings().shortcut, DEFAULT_SHORTCUT);
        assert_eq!(store.settings().color_format, "hex");
        assert!(root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("screenshot.json")
            .is_file());
    }

    #[test]
    fn preserves_malformed_settings_and_falls_back_to_defaults() {
        let root = tempdir().unwrap();
        let tools = root.path().join(INTERNAL_DATA_DIR).join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let settings_path = tools.join("screenshot.json");
        std::fs::write(&settings_path, b"{not valid json").unwrap();

        let store = ScreenshotStore::load(root.path()).unwrap();
        assert_eq!(store.settings(), ScreenshotSettings::default());
        let persisted: ScreenshotSettings =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(persisted, ScreenshotSettings::default());
        let backups = std::fs::read_dir(&tools)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("screenshot.corrupt-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(backups[0].path()).unwrap(),
            b"{not valid json"
        );
    }

    #[test]
    fn preserves_semantically_invalid_settings_and_falls_back_to_defaults() {
        let root = tempdir().unwrap();
        let tools = root.path().join(INTERNAL_DATA_DIR).join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let settings_path = tools.join("screenshot.json");
        std::fs::write(
            &settings_path,
            br#"{"schemaVersion":1,"shortcut":"not-a-key","colorFormat":"hsl"}"#,
        )
        .unwrap();

        let store = ScreenshotStore::load(root.path()).unwrap();
        assert_eq!(store.settings(), ScreenshotSettings::default());
        assert_eq!(
            std::fs::read_dir(&tools)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("screenshot.corrupt-"))
                .count(),
            1
        );
    }

    #[test]
    fn normalizes_valid_settings_and_rejects_bad_values() {
        assert_eq!(normalize_shortcut("  F1 ").unwrap(), "F1");
        assert!(normalize_shortcut("").is_err());
        assert!(normalize_shortcut("DefinitelyNotAKeyboardKey").is_err());
        assert_eq!(normalize_color_format(" RGB ").unwrap(), "rgb");
        assert!(normalize_color_format("hsl").is_err());
    }

    #[test]
    fn png_round_trip_preserves_rgba_pixels() {
        let rgba = vec![255, 0, 0, 255, 0, 128, 255, 64];
        let encoded = encode_rgba_png(2, 1, &rgba).unwrap();
        let decoded = decode_png(&encoded).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.rgba, rgba);
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn bgra_capture_pixels_are_converted_and_forced_opaque() {
        assert_eq!(
            bgra_to_rgba(&[3, 2, 1, 0, 30, 20, 10, 99]).unwrap(),
            [1, 2, 3, 255, 10, 20, 30, 255]
        );
        assert!(bgra_to_rgba(&[1, 2, 3]).is_err());
    }

    #[test]
    fn pin_window_size_preserves_ratio_and_is_bounded() {
        assert_eq!(initial_pin_size(720, 360), (720.0, 360.0));
        assert_eq!(initial_pin_size(1440, 720), (720.0, 360.0));
        let (width, height) = initial_pin_size(100, 1000);
        assert_eq!(height, 520.0);
        assert_eq!(width, 52.0);
        assert_eq!(width / height, 0.1);

        let (width, height) = initial_pin_size(1000, 100);
        assert_eq!(width, 720.0);
        assert_eq!(height, 72.0);
        assert_eq!(width / height, 10.0);
    }

    #[cfg(windows)]
    #[test]
    fn monitor_rect_keeps_negative_coordinates() {
        use windows_sys::Win32::Foundation::RECT;
        let bounds = bounds_from_rect(RECT {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        })
        .unwrap();
        assert_eq!(bounds.x, -1920);
        assert_eq!(bounds.y, -200);
        assert_eq!(bounds.width, 1920);
        assert_eq!(bounds.height, 1080);
    }

    #[test]
    fn expired_or_reused_export_tickets_are_rejected() {
        let root = tempdir().unwrap();
        let store = ScreenshotStore::load(root.path()).unwrap();
        let token = store.prepare_ticket("session".to_string(), ScreenshotExportAction::Copy, None);
        assert_eq!(
            store.consume_ticket(&token).unwrap().action,
            ScreenshotExportAction::Copy
        );
        assert!(store.consume_ticket(&token).is_err());
    }

    #[test]
    fn stale_session_teardown_cannot_clear_the_current_session() {
        let root = tempdir().unwrap();
        let store = ScreenshotStore::load(root.path()).unwrap();
        *lock_unpoisoned(&store.session) = Some(ActiveSession {
            meta: ScreenshotSession {
                id: "session-new".to_string(),
                monitor: MonitorBounds {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                    scale_factor: 1.0,
                },
                frame_width: 100,
                frame_height: 100,
                captured_at: "now".to_string(),
            },
            png: Arc::new(Vec::new()),
        });

        let _start_guard = store.lock_start();
        assert!(!store.clear_session(Some("session-old")));
        assert_eq!(
            store.active_session().map(|session| session.id),
            Some("session-new".to_string())
        );
        assert!(store.clear_session(Some("session-new")));
        assert!(store.active_session().is_none());
    }
}

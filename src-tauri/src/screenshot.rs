use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write, atomic_write_json, INTERNAL_DATA_DIR};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
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
const SHORTCUT_HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(30);
const SHORTCUT_HEALTHY_REFRESH_POLLS: u32 = 10;
const CAPTURE_PRESENT_TIMEOUT: Duration = Duration::from_secs(5);
static CAPTURE_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static PIN_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenshotSaveKind {
    Screenshot,
    LongScreenshot,
}

impl ScreenshotSaveKind {
    fn file_name(self) -> String {
        let prefix = match self {
            Self::Screenshot => "PetalDesk截图",
            Self::LongScreenshot => "PetalDesk长截图",
        };
        format!("{prefix}-{}.png", Utc::now().format("%Y%m%d-%H%M%S"))
    }

    fn dialog_title(self) -> &'static str {
        match self {
            Self::Screenshot => "保存截图 - 飞花 - PetalDesk",
            Self::LongScreenshot => "保存长截图 - 飞花 - PetalDesk",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Screenshot => "截图",
            Self::LongScreenshot => "长截图",
        }
    }
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
    settings_change_lock: Mutex<()>,
    shortcut_retry_needed: AtomicBool,
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
            settings_change_lock: Mutex::new(()),
            shortcut_retry_needed: AtomicBool::new(true),
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
        // Every settings update persists a complete snapshot. Serialize it
        // with shortcut changes so an older save-directory snapshot cannot
        // overwrite a newly registered shortcut.
        let _change_guard = lock_unpoisoned(&self.settings_change_lock);
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
    // run them from workers after setup returns to avoid a channel deadlock.
    // Registration must not wait for WebView2 prewarming: a slow or failed
    // controller creation must never leave this process without its shortcut.
    let shortcut_app = app.handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = ensure_screenshot_shortcut_registered(&shortcut_app) {
            let shortcut = shortcut_app.state::<ScreenshotStore>().settings().shortcut;
            eprintln!("无法注册截图快捷键 {shortcut}: {error}");
            let _ = shortcut_app.emit(
                "screenshot_shortcut_error",
                serde_json::json!({ "shortcut": shortcut, "message": error.message }),
            );
        }
    });
    spawn_screenshot_shortcut_health_check(app.handle());
    let prewarm_app = app.handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = ensure_capture_window(&prewarm_app) {
            eprintln!("无法预创建截图窗口: {error}");
            let _ = prewarm_app.emit("screenshot_capture_error", error);
        }
    });
    Ok(())
}

fn register_screenshot_shortcut(app: &AppHandle, shortcut: &str) -> AppResult<()> {
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                // Receiving the Win32 event is stronger evidence than the
                // plugin's in-memory `is_registered` bookkeeping.
                app.state::<ScreenshotStore>()
                    .shortcut_retry_needed
                    .store(false, Ordering::Release);
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

fn ensure_screenshot_shortcut_registered(app: &AppHandle) -> AppResult<()> {
    let store = app.state::<ScreenshotStore>();
    let _change_guard = lock_unpoisoned(&store.settings_change_lock);
    let shortcut = store.settings().shortcut;
    // This only avoids asking the plugin to insert a duplicate entry. It does
    // not prove that Windows still owns the underlying RegisterHotKey.
    let result = if app.global_shortcut().is_registered(shortcut.as_str()) {
        Ok(())
    } else {
        register_screenshot_shortcut(app, &shortcut)
    };
    store
        .shortcut_retry_needed
        .store(result.is_err(), Ordering::Release);
    result
}

fn refresh_screenshot_shortcut_registration(app: &AppHandle) -> AppResult<()> {
    let store = app.state::<ScreenshotStore>();
    let _change_guard = lock_unpoisoned(&store.settings_change_lock);
    let shortcut = store.settings().shortcut;
    let result = refresh_shortcut_with(
        &shortcut,
        app.global_shortcut().is_registered(shortcut.as_str()),
        || {
            app.global_shortcut()
                .unregister(shortcut.as_str())
                .map_err(|error| error.to_string())
        },
        || register_screenshot_shortcut(app, &shortcut),
    );
    store
        .shortcut_retry_needed
        .store(result.is_err(), Ordering::Release);
    result
}

fn refresh_shortcut_with(
    shortcut: &str,
    is_registered: bool,
    unregister: impl FnOnce() -> Result<(), String>,
    register: impl FnOnce() -> AppResult<()>,
) -> AppResult<()> {
    // `is_registered` reflects the plugin's in-process bookkeeping, while
    // Windows may already have dropped RegisterHotKey after a shell or power
    // transition. Even if unregister reports that stale OS state, registering
    // again can repair it, so unregister is deliberately best effort here.
    let unregister_error = is_registered.then(unregister).and_then(Result::err);
    match register() {
        Ok(()) => Ok(()),
        Err(register_error) => {
            if let Some(unregister_error) = unregister_error {
                Err(AppError::new(
                    "shortcut_refresh_failed",
                    format!(
                        "恢复截图快捷键 {shortcut} 失败；注销旧注册失败: {unregister_error}；重新注册失败: {}",
                        register_error.message
                    ),
                ))
            } else {
                Err(register_error)
            }
        }
    }
}

fn shortcut_health_check_due(retry_needed: bool, healthy_polls: u32) -> bool {
    retry_needed || healthy_polls >= SHORTCUT_HEALTHY_REFRESH_POLLS
}

fn spawn_screenshot_shortcut_health_check(app: &AppHandle) {
    let app = app.clone();
    let spawn_result = std::thread::Builder::new()
        .name("petaldesk-shortcut-health".to_string())
        .spawn(move || {
            let mut healthy_polls = 0_u32;
            loop {
                std::thread::sleep(SHORTCUT_HEALTH_POLL_INTERVAL);
                let retry_needed = app
                    .state::<ScreenshotStore>()
                    .shortcut_retry_needed
                    .load(Ordering::Acquire);
                healthy_polls = if retry_needed {
                    0
                } else {
                    healthy_polls.saturating_add(1)
                };
                if !shortcut_health_check_due(retry_needed, healthy_polls) {
                    continue;
                }
                healthy_polls = 0;

                // A forced unregister/register is the only meaningful probe:
                // the plugin's `is_registered` value is only its own hashmap
                // and cannot report whether Win32 silently lost the hotkey.
                // Periodic failures stay out of the UI to avoid repeated toast
                // messages; explicit setup/settings operations still report
                // their errors to the user.
                if let Err(error) = refresh_screenshot_shortcut_registration(&app) {
                    let shortcut = app.state::<ScreenshotStore>().settings().shortcut;
                    eprintln!("后台恢复截图快捷键 {shortcut} 失败: {error}");
                }
            }
        });
    if let Err(error) = spawn_result {
        eprintln!("无法启动截图快捷键健康检查: {error}");
    }
}

fn change_shortcut_with(
    previous: &str,
    shortcut: &str,
    mut register: impl FnMut(&str) -> AppResult<()>,
    mut unregister: impl FnMut(&str) -> Result<(), String>,
    mut persist: impl FnMut() -> AppResult<()>,
) -> AppResult<Option<String>> {
    // Keep the old registration alive until Windows has accepted its
    // replacement.
    register(shortcut)?;

    // A failed old-key unregister can mean that Win32 already lost it while
    // the plugin still has a stale bookkeeping entry. The new key is proven
    // usable, so continue committing the requested setting and report the
    // cleanup issue only as a diagnostic warning.
    let old_unregister_error = unregister(previous).err();

    if let Err(persist_error) = persist() {
        return match register(previous) {
            Ok(()) => match unregister(shortcut) {
                Ok(()) => Err(AppError::new(
                    "shortcut_persist_failed",
                    format!(
                        "保存截图快捷键失败，已恢复原快捷键 {previous}: {persist_error}"
                    ),
                )),
                Err(cleanup_error) => Err(AppError::new(
                    "shortcut_rollback_cleanup_failed",
                    format!(
                        "保存截图快捷键失败，原快捷键 {previous} 已恢复，但无法停用新快捷键 {shortcut}；两个快捷键本次运行中可能都可用: {persist_error}; {cleanup_error}"
                    ),
                )),
            },
            Err(rollback_error) => Err(AppError::new(
                "shortcut_rollback_failed",
                format!(
                    "保存截图快捷键失败，且无法恢复原快捷键 {previous}；已保留可用的新快捷键 {shortcut}: {persist_error}; {rollback_error}"
                ),
            )),
        };
    }

    Ok(old_unregister_error)
}

pub(crate) fn spawn_start_capture(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = start_capture_inner_impl(&app, false) {
            let _ = app.emit("screenshot_capture_error", error);
        }
    });
}

#[tauri::command]
pub fn get_screenshot_settings(store: State<'_, ScreenshotStore>) -> ScreenshotSettings {
    store.settings()
}

#[tauri::command]
pub async fn set_screenshot_shortcut(
    app: AppHandle,
    shortcut: String,
) -> AppResult<ScreenshotSettings> {
    tauri::async_runtime::spawn_blocking(move || set_screenshot_shortcut_inner(&app, shortcut))
        .await
        .map_err(|error| {
            AppError::new(
                "shortcut_update_failed",
                format!("更新截图快捷键任务异常结束: {error}"),
            )
        })?
}

fn set_screenshot_shortcut_inner(
    app: &AppHandle,
    shortcut: String,
) -> AppResult<ScreenshotSettings> {
    let store = app.state::<ScreenshotStore>();
    let shortcut = normalize_shortcut(&shortcut)?;
    let _change_guard = lock_unpoisoned(&store.settings_change_lock);
    let previous = store.settings();
    if shortcut.eq_ignore_ascii_case(&previous.shortcut) {
        drop(_change_guard);
        refresh_screenshot_shortcut_registration(app)?;
        return Ok(store.settings());
    }

    let mut updated = previous.clone();
    updated.shortcut = shortcut.clone();
    let transition = change_shortcut_with(
        &previous.shortcut,
        &shortcut,
        |candidate| register_screenshot_shortcut(app, candidate),
        |candidate| {
            app.global_shortcut()
                .unregister(candidate)
                .map_err(|error| error.to_string())
        },
        || store.persist_settings(&updated),
    );
    store
        .shortcut_retry_needed
        .store(transition.is_err(), Ordering::Release);
    if let Some(error) = transition? {
        eprintln!(
            "新截图快捷键 {shortcut} 已保存，但无法停用原快捷键 {}: {error}",
            previous.shortcut
        );
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
pub async fn update_screenshot_settings(
    app: AppHandle,
    patch: ScreenshotSettingsPatch,
) -> AppResult<ScreenshotSettings> {
    crate::commands::run_background("保存截图偏好", move || {
        let store = app.state::<ScreenshotStore>();
        // Keep the full-snapshot write atomic with shortcut and save-directory
        // changes; otherwise the last writer can restore stale fields.
        let _change_guard = lock_unpoisoned(&store.settings_change_lock);
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
    })
    .await
}

#[tauri::command]
pub async fn start_screenshot_capture(app: AppHandle) -> AppResult<ScreenshotSession> {
    tauri::async_runtime::spawn_blocking(move || start_capture_inner(&app))
        .await
        .map_err(|error| AppError::new("capture_error", format!("截图任务异常结束: {error}")))?
}

pub(crate) fn start_capture_inner(app: &AppHandle) -> AppResult<ScreenshotSession> {
    start_capture_inner_impl(app, true)
}

fn start_capture_inner_impl(
    app: &AppHandle,
    refresh_shortcut: bool,
) -> AppResult<ScreenshotSession> {
    // Tray/menu capture remains usable even if Windows temporarily rejects the
    // configured hotkey. Starting from a non-shortcut surface also gives the
    // registration a bounded opportunity to recover after a startup conflict.
    // A capture triggered by the shortcut itself already proves the Win32
    // registration works and skips this extra rebind.
    if refresh_shortcut {
        if let Err(error) = refresh_screenshot_shortcut_registration(app) {
            let shortcut = app.state::<ScreenshotStore>().settings().shortcut;
            let _ = app.emit(
                "screenshot_shortcut_error",
                serde_json::json!({ "shortcut": shortcut, "message": error.message }),
            );
        }
    }
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
pub async fn get_screenshot_frame(app: AppHandle, session_id: String) -> AppResult<Response> {
    crate::commands::run_background("读取截图画面", move || {
        // `Response` owns its body, so unwrap the Arc when we hold the only
        // reference and fall back to a copy otherwise.
        let png = app.state::<ScreenshotStore>().session_png(&session_id)?;
        Ok(Response::new(
            Arc::try_unwrap(png).unwrap_or_else(|shared| (*shared).clone()),
        ))
    })
    .await
}

#[tauri::command]
pub async fn present_screenshot_capture(app: AppHandle, session_id: String) -> AppResult<()> {
    crate::commands::run_background("显示截图界面", move || {
        let store = app.state::<ScreenshotStore>();
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
    })
    .await
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
    cancel_screenshot_capture_locked(app, &store, session_id)
}

/// Cancels the active screenshot while the caller owns `ScreenshotStore::lock_start()`.
fn cancel_screenshot_capture_locked(
    app: &AppHandle,
    store: &ScreenshotStore,
    session_id: Option<&str>,
) -> bool {
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
        let path = choose_screenshot_save_path(
            app,
            ScreenshotSaveKind::Screenshot,
            Some(CAPTURE_WINDOW_LABEL),
        )?;
        let Some(path) = path else {
            return Ok(PrepareScreenshotExportResult {
                canceled: true,
                ticket: None,
            });
        };
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
pub async fn commit_screenshot_export(
    app: AppHandle,
    request: Request<'_>,
) -> AppResult<ScreenshotExportResult> {
    let token = request
        .headers()
        .get(EXPORT_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::invalid("缺少截图导出凭证"))?
        .to_string();
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
    crate::commands::run_background("提交截图导出", move || {
        commit_screenshot_export_inner(&app, &token, png)
    })
    .await
}

fn commit_screenshot_export_inner(
    app: &AppHandle,
    token: &str,
    png: Arc<Vec<u8>>,
) -> AppResult<ScreenshotExportResult> {
    let store = app.state::<ScreenshotStore>();
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
    let _ = cancel_screenshot_capture_inner(app, Some(&ticket.session_id));
    Ok(result)
}

#[tauri::command]
pub async fn get_pinned_screenshot(app: AppHandle, pin_id: String) -> AppResult<Response> {
    crate::commands::run_background("读取置顶截图", move || {
        let store = app.state::<ScreenshotStore>();
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
    })
    .await
}

#[tauri::command]
pub async fn copy_pinned_screenshot(app: AppHandle, pin_id: String) -> AppResult<()> {
    crate::commands::run_background("复制置顶截图", move || {
        let store = app.state::<ScreenshotStore>();
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
    })
    .await
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
    let parent_label = format!("{PIN_WINDOW_PREFIX}{pin_id}");
    let Some(path) =
        choose_screenshot_save_path(app, ScreenshotSaveKind::Screenshot, Some(&parent_label))?
    else {
        return Ok(SavePinnedScreenshotResult {
            canceled: true,
            saved_path: None,
        });
    };
    atomic_write(&path, &png)?;
    Ok(SavePinnedScreenshotResult {
        canceled: false,
        saved_path: Some(path.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub async fn close_pinned_screenshot(app: AppHandle, pin_id: String) -> AppResult<bool> {
    crate::commands::run_background("关闭置顶截图", move || {
        let removed = lock_unpoisoned(&app.state::<ScreenshotStore>().pins)
            .remove(&pin_id)
            .is_some();
        if let Some(window) = app.get_webview_window(&format!("{PIN_WINDOW_PREFIX}{pin_id}")) {
            let _ = window.destroy();
        }
        Ok(removed)
    })
    .await
}

fn spawn_capture_window_cleanup(app: &AppHandle, session_id: Option<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(session_id) = session_id {
            let _ = cancel_screenshot_capture_inner(&app, Some(&session_id));
        }
    });
}

pub(crate) fn handle_window_close_requested(
    app: &AppHandle,
    label: &str,
    window_instance: Option<isize>,
) -> bool {
    let store = app.state::<ScreenshotStore>();
    if label == CAPTURE_WINDOW_LABEL {
        let current_instance = app
            .get_webview_window(CAPTURE_WINDOW_LABEL)
            .as_ref()
            .and_then(capture_window_instance_id);
        if !destroyed_window_matches_current(window_instance, current_instance) {
            return false;
        }
        let session_id = store.active_session().map(|session| session.id);
        let Some(session_id) = session_id else {
            return false;
        };
        // The capture WebView is prewarmed and reused. Hiding it after owner
        // cleanup also prevents a delayed destroy from removing a new session.
        spawn_capture_window_cleanup(app, Some(session_id));
        true
    } else if let Some(pin_id) = label.strip_prefix(PIN_WINDOW_PREFIX) {
        lock_unpoisoned(&store.pins).remove(pin_id);
        false
    } else {
        false
    }
}

fn destroyed_window_matches_current(
    destroyed_instance: Option<isize>,
    current_instance: Option<isize>,
) -> bool {
    match (destroyed_instance, current_instance) {
        (Some(destroyed), Some(current)) => destroyed == current,
        (None, Some(_)) => false,
        (_, None) => true,
    }
}

#[cfg(windows)]
fn capture_window_instance_id(window: &tauri::WebviewWindow<tauri::Wry>) -> Option<isize> {
    window.hwnd().ok().map(|handle| handle.0 as isize)
}

#[cfg(not(windows))]
fn capture_window_instance_id(_window: &tauri::WebviewWindow<tauri::Wry>) -> Option<isize> {
    None
}

pub(crate) fn handle_window_destroyed(
    app: &AppHandle,
    label: &str,
    destroyed_instance: Option<isize>,
) {
    let store = app.state::<ScreenshotStore>();
    if label == CAPTURE_WINDOW_LABEL {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let store = app.state::<ScreenshotStore>();
            // Wait out capture startup before comparing instances. Otherwise a
            // late event from the previous WebView can land after the new
            // session is published but before its window has been prepared.
            let _start_guard = store.lock_start();
            let current_instance = app
                .get_webview_window(CAPTURE_WINDOW_LABEL)
                .as_ref()
                .and_then(capture_window_instance_id);
            if !destroyed_window_matches_current(destroyed_instance, current_instance) {
                return;
            }
            let Some(session_id) = store.active_session().map(|session| session.id) else {
                return;
            };
            // `destroy()` bypasses CloseRequested. Keep the instance check and
            // owner cleanup in one lifecycle critical section so this stale
            // event cannot close a replacement window or session.
            let _ = cancel_screenshot_capture_locked(&app, &store, Some(&session_id));
        });
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

/// Completes cleanup while the caller holds `ScreenshotStore::lock_start()`.
/// Long screenshot export takes that lock before its job operation lock so it
/// cannot deadlock with ordinary screenshot cancellation.
pub(crate) fn finish_capture_after_cleanup_locked(
    app: &AppHandle,
    store: &ScreenshotStore,
    session_id: &str,
    cleanup: impl FnOnce(),
) {
    cleanup();
    finish_capture_unlocked(app, store, session_id);
}

trait TopmostWindow: Clone {
    fn topmost_state(&self) -> Result<bool, String>;
    fn set_topmost_state(&self, always_on_top: bool) -> Result<(), String>;
}

impl TopmostWindow for WebviewWindow {
    fn topmost_state(&self) -> Result<bool, String> {
        self.is_always_on_top().map_err(|error| error.to_string())
    }

    fn set_topmost_state(&self, always_on_top: bool) -> Result<(), String> {
        self.set_always_on_top(always_on_top)
            .map_err(|error| error.to_string())
    }
}

struct AlwaysOnTopRestoreGuard<W: TopmostWindow> {
    window: Option<W>,
    restore_always_on_top: bool,
}

impl<W: TopmostWindow> AlwaysOnTopRestoreGuard<W> {
    fn lower(window: Option<&W>) -> AppResult<Self> {
        let Some(window) = window else {
            return Ok(Self {
                window: None,
                restore_always_on_top: false,
            });
        };
        let restore_always_on_top = window.topmost_state().map_err(|error| {
            AppError::new("window_error", format!("读取截图窗口置顶状态失败: {error}"))
        })?;
        if restore_always_on_top {
            window.set_topmost_state(false).map_err(|error| {
                AppError::new(
                    "window_error",
                    format!("打开保存窗口前临时取消截图窗口置顶失败: {error}"),
                )
            })?;
        }
        Ok(Self {
            window: Some(window.clone()),
            restore_always_on_top,
        })
    }

    fn restore(mut self) -> AppResult<()> {
        if self.restore_always_on_top {
            if let Some(window) = &self.window {
                window.set_topmost_state(true).map_err(|error| {
                    AppError::new("window_error", format!("恢复截图窗口置顶状态失败: {error}"))
                })?;
            }
            self.restore_always_on_top = false;
        }
        Ok(())
    }
}

impl<W: TopmostWindow> Drop for AlwaysOnTopRestoreGuard<W> {
    fn drop(&mut self) {
        if self.restore_always_on_top {
            if let Some(window) = &self.window {
                let _ = window.set_topmost_state(true);
            }
        }
    }
}

pub(crate) fn choose_screenshot_save_path(
    app: &AppHandle,
    kind: ScreenshotSaveKind,
    parent_label: Option<&str>,
) -> AppResult<Option<PathBuf>> {
    let store = app.state::<ScreenshotStore>();
    let settings = store.settings();
    let parent_window = parent_label.and_then(|label| app.get_webview_window(label));
    // All callers execute this helper on a blocking worker. Use rfd directly
    // so opening the native dialog does not synchronously dispatch back to
    // Tauri's event loop and then wait forever if that dispatch is rejected.
    let mut dialog = rfd::FileDialog::new()
        .add_filter("PNG 图片", &["png"])
        .set_title(kind.dialog_title())
        .set_file_name(kind.file_name());
    if let Some(directory) = settings
        .last_save_directory
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_dir())
    {
        dialog = dialog.set_directory(directory);
    }
    if let Some(parent) = &parent_window {
        dialog = dialog.set_parent(parent);
    }
    let selected = {
        // The capture and pin windows are normally topmost. A native dialog
        // without an owner can be created behind them and look like a hung
        // export, so bind it to the invoking window and lower that window only
        // while the modal dialog is alive. The guard restores the prior state
        // for success, cancellation, and path-validation errors.
        let topmost_guard = AlwaysOnTopRestoreGuard::lower(parent_window.as_ref())?;
        let selected = dialog.save_file();
        topmost_guard.restore()?;
        selected
    };
    let Some(path) = selected else {
        return Ok(None);
    };
    let mut path = path;
    if path.extension().is_none() {
        path.set_extension("png");
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err(AppError::invalid(format!(
            "{}只能保存为 PNG 文件",
            kind.display_name()
        )));
    }
    store.update_last_save_directory(&path)?;
    Ok(Some(path))
}

fn ensure_capture_window(app: &AppHandle) -> AppResult<()> {
    if app.get_webview_window(CAPTURE_WINDOW_LABEL).is_some() {
        return Ok(());
    }
    let _creation_guard = lock_unpoisoned(&CAPTURE_WINDOW_CREATION_LOCK);
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

trait CaptureWindowActivation {
    fn show_for_capture(&self) -> AppResult<()>;
    fn unminimize_for_capture(&self) -> AppResult<()>;
    fn focus_for_capture(&self) -> AppResult<()>;
    fn ensure_foreground_for_capture(&self) -> AppResult<()>;
    fn hide_after_failed_capture(&self);
}

impl CaptureWindowActivation for WebviewWindow {
    fn show_for_capture(&self) -> AppResult<()> {
        self.show()
            .map_err(|error| AppError::new("window_error", format!("显示截图窗口失败: {error}")))
    }

    fn unminimize_for_capture(&self) -> AppResult<()> {
        self.unminimize()
            .map_err(|error| AppError::new("window_error", format!("恢复截图窗口失败: {error}")))
    }

    fn focus_for_capture(&self) -> AppResult<()> {
        self.set_focus()
            .map_err(|error| AppError::new("window_error", format!("聚焦截图窗口失败: {error}")))
    }

    fn ensure_foreground_for_capture(&self) -> AppResult<()> {
        ensure_capture_window_foreground(self)
    }

    fn hide_after_failed_capture(&self) {
        let _ = self.hide();
    }
}

fn present_capture_window_steps(window: &impl CaptureWindowActivation) -> AppResult<()> {
    let result = (|| {
        window.show_for_capture()?;
        window.unminimize_for_capture()?;
        // Activate the top-level HWND first. Calling Tauri's focus helper before
        // Win32 activation can leave the previous PetalDesk WebView as the active
        // child when the shortcut was pressed inside the app.
        window.ensure_foreground_for_capture()?;
        // A top-level SetFocus(hwnd) is not enough for WebView2: the final focus
        // call lets Tauri focus the embedded WebView child after activation.
        window.focus_for_capture()
    })();
    if result.is_err() {
        window.hide_after_failed_capture();
    }
    result
}

fn input_attachment_plan(
    current_thread: u32,
    foreground_thread: u32,
    target_thread: u32,
) -> (bool, bool) {
    let attach_foreground =
        current_thread != 0 && foreground_thread != 0 && current_thread != foreground_thread;
    let attach_target = current_thread != 0
        && target_thread != 0
        && current_thread != target_thread
        && foreground_thread != target_thread;
    (attach_foreground, attach_target)
}

fn ensure_foreground_with(
    mut is_foreground: impl FnMut() -> bool,
    activate_directly: impl FnOnce() -> AppResult<()>,
    activate_with_input_attachment: impl FnOnce() -> AppResult<()>,
) -> AppResult<()> {
    if is_foreground() {
        return Ok(());
    }
    activate_directly()?;
    if is_foreground() {
        return Ok(());
    }
    activate_with_input_attachment()?;
    if is_foreground() {
        Ok(())
    } else {
        Err(AppError::new(
            "window_activation_failed",
            "Windows 未能激活截图窗口，请重试截图",
        ))
    }
}

#[cfg(windows)]
fn ensure_capture_window_foreground(window: &WebviewWindow) -> AppResult<()> {
    use crate::window_activation::ThreadInputAttachment;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
        SetForegroundWindow,
    };

    let hwnd = window
        .hwnd()
        .map_err(|error| AppError::new("window_error", format!("读取截图窗口句柄失败: {error}")))?;
    let hwnd = hwnd.0 as *mut std::ffi::c_void;
    if unsafe { IsWindow(hwnd) } == 0 {
        return Err(AppError::new("window_error", "截图窗口句柄已失效"));
    }
    let target_thread = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
    if target_thread == 0 {
        return Err(AppError::new("window_error", "无法读取截图窗口输入线程"));
    }

    let activate_directly = || {
        unsafe {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetActiveWindow(hwnd);
            let _ = SetFocus(hwnd);
        }
        Ok(())
    };
    let activate_with_input_attachment = || {
        // Re-check before attaching: the normal activation above often wins
        // as soon as Windows processes the foreground request.
        if unsafe { GetForegroundWindow() } == hwnd {
            return Ok(());
        }
        let current_thread = unsafe { GetCurrentThreadId() };
        let foreground = unsafe { GetForegroundWindow() };
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            unsafe { GetWindowThreadProcessId(foreground, std::ptr::null_mut()) }
        };
        let (attach_foreground, attach_target) =
            input_attachment_plan(current_thread, foreground_thread, target_thread);
        let _foreground_attachment = if attach_foreground {
            ThreadInputAttachment::attach(
                current_thread,
                foreground_thread,
                "连接截图前台输入线程",
            )?
        } else {
            None
        };
        // When the shortcut came from another PetalDesk WebView, the target
        // capture window may have a different GUI thread in the same process.
        // Joining only the foreground thread leaves SetForegroundWindow able
        // to raise the frame but does not transfer keyboard focus to WebView2.
        let _target_attachment = if attach_target {
            ThreadInputAttachment::attach(current_thread, target_thread, "连接截图目标输入线程")?
        } else {
            None
        };
        unsafe {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetActiveWindow(hwnd);
            let _ = SetFocus(hwnd);
        }
        Ok(())
    };

    ensure_foreground_with(
        || unsafe { GetForegroundWindow() } == hwnd,
        activate_directly,
        activate_with_input_attachment,
    )
}

#[cfg(not(windows))]
fn ensure_capture_window_foreground(_window: &WebviewWindow) -> AppResult<()> {
    Ok(())
}

pub(crate) fn present_capture_window(app: &AppHandle) -> AppResult<()> {
    let window = app
        .get_webview_window(CAPTURE_WINDOW_LABEL)
        .ok_or_else(|| AppError::new("window_error", "截图窗口尚未准备完成"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let should_present = Arc::new(AtomicBool::new(true));
    let main_thread_should_present = should_present.clone();
    app.run_on_main_thread(move || {
        let result = if main_thread_should_present.load(Ordering::Acquire) {
            present_capture_window_steps(&window)
        } else {
            Err(AppError::new(
                "window_activation_canceled",
                "截图窗口显示请求已超时取消",
            ))
        };
        let _ = sender.send(result);
    })
    .map_err(|error| AppError::new("window_error", format!("调度截图窗口到主线程失败: {error}")))?;

    match receiver.recv_timeout(CAPTURE_PRESENT_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            should_present.store(false, Ordering::Release);
            Err(AppError::new(
                "window_activation_timeout",
                "等待截图窗口显示超时，请重试截图",
            ))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(AppError::new("window_error", "截图窗口主线程任务意外结束"))
        }
    }
}

fn open_pin_window(app: AppHandle, pin_id: &str) -> AppResult<String> {
    let _creation_guard = lock_unpoisoned(&PIN_WINDOW_CREATION_LOCK);
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

#[cfg(windows)]
fn capture_cursor_monitor(app: &AppHandle) -> AppResult<(MonitorBounds, Vec<u8>)> {
    let (mut bounds, bgra) = capture_cursor_monitor_bgra()?;
    bounds.scale_factor = monitor_scale_factor(app, &bounds);
    let rgba = bgra_to_rgba(&bgra)?;
    let png = encode_rgba_png(bounds.width, bounds.height, &rgba)?;
    Ok((bounds, png))
}

#[cfg(target_os = "macos")]
fn capture_cursor_monitor(app: &AppHandle) -> AppResult<(MonitorBounds, Vec<u8>)> {
    let (bounds, rgba) = capture_cursor_monitor_rgba(app)?;
    let png = encode_rgba_png(bounds.width, bounds.height, &rgba)?;
    Ok((bounds, png))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn capture_cursor_monitor(_app: &AppHandle) -> AppResult<(MonitorBounds, Vec<u8>)> {
    Err(AppError::new(
        "unsupported_platform",
        "截图仅支持 Windows 10/11 和 macOS 12 及以上版本",
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn capture_cursor_monitor_rgba(app: &AppHandle) -> AppResult<(MonitorBounds, Vec<u8>)> {
    if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        let _ = objc2_core_graphics::CGRequestScreenCaptureAccess();
        return Err(AppError::new(
            "screen_recording_permission",
            "请在系统设置 > 隐私与安全性 > 屏幕录制中允许 PetalDesk，然后重新打开应用后再截图",
        ));
    }
    let cursor = app
        .cursor_position()
        .map_err(|error| AppError::new("capture_error", format!("读取鼠标位置失败: {error}")))?;
    // Tao reports the global cursor in physical coordinates based on the main
    // display scale. CoreGraphics, which xcap uses, expects global points.
    let main_scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(1.0);
    let logical_x = (cursor.x / main_scale).round();
    let logical_y = (cursor.y / main_scale).round();
    let point_x = i32::try_from(logical_x as i64)
        .map_err(|_| AppError::new("capture_error", "鼠标横坐标超过截图范围"))?;
    let point_y = i32::try_from(logical_y as i64)
        .map_err(|_| AppError::new("capture_error", "鼠标纵坐标超过截图范围"))?;
    let monitor = xcap::Monitor::from_point(point_x, point_y).map_err(|error| {
        AppError::new("capture_error", format!("定位鼠标所在显示器失败: {error}"))
    })?;
    let logical_monitor_x = monitor
        .x()
        .map_err(|error| AppError::new("capture_error", format!("读取显示器位置失败: {error}")))?;
    let logical_monitor_y = monitor
        .y()
        .map_err(|error| AppError::new("capture_error", format!("读取显示器位置失败: {error}")))?;
    let logical_width = monitor
        .width()
        .map_err(|error| AppError::new("capture_error", format!("读取显示器宽度失败: {error}")))?;
    let logical_height = monitor
        .height()
        .map_err(|error| AppError::new("capture_error", format!("读取显示器高度失败: {error}")))?;
    let xcap_scale = f64::from(monitor.scale_factor().map_err(|error| {
        AppError::new("capture_error", format!("读取显示器缩放比例失败: {error}"))
    })?);
    let image = monitor.capture_image().map_err(|error| {
        AppError::new(
            "screen_recording_permission",
            format!(
                "无法捕获屏幕。请在系统设置 > 隐私与安全性 > 屏幕录制中允许 PetalDesk，然后重新打开应用。详情: {error}"
            ),
        )
    })?;
    let width = image.width();
    let height = image.height();
    let expected = checked_rgba_len(width, height)?;
    let rgba = image.into_raw();
    if rgba.len() != expected {
        return Err(AppError::new("capture_error", "macOS 截图像素长度无效"));
    }

    let tauri_monitor = app.available_monitors().ok().and_then(|monitors| {
        monitors.into_iter().find(|candidate| {
            let scale = candidate.scale_factor();
            if !scale.is_finite() || scale <= 0.0 {
                return false;
            }
            let position = candidate.position();
            let logical_candidate_x = f64::from(position.x) / scale;
            let logical_candidate_y = f64::from(position.y) / scale;
            (logical_candidate_x - f64::from(logical_monitor_x)).abs() <= 1.0
                && (logical_candidate_y - f64::from(logical_monitor_y)).abs() <= 1.0
                && candidate.size().width.abs_diff(width) <= 2
                && candidate.size().height.abs_diff(height) <= 2
        })
    });
    let scale_factor = tauri_monitor
        .as_ref()
        .map(|candidate| candidate.scale_factor())
        .unwrap_or(xcap_scale)
        .max(1.0);
    let (x, y) = tauri_monitor
        .map(|candidate| (candidate.position().x, candidate.position().y))
        .unwrap_or_else(|| {
            (
                (f64::from(logical_monitor_x) * scale_factor).round() as i32,
                (f64::from(logical_monitor_y) * scale_factor).round() as i32,
            )
        });
    Ok((
        MonitorBounds {
            x,
            y,
            width,
            height,
            scale_factor,
        },
        rgba,
    ))
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

#[cfg(not(any(windows, target_os = "macos")))]
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

#[cfg(target_os = "macos")]
fn write_png_to_clipboard(_png: &[u8], decoded: &DecodedPng) -> AppResult<()> {
    use std::borrow::Cow;

    let expected = checked_rgba_len(decoded.width, decoded.height)?;
    if decoded.rgba.len() != expected {
        return Err(AppError::new("invalid_png", "剪贴板图片像素长度无效"));
    }
    let mut clipboard = arboard::Clipboard::new().map_err(|error| {
        AppError::new("clipboard_error", format!("打开 macOS 剪贴板失败: {error}"))
    })?;
    clipboard
        .set_image(arboard::ImageData {
            width: decoded.width as usize,
            height: decoded.height as usize,
            bytes: Cow::Borrowed(&decoded.rgba),
        })
        .map_err(|error| {
            AppError::new(
                "clipboard_error",
                format!("写入 macOS 剪贴板图片失败: {error}"),
            )
        })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn write_png_to_clipboard(_png: &[u8], _decoded: &DecodedPng) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "截图剪贴板第一阶段仅支持 Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use tempfile::tempdir;

    struct FakeCaptureWindow {
        operations: RefCell<Vec<&'static str>>,
        fail_at: Option<&'static str>,
    }

    impl FakeCaptureWindow {
        fn new(fail_at: Option<&'static str>) -> Self {
            Self {
                operations: RefCell::new(Vec::new()),
                fail_at,
            }
        }

        fn run(&self, operation: &'static str) -> AppResult<()> {
            self.operations.borrow_mut().push(operation);
            if self.fail_at == Some(operation) {
                Err(AppError::new("window_error", format!("{operation} failed")))
            } else {
                Ok(())
            }
        }
    }

    impl CaptureWindowActivation for FakeCaptureWindow {
        fn show_for_capture(&self) -> AppResult<()> {
            self.run("show")
        }

        fn unminimize_for_capture(&self) -> AppResult<()> {
            self.run("unminimize")
        }

        fn focus_for_capture(&self) -> AppResult<()> {
            self.run("focus")
        }

        fn ensure_foreground_for_capture(&self) -> AppResult<()> {
            self.run("foreground")
        }

        fn hide_after_failed_capture(&self) {
            self.operations.borrow_mut().push("hide");
        }
    }

    #[test]
    fn capture_window_presentation_runs_every_step_in_strict_order() {
        let window = FakeCaptureWindow::new(None);

        present_capture_window_steps(&window).unwrap();

        assert_eq!(
            *window.operations.borrow(),
            vec!["show", "unminimize", "foreground", "focus"]
        );
    }

    #[test]
    fn capture_window_presentation_stops_and_propagates_the_first_error() {
        let window = FakeCaptureWindow::new(Some("unminimize"));

        let error = present_capture_window_steps(&window).unwrap_err();

        assert_eq!(error.code, "window_error");
        assert!(error.message.contains("unminimize failed"));
        assert_eq!(
            *window.operations.borrow(),
            vec!["show", "unminimize", "hide"]
        );
    }

    #[test]
    fn foreground_activation_avoids_input_attachment_until_direct_activation_fails() {
        let foreground = Cell::new(true);
        let direct_calls = Cell::new(0);
        let attached_calls = Cell::new(0);
        ensure_foreground_with(
            || foreground.get(),
            || {
                direct_calls.set(direct_calls.get() + 1);
                Ok(())
            },
            || {
                attached_calls.set(attached_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(direct_calls.get(), 0);
        assert_eq!(attached_calls.get(), 0);

        foreground.set(false);
        ensure_foreground_with(
            || foreground.get(),
            || {
                direct_calls.set(direct_calls.get() + 1);
                foreground.set(true);
                Ok(())
            },
            || {
                attached_calls.set(attached_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(direct_calls.get(), 1);
        assert_eq!(attached_calls.get(), 0);
    }

    #[test]
    fn foreground_activation_attaches_only_after_direct_activation_is_insufficient() {
        let foreground = Cell::new(false);
        let direct_calls = Cell::new(0);
        let attached_calls = Cell::new(0);

        ensure_foreground_with(
            || foreground.get(),
            || {
                direct_calls.set(direct_calls.get() + 1);
                Ok(())
            },
            || {
                attached_calls.set(attached_calls.get() + 1);
                foreground.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(direct_calls.get(), 1);
        assert_eq!(attached_calls.get(), 1);
    }

    #[test]
    fn foreground_activation_reports_failure_after_both_attempts() {
        let error = ensure_foreground_with(|| false, || Ok(()), || Ok(())).unwrap_err();

        assert_eq!(error.code, "window_activation_failed");
    }

    #[test]
    fn foreground_activation_includes_target_thread_for_same_process_windows() {
        // A PetalDesk WebView can own a different GUI thread from the
        // screenshot window even though both windows belong to this process.
        assert_eq!(input_attachment_plan(10, 10, 20), (false, true));
        assert_eq!(input_attachment_plan(10, 30, 20), (true, true));
        assert_eq!(input_attachment_plan(10, 30, 30), (true, false));
        assert_eq!(input_attachment_plan(10, 0, 20), (false, true));
        assert_eq!(input_attachment_plan(0, 30, 20), (false, false));
    }

    #[derive(Clone)]
    struct FakeTopmostWindow {
        state: Arc<AtomicBool>,
        transitions: Arc<Mutex<Vec<bool>>>,
    }

    impl FakeTopmostWindow {
        fn new(always_on_top: bool) -> Self {
            Self {
                state: Arc::new(AtomicBool::new(always_on_top)),
                transitions: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl TopmostWindow for FakeTopmostWindow {
        fn topmost_state(&self) -> Result<bool, String> {
            Ok(self.state.load(Ordering::SeqCst))
        }

        fn set_topmost_state(&self, always_on_top: bool) -> Result<(), String> {
            self.state.store(always_on_top, Ordering::SeqCst);
            lock_unpoisoned(&self.transitions).push(always_on_top);
            Ok(())
        }
    }

    #[test]
    fn save_dialog_guard_restores_topmost_window_on_every_exit_path() {
        let explicit_window = FakeTopmostWindow::new(true);
        let guard = AlwaysOnTopRestoreGuard::lower(Some(&explicit_window)).unwrap();
        assert!(!explicit_window.state.load(Ordering::SeqCst));
        guard.restore().unwrap();
        assert!(explicit_window.state.load(Ordering::SeqCst));
        assert_eq!(
            *lock_unpoisoned(&explicit_window.transitions),
            vec![false, true]
        );

        let window = FakeTopmostWindow::new(true);
        {
            let _guard = AlwaysOnTopRestoreGuard::lower(Some(&window)).unwrap();
            assert!(!window.state.load(Ordering::SeqCst));
        }
        assert!(window.state.load(Ordering::SeqCst));
        assert_eq!(*lock_unpoisoned(&window.transitions), vec![false, true]);

        let ordinary_window = FakeTopmostWindow::new(false);
        drop(AlwaysOnTopRestoreGuard::lower(Some(&ordinary_window)).unwrap());
        assert!(!ordinary_window.state.load(Ordering::SeqCst));
        assert!(lock_unpoisoned(&ordinary_window.transitions).is_empty());
    }

    #[test]
    fn save_dialog_kind_uses_distinct_png_names_and_titles() {
        let screenshot_name = ScreenshotSaveKind::Screenshot.file_name();
        let long_name = ScreenshotSaveKind::LongScreenshot.file_name();
        assert!(screenshot_name.starts_with("PetalDesk截图-"));
        assert!(long_name.starts_with("PetalDesk长截图-"));
        assert!(screenshot_name.ends_with(".png"));
        assert!(long_name.ends_with(".png"));
        assert_eq!(
            ScreenshotSaveKind::Screenshot.dialog_title(),
            "保存截图 - 飞花 - PetalDesk"
        );
        assert_eq!(
            ScreenshotSaveKind::LongScreenshot.dialog_title(),
            "保存长截图 - 飞花 - PetalDesk"
        );
    }

    #[test]
    fn shortcut_refresh_registers_even_when_unregister_fails() {
        let unregister_called = AtomicBool::new(false);
        let register_called = AtomicBool::new(false);
        let result = refresh_shortcut_with(
            "F1",
            true,
            || {
                unregister_called.store(true, Ordering::SeqCst);
                Err("Windows registration was already lost".to_string())
            },
            || {
                register_called.store(true, Ordering::SeqCst);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(unregister_called.load(Ordering::SeqCst));
        assert!(register_called.load(Ordering::SeqCst));
    }

    #[test]
    fn shortcut_refresh_preserves_unregister_and_register_errors() {
        let error = refresh_shortcut_with(
            "Ctrl+F1",
            true,
            || Err("unregister failed".to_string()),
            || Err(AppError::new("shortcut_conflict", "register failed")),
        )
        .unwrap_err();

        assert_eq!(error.code, "shortcut_refresh_failed");
        assert!(error.message.contains("Ctrl+F1"));
        assert!(error.message.contains("unregister failed"));
        assert!(error.message.contains("register failed"));
    }

    #[test]
    fn shortcut_refresh_skips_unregister_when_plugin_has_no_registration() {
        let unregister_called = AtomicBool::new(false);
        let result = refresh_shortcut_with(
            "F1",
            false,
            || {
                unregister_called.store(true, Ordering::SeqCst);
                Ok(())
            },
            || Ok(()),
        );

        assert!(result.is_ok());
        assert!(!unregister_called.load(Ordering::SeqCst));
    }

    #[test]
    fn shortcut_health_check_retries_failures_and_periodically_rebinds_healthy_keys() {
        assert!(shortcut_health_check_due(true, 0));
        assert!(shortcut_health_check_due(true, 1));
        assert!(!shortcut_health_check_due(
            false,
            SHORTCUT_HEALTHY_REFRESH_POLLS - 1
        ));
        assert!(shortcut_health_check_due(
            false,
            SHORTCUT_HEALTHY_REFRESH_POLLS
        ));
    }

    #[test]
    fn shortcut_change_commits_new_key_when_old_key_cannot_be_unregistered() {
        let operations = Mutex::new(Vec::<String>::new());
        let warning = change_shortcut_with(
            "F1",
            "Ctrl+F1",
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("register:{shortcut}"));
                Ok(())
            },
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("unregister:{shortcut}"));
                Err("old key was already lost by Windows".to_string())
            },
            || {
                lock_unpoisoned(&operations).push("persist".to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            warning.as_deref(),
            Some("old key was already lost by Windows")
        );
        assert_eq!(
            *lock_unpoisoned(&operations),
            vec!["register:Ctrl+F1", "unregister:F1", "persist"]
        );
    }

    #[test]
    fn shortcut_change_keeps_new_key_when_persistence_and_old_key_recovery_fail() {
        let operations = Mutex::new(Vec::<String>::new());
        let error = change_shortcut_with(
            "F1",
            "Ctrl+F1",
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("register:{shortcut}"));
                if shortcut == "F1" {
                    Err(AppError::new("shortcut_conflict", "old key unavailable"))
                } else {
                    Ok(())
                }
            },
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("unregister:{shortcut}"));
                Ok(())
            },
            || Err(AppError::new("io_error", "disk full")),
        )
        .unwrap_err();

        assert_eq!(error.code, "shortcut_rollback_failed");
        assert!(error.message.contains("已保留可用的新快捷键 Ctrl+F1"));
        assert_eq!(
            *lock_unpoisoned(&operations),
            vec!["register:Ctrl+F1", "unregister:F1", "register:F1"]
        );
    }

    #[test]
    fn shortcut_change_removes_new_key_only_after_old_key_recovery_succeeds() {
        let operations = Mutex::new(Vec::<String>::new());
        let error = change_shortcut_with(
            "F1",
            "Ctrl+F1",
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("register:{shortcut}"));
                Ok(())
            },
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("unregister:{shortcut}"));
                Ok(())
            },
            || Err(AppError::new("io_error", "disk full")),
        )
        .unwrap_err();

        assert_eq!(error.code, "shortcut_persist_failed");
        assert!(error.message.contains("已恢复原快捷键 F1"));
        assert_eq!(
            *lock_unpoisoned(&operations),
            vec![
                "register:Ctrl+F1",
                "unregister:F1",
                "register:F1",
                "unregister:Ctrl+F1"
            ]
        );
    }

    #[test]
    fn shortcut_change_reports_when_recovered_old_key_cannot_clean_up_new_key() {
        let operations = Mutex::new(Vec::<String>::new());
        let error = change_shortcut_with(
            "F1",
            "Ctrl+F1",
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("register:{shortcut}"));
                Ok(())
            },
            |shortcut| {
                lock_unpoisoned(&operations).push(format!("unregister:{shortcut}"));
                if shortcut == "Ctrl+F1" {
                    Err("new-key cleanup failed".to_string())
                } else {
                    Ok(())
                }
            },
            || Err(AppError::new("io_error", "disk full")),
        )
        .unwrap_err();

        assert_eq!(error.code, "shortcut_rollback_cleanup_failed");
        assert!(error.message.contains("两个快捷键本次运行中可能都可用"));
        assert_eq!(
            *lock_unpoisoned(&operations),
            vec![
                "register:Ctrl+F1",
                "unregister:F1",
                "register:F1",
                "unregister:Ctrl+F1"
            ]
        );
    }

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

    #[test]
    fn delayed_destroy_event_cannot_target_a_replacement_capture_window() {
        assert!(destroyed_window_matches_current(Some(10), Some(10)));
        assert!(!destroyed_window_matches_current(Some(10), Some(11)));
        assert!(!destroyed_window_matches_current(None, Some(11)));
        assert!(destroyed_window_matches_current(Some(10), None));
        assert!(destroyed_window_matches_current(None, None));
    }
}

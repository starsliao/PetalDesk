use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::storage::WorkspaceStore;
use serde_json::json;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

pub(crate) const TIMER_WINDOW_LABEL: &str = "timer";
pub(crate) const REMINDER_WINDOW_LABEL: &str = "reminder";
pub(crate) const GANTT_WINDOW_LABEL: &str = "gantt";
pub(crate) const MFA_WINDOW_LABEL: &str = "mfa";
pub(crate) const PASSWORD_WINDOW_LABEL: &str = "passwords";
pub(crate) const SENSITIVE_TOOL_REMOTE_SESSION_CODE: &str =
    "remote_desktop_sensitive_window_unavailable";
const SENSITIVE_TOOL_REMOTE_SESSION_MESSAGE: &str =
    "远程桌面会隐藏 MFA 验证器和密码管理器的敏感内容，因此当前不会打开窗口。请在本机登录后使用。";
const TIMER_DEFAULT_WIDTH: f64 = 320.0;
const TIMER_DEFAULT_HEIGHT: f64 = 140.0;
const TIMER_MIN_VISIBLE_WIDTH: f64 = 48.0;
const TIMER_MIN_VISIBLE_HEIGHT: f64 = 32.0;

// Tauri's window registry does not reserve a label atomically between
// `get_webview_window` and `WebviewWindowBuilder::build`. Keep independent
// creation domains so a slow WebView2 controller cannot block every note,
// tool and screenshot window in the process.
static NOTE_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static TIMER_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static REMINDER_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static GANTT_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static MFA_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());
static PASSWORD_WINDOW_CREATION_LOCK: Mutex<()> = Mutex::new(());

const SLOW_BACKGROUND_OPERATION: Duration = Duration::from_secs(2);

/// Tauri executes synchronous commands on its event-loop thread. Filesystem,
/// SQLite and native window construction can block unpredictably on Windows,
/// so keep those operations off the thread that owns clicks, tray events and
/// global shortcuts.
pub(crate) async fn run_background<T, F>(operation: &'static str, task: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AppResult<T> + Send + 'static,
{
    let started = Instant::now();
    let result = tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| {
            AppError::new(
                "background_task_error",
                format!("{operation}任务异常结束: {error}"),
            )
        })?;
    let elapsed = started.elapsed();
    if elapsed >= SLOW_BACKGROUND_OPERATION {
        eprintln!("后台操作耗时过长: {operation} ({elapsed:?})");
    }
    result
}

#[cfg(test)]
mod background_command_tests {
    use super::*;

    #[test]
    fn blocking_command_work_does_not_run_on_the_caller_thread() {
        let caller = std::thread::current().id();
        let worker = tauri::async_runtime::block_on(run_background("测试后台任务", || {
            Ok(std::thread::current().id())
        }))
        .unwrap();
        assert_ne!(worker, caller);
    }
}

fn lock_window_creation(lock: &'static Mutex<()>) -> MutexGuard<'static, ()> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy)]
struct LogicalWorkArea {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn timer_work_areas(app: &AppHandle) -> Vec<LogicalWorkArea> {
    app.available_monitors()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|monitor| {
            let scale = monitor.scale_factor();
            if !scale.is_finite() || scale <= 0.0 {
                return None;
            }
            let work_area = monitor.work_area();
            Some(LogicalWorkArea {
                x: f64::from(work_area.position.x) / scale,
                y: f64::from(work_area.position.y) / scale,
                width: f64::from(work_area.size.width) / scale,
                height: f64::from(work_area.size.height) / scale,
            })
        })
        .collect()
}

fn timer_state_is_visible(state: &WindowState, work_areas: &[LogicalWorkArea]) -> bool {
    if work_areas.is_empty()
        || !state.x.is_finite()
        || !state.y.is_finite()
        || !state.width.is_finite()
        || !state.height.is_finite()
        || state.width < WorkspaceStore::TIMER_MIN_WIDTH
        || state.height < WorkspaceStore::TIMER_MIN_HEIGHT
    {
        return false;
    }

    let right = state.x + state.width;
    let bottom = state.y + state.height;
    if !right.is_finite() || !bottom.is_finite() {
        return false;
    }

    let max_work_width = work_areas.iter().map(|area| area.width).fold(0.0, f64::max);
    let max_work_height = work_areas
        .iter()
        .map(|area| area.height)
        .fold(0.0, f64::max);
    if state.width > max_work_width * 2.0 || state.height > max_work_height * 2.0 {
        return false;
    }

    let required_width = state.width.min(TIMER_MIN_VISIBLE_WIDTH);
    let required_height = state.height.min(TIMER_MIN_VISIBLE_HEIGHT);
    work_areas.iter().any(|area| {
        let intersection_width = right.min(area.x + area.width) - state.x.max(area.x);
        let intersection_height = bottom.min(area.y + area.height) - state.y.max(area.y);
        intersection_width >= required_width && intersection_height >= required_height
    })
}

#[tauri::command]
pub fn get_app_info(store: State<'_, WorkspaceStore>) -> AppInfo {
    AppInfo {
        name: "飞花 - PetalDesk".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_path: store.workspace_path().to_string_lossy().into_owned(),
        default_editor_mode: store.default_editor_mode(),
        tray_shortcut_settings: store.tray_shortcut_settings(),
        colors: ALLOWED_COLORS
            .iter()
            .map(|color| (*color).to_string())
            .collect(),
        recovered_drafts: store.startup_recovery().len(),
    }
}

#[tauri::command]
pub async fn set_tray_shortcut_settings(
    app: AppHandle,
    settings: TrayShortcutSettings,
) -> AppResult<TrayShortcutSettings> {
    run_background("设置托盘双击动作", move || {
        app.state::<WorkspaceStore>()
            .set_tray_shortcut_settings(settings)
    })
    .await
}

#[tauri::command]
pub async fn set_default_editor_mode(
    app: AppHandle,
    default_editor_mode: String,
) -> AppResult<String> {
    run_background("设置默认编辑样式", move || {
        let default_editor_mode = app
            .state::<WorkspaceStore>()
            .set_default_editor_mode(&default_editor_mode)?;
        let _ = app.emit(
            "default_editor_mode_changed",
            json!({ "mode": default_editor_mode.clone() }),
        );
        Ok(default_editor_mode)
    })
    .await
}

#[tauri::command]
pub async fn set_data_storage_path(
    app: AppHandle,
    path: String,
) -> AppResult<DataStorageChangeResult> {
    run_background("设置数据存储路径", move || {
        app.state::<WorkspaceStore>().set_data_storage_path(path)
    })
    .await
}

#[tauri::command]
pub async fn restart_app(app: AppHandle) -> AppResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<crate::mfa::MfaStore>().lock();
        app.state::<crate::passwords::PasswordStore>().lock();
        crate::long_screenshot::shutdown(&app);
        app.restart()
    })
    .await
    .map_err(|error| AppError::new("restart_error", format!("重启应用失败: {error}")))?;
    Ok(())
}

#[tauri::command]
pub async fn list_notes(app: AppHandle) -> AppResult<Vec<NoteSummary>> {
    run_background("读取便签列表", move || {
        app.state::<WorkspaceStore>().list_notes()
    })
    .await
}

#[tauri::command]
pub async fn reorder_notes(
    app: AppHandle,
    ordered_ids: Vec<String>,
) -> AppResult<Vec<NoteSummary>> {
    run_background("调整便签顺序", move || {
        let notes = app.state::<WorkspaceStore>().reorder_notes(ordered_ids)?;
        let _ = app.emit("notes_reordered", json!({}));
        crate::refresh_tray_menu(&app);
        Ok(notes)
    })
    .await
}

#[tauri::command]
pub async fn create_note(app: AppHandle) -> AppResult<NoteSnapshot> {
    run_background("创建便签", move || {
        let note = app.state::<WorkspaceStore>().create_note()?;
        let _ = app.emit("note_changed", json!({ "id": note.id, "kind": "created" }));
        crate::refresh_tray_menu(&app);
        Ok(note)
    })
    .await
}

#[tauri::command]
pub async fn get_note(app: AppHandle, note_id: String) -> AppResult<NoteSnapshot> {
    run_background("读取便签", move || {
        app.state::<WorkspaceStore>().get_note(&note_id)
    })
    .await
}

#[tauri::command]
pub async fn commit_note(app: AppHandle, request: CommitNoteRequest) -> AppResult<CommitResult> {
    run_background("保存便签", move || {
        let store = app.state::<WorkspaceStore>();
        let note_id = request.id.clone();
        let tray_menu_changed =
            request.meta_patch.title.is_some() || request.meta_patch.pinned.is_some();
        match store.commit_note(request) {
            Ok(result) => {
                let _ = app.emit(
                    "note_changed",
                    json!({ "id": note_id, "kind": "committed", "revision": result.revision }),
                );
                if let Some(window) = app.get_webview_window(&format!("note-{note_id}")) {
                    if let Ok(snapshot) = store.get_note(&note_id) {
                        let _ = window.set_always_on_top(snapshot.meta.pinned);
                    }
                }
                if tray_menu_changed {
                    crate::refresh_tray_menu(&app);
                }
                Ok(result)
            }
            Err(error) => {
                if error.code == "revision_conflict" {
                    let _ = app.emit(
                        "conflict_detected",
                        json!({ "id": note_id, "error": error.clone() }),
                    );
                }
                Err(error)
            }
        }
    })
    .await
}

#[tauri::command]
pub async fn delete_note(app: AppHandle, note_id: String) -> AppResult<()> {
    run_background("删除便签", move || {
        let store = app.state::<WorkspaceStore>();
        store.delete_note(&note_id)?;
        store.set_note_window_open(&note_id, false)?;
        if let Some(window) = app.get_webview_window(&format!("note-{note_id}")) {
            let _ = window.destroy();
        }
        let _ = app.emit("note_changed", json!({ "id": note_id, "kind": "deleted" }));
        crate::refresh_tray_menu(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn list_trash(app: AppHandle) -> AppResult<Vec<NoteSummary>> {
    run_background("读取回收站", move || {
        app.state::<WorkspaceStore>().list_trash()
    })
    .await
}

#[tauri::command]
pub async fn restore_note(app: AppHandle, note_id: String) -> AppResult<NoteSnapshot> {
    run_background("恢复便签", move || {
        let note = app.state::<WorkspaceStore>().restore_note(&note_id)?;
        let _ = app.emit("note_changed", json!({ "id": note_id, "kind": "restored" }));
        crate::refresh_tray_menu(&app);
        Ok(note)
    })
    .await
}

#[tauri::command]
pub async fn empty_trash(app: AppHandle) -> AppResult<()> {
    run_background("清空回收站", move || {
        app.state::<WorkspaceStore>().empty_trash()
    })
    .await
}

#[tauri::command]
pub async fn import_asset(
    app: AppHandle,
    note_id: String,
    file_name: String,
    bytes: Vec<u8>,
) -> AppResult<AssetResult> {
    run_background("导入便签图片", move || {
        let asset = app
            .state::<WorkspaceStore>()
            .import_asset(&note_id, &file_name, &bytes)?;
        let _ = app.emit(
            "asset_imported",
            json!({ "noteId": note_id, "asset": asset.clone() }),
        );
        Ok(asset)
    })
    .await
}

#[tauri::command]
pub async fn read_asset(
    app: AppHandle,
    note_id: String,
    relative_path: String,
) -> AppResult<AssetContent> {
    run_background("读取便签图片", move || {
        app.state::<WorkspaceStore>()
            .read_asset(&note_id, &relative_path)
    })
    .await
}

#[tauri::command]
pub async fn search_notes(
    app: AppHandle,
    query: String,
    limit: Option<u32>,
) -> AppResult<Vec<SearchResult>> {
    run_background("搜索便签", move || {
        app.state::<WorkspaceStore>().search_notes(&query, limit)
    })
    .await
}

#[tauri::command]
pub async fn open_note_window(app: AppHandle, note_id: String) -> AppResult<String> {
    run_background("打开便签窗口", move || {
        open_note_window_inner(&app, &app.state(), &note_id)
    })
    .await
}

#[tauri::command]
pub async fn open_tool_window(app: AppHandle, tool: ToolName) -> AppResult<String> {
    run_background("打开小工具窗口", move || match tool {
        ToolName::Timer => open_timer_window_inner(&app, &app.state()),
        ToolName::Reminder => open_reminder_window_inner(&app, &app.state()),
        ToolName::Gantt => open_gantt_window_inner(&app, &app.state()),
        ToolName::Mfa => open_mfa_window_inner(&app, &app.state()),
        ToolName::Passwords => {
            open_password_window(&app).map(|_| PASSWORD_WINDOW_LABEL.to_string())
        }
        ToolName::Screenshot => crate::screenshot::start_capture_inner(&app)
            .map(|_| crate::screenshot::CAPTURE_WINDOW_LABEL.to_string()),
    })
    .await
}

pub fn open_note_window_inner(
    app: &AppHandle,
    store: &WorkspaceStore,
    note_id: &str,
) -> AppResult<String> {
    crate::trace_activation(&format!("open_note:{note_id}:waiting_creation_lock"));
    let _creation_guard = lock_window_creation(&NOTE_WINDOW_CREATION_LOCK);
    crate::trace_activation(&format!("open_note:{note_id}:creation_lock_acquired"));
    let label = format!("note-{note_id}");
    if let Some(window) = app.get_webview_window(&label) {
        crate::trace_activation(&format!("open_note:{note_id}:existing_window"));
        window
            .show()
            .map_err(|error| AppError::new("window_error", format!("显示便签窗口失败: {error}")))?;
        let _ = window.unminimize();
        let _ = window.set_focus();
        if let Ok(snapshot) = store.get_note(note_id) {
            let _ = window.set_always_on_top(snapshot.meta.pinned);
        }
        store.set_note_window_open(note_id, true)?;
        crate::trace_activation(&format!("open_note:{note_id}:existing_window_shown"));
        return Ok(label);
    }

    crate::trace_activation(&format!("open_note:{note_id}:snapshot_start"));
    let snapshot = store.get_note(note_id)?;
    crate::trace_activation(&format!("open_note:{note_id}:snapshot_end"));
    let url = WebviewUrl::App(format!("?note={note_id}").into());
    let mut builder = WebviewWindowBuilder::new(app, &label, url)
        .title(format!("{} - 飞花 - PetalDesk", snapshot.meta.title))
        .decorations(false)
        .resizable(true)
        .inner_size(420.0, 420.0)
        .min_inner_size(280.0, 220.0)
        .always_on_top(snapshot.meta.pinned)
        .skip_taskbar(false);
    if let Some(state) = store.window_state(&label) {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height)
            .maximized(state.maximized);
    }
    crate::trace_activation(&format!("open_note:{note_id}:build_start"));
    let window = builder
        .build()
        .map_err(|error| AppError::new("window_error", format!("创建便签窗口失败: {error}")))?;
    crate::trace_activation(&format!("open_note:{note_id}:build_end"));
    let _ = window.set_focus();
    store.set_note_window_open(note_id, true)?;
    crate::trace_activation(&format!("open_note:{note_id}:state_saved"));
    Ok(label)
}

pub fn open_timer_window_inner(app: &AppHandle, store: &WorkspaceStore) -> AppResult<String> {
    let _creation_guard = lock_window_creation(&TIMER_WINDOW_CREATION_LOCK);
    if let Some(window) = app.get_webview_window(TIMER_WINDOW_LABEL) {
        window
            .show()
            .map_err(|error| AppError::new("window_error", format!("显示计时器失败: {error}")))?;
        let _ = window.unminimize();
        let _ = window.set_always_on_top(true);
        let _ = window.set_focus();
        return Ok(TIMER_WINDOW_LABEL.to_string());
    }

    let url = WebviewUrl::App("?tool=timer".into());
    let saved_state = store
        .window_state(TIMER_WINDOW_LABEL)
        .filter(|state| timer_state_is_visible(state, &timer_work_areas(app)));
    let mut builder = WebviewWindowBuilder::new(app, TIMER_WINDOW_LABEL, url)
        .title("计时器 - 飞花 - PetalDesk")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(true)
        .inner_size(TIMER_DEFAULT_WIDTH, TIMER_DEFAULT_HEIGHT)
        .min_inner_size(
            WorkspaceStore::TIMER_MIN_WIDTH,
            WorkspaceStore::TIMER_MIN_HEIGHT,
        )
        .max_inner_size(
            WorkspaceStore::TIMER_MAX_WIDTH,
            WorkspaceStore::TIMER_MAX_HEIGHT,
        )
        .always_on_top(true)
        .skip_taskbar(true);
    if let Some(state) = saved_state {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height);
    } else {
        builder = builder.center();
    }
    let window = builder
        .build()
        .map_err(|error| AppError::new("window_error", format!("创建计时器失败: {error}")))?;
    let _ = window.set_focus();
    Ok(TIMER_WINDOW_LABEL.to_string())
}

#[cfg(test)]
mod timer_window_tests {
    use super::*;

    fn state(x: f64, y: f64, width: f64, height: f64) -> WindowState {
        WindowState {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    const PRIMARY_DISPLAY: [LogicalWorkArea; 1] = [LogicalWorkArea {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1040.0,
    }];

    #[test]
    fn restores_visible_timer_geometry() {
        assert!(timer_state_is_visible(
            &state(240.0, 180.0, 480.0, 260.0),
            &PRIMARY_DISPLAY,
        ));
        assert!(timer_state_is_visible(
            &state(-20.0, 980.0, 320.0, 140.0),
            &PRIMARY_DISPLAY,
        ));
    }

    #[test]
    fn rejects_offscreen_or_implausible_timer_geometry() {
        assert!(!timer_state_is_visible(
            &state(2100.0, 100.0, 320.0, 140.0),
            &PRIMARY_DISPLAY,
        ));
        assert!(!timer_state_is_visible(
            &state(1900.0, 1025.0, 320.0, 140.0),
            &PRIMARY_DISPLAY,
        ));
        assert!(!timer_state_is_visible(
            &state(0.0, 0.0, 10_000.0, 10_000.0),
            &PRIMARY_DISPLAY,
        ));
        assert!(!timer_state_is_visible(
            &state(0.0, 0.0, f64::INFINITY, 140.0),
            &PRIMARY_DISPLAY,
        ));
    }

    #[test]
    fn rejects_saved_timer_geometry_when_monitor_detection_fails() {
        assert!(!timer_state_is_visible(&state(0.0, 0.0, 320.0, 140.0), &[],));
    }
}

pub fn open_reminder_window_inner(app: &AppHandle, store: &WorkspaceStore) -> AppResult<String> {
    let _creation_guard = lock_window_creation(&REMINDER_WINDOW_CREATION_LOCK);
    if let Some(window) = app.get_webview_window(REMINDER_WINDOW_LABEL) {
        window
            .show()
            .map_err(|error| AppError::new("window_error", format!("显示提醒工具失败: {error}")))?;
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(REMINDER_WINDOW_LABEL.to_string());
    }

    let url = WebviewUrl::App("?tool=reminder".into());
    let mut builder = WebviewWindowBuilder::new(app, REMINDER_WINDOW_LABEL, url)
        .title("提醒 - 飞花 - PetalDesk")
        .decorations(false)
        .resizable(true)
        .inner_size(560.0, 620.0)
        .min_inner_size(440.0, 360.0)
        .skip_taskbar(false);
    if let Some(state) = store.window_state(REMINDER_WINDOW_LABEL) {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height)
            .maximized(state.maximized);
    }
    let window = builder
        .build()
        .map_err(|error| AppError::new("window_error", format!("创建提醒工具失败: {error}")))?;
    let _ = window.set_focus();
    Ok(REMINDER_WINDOW_LABEL.to_string())
}

pub fn open_gantt_window_inner(app: &AppHandle, store: &WorkspaceStore) -> AppResult<String> {
    let _creation_guard = lock_window_creation(&GANTT_WINDOW_CREATION_LOCK);
    if let Some(window) = app.get_webview_window(GANTT_WINDOW_LABEL) {
        window.show().map_err(|error| {
            AppError::new("window_error", format!("显示任务甘特图失败: {error}"))
        })?;
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(GANTT_WINDOW_LABEL.to_string());
    }

    let url = WebviewUrl::App("?tool=gantt".into());
    let mut builder = WebviewWindowBuilder::new(app, GANTT_WINDOW_LABEL, url)
        .title("任务甘特图 - 飞花 - PetalDesk")
        .decorations(false)
        .resizable(true)
        .inner_size(980.0, 600.0)
        .min_inner_size(
            WorkspaceStore::GANTT_MIN_WIDTH,
            WorkspaceStore::GANTT_MIN_HEIGHT,
        )
        .skip_taskbar(false);
    if let Some(state) = store.window_state(GANTT_WINDOW_LABEL) {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height)
            .maximized(state.maximized);
    } else {
        builder = builder.center();
    }
    let window = builder
        .build()
        .map_err(|error| AppError::new("window_error", format!("创建任务甘特图失败: {error}")))?;
    let _ = window.set_focus();
    Ok(GANTT_WINDOW_LABEL.to_string())
}

pub fn open_mfa_window_inner(app: &AppHandle, store: &WorkspaceStore) -> AppResult<String> {
    let _creation_guard = lock_window_creation(&MFA_WINDOW_CREATION_LOCK);
    ensure_sensitive_tool_local(app, MFA_WINDOW_LABEL)?;
    if let Some(window) = app.get_webview_window(MFA_WINDOW_LABEL) {
        window.show().map_err(|error| {
            AppError::new("window_error", format!("显示 MFA 验证器失败: {error}"))
        })?;
        let _ = window.unminimize();
        app.state::<crate::mfa::MfaStore>().activate();
        let protected = protect_mfa_window(&window);
        app.state::<crate::mfa::MfaStore>()
            .set_capture_excluded(protected);
        let _ = window.set_focus();
        return Ok(MFA_WINDOW_LABEL.to_string());
    }

    let url = WebviewUrl::App("?tool=mfa".into());
    let mut builder = WebviewWindowBuilder::new(app, MFA_WINDOW_LABEL, url)
        .title("MFA 验证器 - 飞花 - PetalDesk")
        .decorations(false)
        .resizable(true)
        .inner_size(520.0, 640.0)
        .min_inner_size(400.0, 360.0)
        .skip_taskbar(false);
    if let Some(state) = store.window_state(MFA_WINDOW_LABEL) {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height)
            .maximized(state.maximized);
    } else {
        builder = builder.center();
    }
    // Activate before building: WebView2 can finish loading quickly enough to
    // invoke `get_mfa_status` before `build` returns on a warm start.
    let mfa_store = app.state::<crate::mfa::MfaStore>();
    mfa_store.activate();
    let window = match builder.build() {
        Ok(window) => window,
        Err(error) => {
            let closing_epoch = mfa_store.deactivate();
            mfa_store.clear_deactivated_state(closing_epoch);
            return Err(AppError::new(
                "window_error",
                format!("创建 MFA 验证器失败: {error}"),
            ));
        }
    };
    let protected = protect_mfa_window(&window);
    app.state::<crate::mfa::MfaStore>()
        .set_capture_excluded(protected);
    let _ = window.set_focus();
    Ok(MFA_WINDOW_LABEL.to_string())
}

/// Opens or focuses the password manager window. Shared by the tool command,
/// the tray menu, and the Firefox extension's openPasswordManager event.
pub(crate) fn open_password_window(app: &AppHandle) -> AppResult<()> {
    open_password_window_inner(app, &app.state()).map(|_| ())
}

pub fn open_password_window_inner(app: &AppHandle, store: &WorkspaceStore) -> AppResult<String> {
    let _creation_guard = lock_window_creation(&PASSWORD_WINDOW_CREATION_LOCK);
    ensure_sensitive_tool_local(app, PASSWORD_WINDOW_LABEL)?;
    if let Some(window) = app.get_webview_window(PASSWORD_WINDOW_LABEL) {
        window.show().map_err(|error| {
            AppError::new("window_error", format!("显示密码管理器失败: {error}"))
        })?;
        let _ = window.unminimize();
        app.state::<crate::passwords::PasswordStore>().activate();
        let _ = protect_sensitive_window(&window);
        let _ = window.set_focus();
        return Ok(PASSWORD_WINDOW_LABEL.to_string());
    }

    let url = WebviewUrl::App("?tool=passwords".into());
    let mut builder = WebviewWindowBuilder::new(app, PASSWORD_WINDOW_LABEL, url)
        .title("密码管理器 - 飞花 - PetalDesk")
        .decorations(false)
        .resizable(true)
        .inner_size(820.0, 640.0)
        .min_inner_size(620.0, 440.0)
        .skip_taskbar(false);
    if let Some(state) = store.window_state(PASSWORD_WINDOW_LABEL) {
        builder = builder
            .position(state.x, state.y)
            .inner_size(state.width, state.height)
            .maximized(state.maximized);
    } else {
        builder = builder.center();
    }

    let password_store = app.state::<crate::passwords::PasswordStore>();
    password_store.activate();
    let window = match builder.build() {
        Ok(window) => window,
        Err(error) => {
            let closing_epoch = password_store.deactivate();
            password_store.clear_deactivated_state(closing_epoch);
            return Err(AppError::new(
                "window_error",
                format!("创建密码管理器失败: {error}"),
            ));
        }
    };
    let _ = protect_sensitive_window(&window);
    let _ = window.set_focus();
    Ok(PASSWORD_WINDOW_LABEL.to_string())
}

fn ensure_sensitive_tool_local(app: &AppHandle, label: &str) -> AppResult<()> {
    let access = sensitive_tool_access(is_remote_desktop_session());
    if access.is_err() {
        close_sensitive_window_for_remote_session(app, label);
    }
    access
}

fn close_sensitive_window_for_remote_session(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
        let _ = window.destroy();
    }

    match label {
        MFA_WINDOW_LABEL => {
            app.state::<crate::mfa::MfaStore>().lock();
        }
        PASSWORD_WINDOW_LABEL => {
            app.state::<crate::passwords::PasswordStore>().lock();
            let cleanup_app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                cleanup_app
                    .state::<crate::password_browser::PasswordBrowserService>()
                    .suspend_capture();
            });
        }
        _ => {}
    }
}

fn remote_session_signals_indicate_remote(
    remote_session_metric: bool,
    remote_control_metric: bool,
    wts_protocol_type: Option<u16>,
) -> bool {
    remote_session_metric || remote_control_metric || wts_protocol_type == Some(2)
}

fn sensitive_tool_access(remote_session: bool) -> AppResult<()> {
    if remote_session {
        Err(AppError::new(
            SENSITIVE_TOOL_REMOTE_SESSION_CODE,
            SENSITIVE_TOOL_REMOTE_SESSION_MESSAGE,
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_remote_desktop_session() -> bool {
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSClientProtocolType, WTSFreeMemory, WTSQuerySessionInformationW,
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_REMOTECONTROL, SM_REMOTESESSION,
    };

    let remote_session_metric = unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 };
    let remote_control_metric = unsafe { GetSystemMetrics(SM_REMOTECONTROL) != 0 };
    let mut buffer = null_mut();
    let mut bytes_returned = 0u32;
    let query_succeeded = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSClientProtocolType,
            &mut buffer,
            &mut bytes_returned,
        ) != 0
    };
    let wts_protocol_type =
        if query_succeeded && !buffer.is_null() && bytes_returned >= size_of::<u16>() as u32 {
            Some(unsafe { *(buffer as *const u16) })
        } else {
            None
        };
    if !buffer.is_null() {
        unsafe { WTSFreeMemory(buffer as *mut core::ffi::c_void) };
    }

    remote_session_signals_indicate_remote(
        remote_session_metric,
        remote_control_metric,
        wts_protocol_type,
    )
}

#[cfg(not(windows))]
fn is_remote_desktop_session() -> bool {
    false
}

#[cfg(test)]
mod remote_session_tests {
    use super::{
        remote_session_signals_indicate_remote, sensitive_tool_access,
        SENSITIVE_TOOL_REMOTE_SESSION_CODE, SENSITIVE_TOOL_REMOTE_SESSION_MESSAGE,
    };

    #[test]
    fn either_windows_remote_metric_blocks_sensitive_tools() {
        assert!(remote_session_signals_indicate_remote(true, false, Some(0)));
        assert!(remote_session_signals_indicate_remote(false, true, Some(0)));
    }

    #[test]
    fn rdp_wts_protocol_blocks_when_metrics_are_unavailable() {
        assert!(remote_session_signals_indicate_remote(
            false,
            false,
            Some(2)
        ));
    }

    #[test]
    fn console_and_unknown_sessions_are_allowed() {
        assert!(!remote_session_signals_indicate_remote(
            false,
            false,
            Some(0)
        ));
        assert!(!remote_session_signals_indicate_remote(false, false, None));
    }

    #[test]
    fn sensitive_tool_policy_returns_the_user_facing_remote_error() {
        let error = sensitive_tool_access(true).unwrap_err();
        assert_eq!(error.code, SENSITIVE_TOOL_REMOTE_SESSION_CODE);
        assert_eq!(error.message, SENSITIVE_TOOL_REMOTE_SESSION_MESSAGE);
        sensitive_tool_access(false).unwrap();
    }
}

#[cfg(windows)]
fn protect_sensitive_window(window: &tauri::WebviewWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity;

    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;
    const WDA_MONITOR: u32 = 0x0000_0001;
    let Ok(handle) = window.hwnd() else {
        return false;
    };
    unsafe {
        SetWindowDisplayAffinity(handle.0, WDA_EXCLUDEFROMCAPTURE) != 0
            || SetWindowDisplayAffinity(handle.0, WDA_MONITOR) != 0
    }
}

#[cfg(not(windows))]
fn protect_sensitive_window(_window: &tauri::WebviewWindow) -> bool {
    false
}

#[cfg(windows)]
fn protect_mfa_window(window: &tauri::WebviewWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity;

    // WDA_EXCLUDEFROMCAPTURE is available on current Windows 10/11. Older
    // builds may reject it, in which case WDA_MONITOR still hides the content
    // from the common capture path. Protection is best effort so an OS policy
    // limitation never prevents the user from opening their vault.
    const WDA_MONITOR: u32 = 0x0000_0001;
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            if SetWindowDisplayAffinity(hwnd.0, WDA_EXCLUDEFROMCAPTURE) != 0 {
                return true;
            }
            return SetWindowDisplayAffinity(hwnd.0, WDA_MONITOR) != 0;
        }
    }
    false
}

#[cfg(not(windows))]
fn protect_mfa_window(_window: &tauri::WebviewWindow) -> bool {
    false
}

#[tauri::command]
pub async fn close_note_window(app: AppHandle, note_id: String) -> AppResult<()> {
    run_background("关闭便签窗口", move || {
        app.state::<WorkspaceStore>()
            .set_note_window_open(&note_id, false)?;
        crate::refresh_tray_menu(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn save_window_state(app: AppHandle, label: String, state: WindowState) -> AppResult<()> {
    run_background("保存窗口状态", move || {
        app.state::<WorkspaceStore>()
            .save_window_state(&label, state)
    })
    .await
}

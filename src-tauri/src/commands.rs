use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::storage::WorkspaceStore;
use serde_json::json;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

pub(crate) const TIMER_WINDOW_LABEL: &str = "timer";
pub(crate) const REMINDER_WINDOW_LABEL: &str = "reminder";
pub(crate) const GANTT_WINDOW_LABEL: &str = "gantt";
const TIMER_DEFAULT_WIDTH: f64 = 320.0;
const TIMER_DEFAULT_HEIGHT: f64 = 140.0;
const TIMER_MIN_VISIBLE_WIDTH: f64 = 48.0;
const TIMER_MIN_VISIBLE_HEIGHT: f64 = 32.0;

// Tauri's window registry does not reserve a label atomically between
// `get_webview_window` and `WebviewWindowBuilder::build`. Serializing this
// short creation path prevents rapid tray/single-instance activations from
// constructing two native windows with the same label.
static WINDOW_CREATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub(crate) fn lock_window_creation() -> MutexGuard<'static, ()> {
    WINDOW_CREATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        colors: ALLOWED_COLORS
            .iter()
            .map(|color| (*color).to_string())
            .collect(),
        recovered_drafts: store.startup_recovery().len(),
    }
}

#[tauri::command]
pub fn set_default_editor_mode(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    default_editor_mode: String,
) -> AppResult<String> {
    let default_editor_mode = store.set_default_editor_mode(&default_editor_mode)?;
    let _ = app.emit(
        "default_editor_mode_changed",
        json!({ "mode": default_editor_mode.clone() }),
    );
    Ok(default_editor_mode)
}

#[tauri::command]
pub fn set_data_storage_path(
    store: State<'_, WorkspaceStore>,
    path: String,
) -> AppResult<DataStorageChangeResult> {
    store.set_data_storage_path(path)
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart()
}

#[tauri::command]
pub fn list_notes(store: State<'_, WorkspaceStore>) -> AppResult<Vec<NoteSummary>> {
    store.list_notes()
}

#[tauri::command]
pub fn reorder_notes(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    ordered_ids: Vec<String>,
) -> AppResult<Vec<NoteSummary>> {
    let notes = store.reorder_notes(ordered_ids)?;
    let _ = app.emit("notes_reordered", json!({}));
    crate::refresh_tray_menu(&app);
    Ok(notes)
}

#[tauri::command]
pub fn create_note(app: AppHandle, store: State<'_, WorkspaceStore>) -> AppResult<NoteSnapshot> {
    let note = store.create_note()?;
    let _ = app.emit("note_changed", json!({ "id": note.id, "kind": "created" }));
    crate::refresh_tray_menu(&app);
    Ok(note)
}

#[tauri::command]
pub fn get_note(store: State<'_, WorkspaceStore>, note_id: String) -> AppResult<NoteSnapshot> {
    store.get_note(&note_id)
}

#[tauri::command]
pub fn commit_note(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    request: CommitNoteRequest,
) -> AppResult<CommitResult> {
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
}

#[tauri::command]
pub fn delete_note(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    note_id: String,
) -> AppResult<()> {
    store.delete_note(&note_id)?;
    store.set_note_window_open(&note_id, false)?;
    if let Some(window) = app.get_webview_window(&format!("note-{note_id}")) {
        let _ = window.destroy();
    }
    let _ = app.emit("note_changed", json!({ "id": note_id, "kind": "deleted" }));
    crate::refresh_tray_menu(&app);
    Ok(())
}

#[tauri::command]
pub fn list_trash(store: State<'_, WorkspaceStore>) -> AppResult<Vec<NoteSummary>> {
    store.list_trash()
}

#[tauri::command]
pub fn restore_note(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    note_id: String,
) -> AppResult<NoteSnapshot> {
    let note = store.restore_note(&note_id)?;
    let _ = app.emit("note_changed", json!({ "id": note_id, "kind": "restored" }));
    crate::refresh_tray_menu(&app);
    Ok(note)
}

#[tauri::command]
pub fn empty_trash(store: State<'_, WorkspaceStore>) -> AppResult<()> {
    store.empty_trash()
}

#[tauri::command]
pub fn import_asset(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    note_id: String,
    file_name: String,
    bytes: Vec<u8>,
) -> AppResult<AssetResult> {
    let asset = store.import_asset(&note_id, &file_name, &bytes)?;
    let _ = app.emit(
        "asset_imported",
        json!({ "noteId": note_id, "asset": asset.clone() }),
    );
    Ok(asset)
}

#[tauri::command]
pub fn read_asset(
    store: State<'_, WorkspaceStore>,
    note_id: String,
    relative_path: String,
) -> AppResult<AssetContent> {
    store.read_asset(&note_id, &relative_path)
}

#[tauri::command]
pub fn search_notes(
    store: State<'_, WorkspaceStore>,
    query: String,
    limit: Option<u32>,
) -> AppResult<Vec<SearchResult>> {
    store.search_notes(&query, limit)
}

#[tauri::command]
pub async fn open_note_window(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    note_id: String,
) -> AppResult<String> {
    open_note_window_inner(&app, &store, &note_id)
}

#[tauri::command]
pub async fn open_tool_window(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    tool: ToolName,
) -> AppResult<String> {
    match tool {
        ToolName::Timer => open_timer_window_inner(&app, &store),
        ToolName::Reminder => open_reminder_window_inner(&app, &store),
        ToolName::Gantt => open_gantt_window_inner(&app, &store),
        ToolName::Screenshot => crate::screenshot::start_capture_inner(&app)
            .map(|_| crate::screenshot::CAPTURE_WINDOW_LABEL.to_string()),
    }
}

pub fn open_note_window_inner(
    app: &AppHandle,
    store: &WorkspaceStore,
    note_id: &str,
) -> AppResult<String> {
    crate::trace_activation(&format!("open_note:{note_id}:waiting_creation_lock"));
    let _creation_guard = lock_window_creation();
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
    let _creation_guard = lock_window_creation();
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
    let _creation_guard = lock_window_creation();
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
    let _creation_guard = lock_window_creation();
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

#[tauri::command]
pub fn close_note_window(
    app: AppHandle,
    store: State<'_, WorkspaceStore>,
    note_id: String,
) -> AppResult<()> {
    store.set_note_window_open(&note_id, false)?;
    crate::refresh_tray_menu(&app);
    Ok(())
}

#[tauri::command]
pub fn save_window_state(
    store: State<'_, WorkspaceStore>,
    label: String,
    state: WindowState,
) -> AppResult<()> {
    store.save_window_state(&label, state)
}

pub mod browser_bridge;
pub mod browser_native_host;
mod commands;
mod error;
mod gantt;
mod long_screenshot;
mod long_screenshot_input;
mod mfa;
mod models;
mod phase_match;
mod reminders;
mod screenshot;
mod storage;
mod timer;
#[cfg(windows)]
mod window_activation;

use crate::gantt::GanttStore;
use crate::long_screenshot::LongScreenshotStore;
use crate::mfa::MfaStore;
use crate::reminders::ReminderStore;
use crate::screenshot::ScreenshotStore;
use crate::storage::WorkspaceStore;
use crate::timer::TimerStore;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

const TRAY_ID: &str = "petaldesk-tray";
const OPEN_NOTE_MENU_PREFIX: &str = "open-note:";
const OPEN_TIMER_MENU_ID: &str = "open-tool:timer";
const OPEN_REMINDER_MENU_ID: &str = "open-tool:reminder";
const OPEN_GANTT_MENU_ID: &str = "open-tool:gantt";
const OPEN_MFA_MENU_ID: &str = "open-tool:mfa";
const OPEN_SCREENSHOT_MENU_ID: &str = "open-tool:screenshot";
const ABOUT_MENU_ID: &str = "about";
const MAX_TRAY_NOTE_TITLE_CHARS: usize = 80;
const ACTIVATION_FALLBACK_DELAY: Duration = Duration::from_secs(2);
static ACTIVATION_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static ACTIVATION_GENERATION: AtomicU64 = AtomicU64::new(0);
static INITIAL_ACTIVATION_SCHEDULED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct TrayRefreshState {
    running: bool,
    pending: bool,
}

static TRAY_REFRESH_STATE: LazyLock<Mutex<TrayRefreshState>> =
    LazyLock::new(|| Mutex::new(TrayRefreshState::default()));

struct ActivationInFlightGuard;

impl Drop for ActivationInFlightGuard {
    fn drop(&mut self) {
        ACTIVATION_IN_FLIGHT.store(false, Ordering::Release);
    }
}

fn next_activation_generation(generation: &AtomicU64) -> u64 {
    generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
}

fn activation_fallback_is_current(
    in_flight: &AtomicBool,
    generation: &AtomicU64,
    expected_generation: u64,
) -> bool {
    in_flight.load(Ordering::Acquire) && generation.load(Ordering::Acquire) == expected_generation
}

pub(crate) fn trace_activation(message: &str) {
    if std::env::var_os("PETALDESK_ACTIVATION_TRACE").is_none() {
        return;
    }
    let Some(data_dir) = dirs::data_local_dir() else {
        return;
    };
    let path = data_dir.join("PetalDesk").join("activation-trace.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{:?} {:?} {}",
            SystemTime::now(),
            std::thread::current().id(),
            message
        );
    }
}

fn tray_note_label(title: &str) -> String {
    let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = if normalized.is_empty() {
        models::DEFAULT_NOTE_TITLE
    } else {
        &normalized
    };
    let mut characters = normalized.chars();
    let mut label = characters
        .by_ref()
        .take(MAX_TRAY_NOTE_TITLE_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        label.pop();
        label.push('…');
    }
    #[cfg(target_os = "windows")]
    {
        label = label.replace('&', "&&");
    }
    label
}

fn note_id_from_tray_menu_id(menu_id: &str) -> Option<&str> {
    let note_id = menu_id.strip_prefix(OPEN_NOTE_MENU_PREFIX)?;
    storage::validate_note_id(note_id).ok()?;
    Some(note_id)
}

fn tool_from_tray_menu_id(menu_id: &str) -> Option<models::ToolName> {
    match menu_id {
        OPEN_TIMER_MENU_ID => Some(models::ToolName::Timer),
        OPEN_REMINDER_MENU_ID => Some(models::ToolName::Reminder),
        OPEN_GANTT_MENU_ID => Some(models::ToolName::Gantt),
        OPEN_MFA_MENU_ID => Some(models::ToolName::Mfa),
        OPEN_SCREENSHOT_MENU_ID => Some(models::ToolName::Screenshot),
        _ => None,
    }
}

fn about_message(last_updated: Option<SystemTime>) -> String {
    let last_updated = last_updated
        .map(chrono::DateTime::<chrono::Local>::from)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "未知".to_string());
    format!(
        "名称：飞花 - PetalDesk\r\n版本：{}\r\n最后更新时间：{last_updated}",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(target_os = "windows")]
fn show_about_dialog() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
    };

    let last_updated = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok());
    let message = about_message(last_updated);
    let message = message
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let title = "关于飞花 - PetalDesk"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_about_dialog() {}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn show_note_and_hide_main(app: &tauri::AppHandle, note_id: &str) -> bool {
    if commands::open_note_window_inner(app, &app.state(), note_id).is_err() {
        return false;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    true
}

fn show_last_note_or_main(app: &tauri::AppHandle) {
    trace_activation("show_last:start");
    let store = app.state::<WorkspaceStore>();
    if let Ok(Some(note_id)) = store.last_or_recent_note_id() {
        trace_activation(&format!("show_last:note:{note_id}"));
        if show_note_and_hide_main(app, &note_id) {
            trace_activation("show_last:note_shown");
            return;
        }
    }
    trace_activation("show_last:fallback_main");
    show_main_window(app);
    trace_activation("show_last:end");
}

fn show_first_note_or_main(app: &tauri::AppHandle) {
    trace_activation("show_first:start");
    let store = app.state::<WorkspaceStore>();
    if let Ok(Some(note_id)) = store.first_note_id() {
        trace_activation(&format!("show_first:note:{note_id}"));
        if show_note_and_hide_main(app, &note_id) {
            trace_activation("show_first:note_shown");
            return;
        }
    }
    trace_activation("show_first:fallback_main");
    show_main_window(app);
    trace_activation("show_first:end");
}

// Tray and single-instance callbacks run on Tauri's event-loop thread. A new
// WebView must be built on a worker thread or its synchronous channel deadlocks.
fn spawn_show_last_note_or_main(app: &tauri::AppHandle) {
    trace_activation("activation:request");
    let generation = next_activation_generation(&ACTIVATION_GENERATION);
    if ACTIVATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        trace_activation("activation:coalesced");
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || show_main_window(&app));
        return;
    }
    let app = app.clone();
    let fallback_app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(ACTIVATION_FALLBACK_DELAY);
        if activation_fallback_is_current(&ACTIVATION_IN_FLIGHT, &ACTIVATION_GENERATION, generation)
        {
            trace_activation("activation:fallback_main");
            show_main_window(&fallback_app);
        }
    });
    tauri::async_runtime::spawn_blocking(move || {
        let _in_flight = ActivationInFlightGuard;
        trace_activation("activation:worker_start");
        show_last_note_or_main(&app);
        trace_activation("activation:worker_end");
    });
}

fn spawn_show_first_note_or_main(app: &tauri::AppHandle) {
    trace_activation("activation_first:request");
    let generation = next_activation_generation(&ACTIVATION_GENERATION);
    if ACTIVATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        trace_activation("activation_first:coalesced");
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || show_main_window(&app));
        return;
    }
    let app = app.clone();
    let fallback_app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(ACTIVATION_FALLBACK_DELAY);
        if activation_fallback_is_current(&ACTIVATION_IN_FLIGHT, &ACTIVATION_GENERATION, generation)
        {
            trace_activation("activation_first:fallback_main");
            show_main_window(&fallback_app);
        }
    });
    tauri::async_runtime::spawn_blocking(move || {
        let _in_flight = ActivationInFlightGuard;
        trace_activation("activation_first:worker_start");
        show_first_note_or_main(&app);
        trace_activation("activation_first:worker_end");
    });
}

fn spawn_show_note(app: &tauri::AppHandle, note_id: String) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !show_note_and_hide_main(&app, &note_id) {
            refresh_tray_menu(&app);
        }
    });
}

fn spawn_open_tool(app: &tauri::AppHandle, tool: models::ToolName) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || match tool {
        models::ToolName::Timer => {
            let _ = commands::open_timer_window_inner(&app, &app.state());
        }
        models::ToolName::Reminder => {
            let _ = commands::open_reminder_window_inner(&app, &app.state());
        }
        models::ToolName::Gantt => {
            let _ = commands::open_gantt_window_inner(&app, &app.state());
        }
        models::ToolName::Mfa => {
            let _ = commands::open_mfa_window_inner(&app, &app.state());
        }
        models::ToolName::Screenshot => {
            let _ = screenshot::start_capture_inner(&app);
        }
    });
}

fn spawn_create_note(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Ok(note) = app.state::<WorkspaceStore>().create_note() else {
            return;
        };
        let note_id = note.id.clone();
        let _ = commands::open_note_window_inner(&app, &app.state(), &note_id);
        let _ = app.emit(
            "note_changed",
            serde_json::json!({ "id": note.id, "kind": "created" }),
        );
        refresh_tray_menu(&app);
    });
}

fn build_tray_menu(app: &tauri::AppHandle) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "显示飞花 - PetalDesk", true, None::<&str>)?;
    let note_list = Submenu::with_id(app, "note-list", "便签列表", true)?;
    let notes = app.state::<WorkspaceStore>().list_notes()?;
    if notes.is_empty() {
        let empty = MenuItem::with_id(app, "no-notes", "暂无便签", false, None::<&str>)?;
        note_list.append(&empty)?;
    } else {
        for note in notes {
            let title = tray_note_label(&note.title);
            let item = MenuItem::with_id(
                app,
                format!("{OPEN_NOTE_MENU_PREFIX}{}", note.id),
                &title,
                true,
                None::<&str>,
            )?;
            note_list.append(&item)?;
        }
    }
    let tools = Submenu::with_id(app, "tools", "小工具", true)?;
    let timer = MenuItem::with_id(app, OPEN_TIMER_MENU_ID, "计时器", true, None::<&str>)?;
    let reminder = MenuItem::with_id(app, OPEN_REMINDER_MENU_ID, "提醒", true, None::<&str>)?;
    let gantt = MenuItem::with_id(app, OPEN_GANTT_MENU_ID, "任务甘特图", true, None::<&str>)?;
    let mfa = MenuItem::with_id(app, OPEN_MFA_MENU_ID, "MFA 验证器", true, None::<&str>)?;
    let screenshot_label = format!(
        "截图({})",
        app.state::<ScreenshotStore>().settings().shortcut
    );
    let screenshot = MenuItem::with_id(
        app,
        OPEN_SCREENSHOT_MENU_ID,
        &screenshot_label,
        true,
        None::<&str>,
    )?;
    tools.append(&timer)?;
    tools.append(&reminder)?;
    tools.append(&gantt)?;
    tools.append(&mfa)?;
    tools.append(&screenshot)?;
    let new_note = MenuItem::with_id(app, "new-note", "新建便签", true, None::<&str>)?;
    let about = MenuItem::with_id(app, ABOUT_MENU_ID, "关于", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    Ok(Menu::with_items(
        app,
        &[&show, &note_list, &tools, &new_note, &about, &quit],
    )?)
}

pub(crate) fn refresh_tray_menu(app: &tauri::AppHandle) {
    // Menu construction reads the workspace and synchronously hops to the UI
    // thread for each native item, so never run it inside an event callback.
    // Coalesce bursts (for example, several autosaves) into one refresh and a
    // final follow-up pass instead of queuing unbounded UI work.
    let should_spawn = {
        let mut state = TRAY_REFRESH_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending = true;
        if state.running {
            false
        } else {
            state.running = true;
            true
        }
    };
    if !should_spawn {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || loop {
        {
            let mut state = TRAY_REFRESH_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending = false;
        }
        refresh_tray_menu_now(&app);
        let continue_refresh = {
            let mut state = TRAY_REFRESH_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.pending {
                true
            } else {
                state.running = false;
                false
            }
        };
        if !continue_refresh {
            break;
        }
    });
}

fn refresh_tray_menu_now(app: &tauri::AppHandle) {
    let Ok(menu) = build_tray_menu(app) else {
        return;
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    screenshot::setup(app)?;
    let menu = build_tray_menu(app.handle())?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("飞花 - PetalDesk")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "new-note" => spawn_create_note(app),
            ABOUT_MENU_ID => show_about_dialog(),
            "quit" => {
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    app.state::<MfaStore>().lock();
                    long_screenshot::shutdown(&app);
                    app.exit(0);
                });
            }
            id => {
                if let Some(tool) = tool_from_tray_menu_id(id) {
                    spawn_open_tool(app, tool);
                    return;
                }
                if let Some(note_id) = note_id_from_tray_menu_id(id) {
                    spawn_show_note(app, note_id.to_string());
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                spawn_show_first_note_or_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    if let Some(state) = app.state::<WorkspaceStore>().window_state("main") {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_position(tauri::LogicalPosition::new(state.x, state.y));
            let _ = window.set_size(tauri::LogicalSize::new(state.width, state.height));
            if state.maximized {
                let _ = window.maximize();
            }
        }
    }
    for draft in app.state::<WorkspaceStore>().startup_recovery() {
        let _ = app.emit("recovered_draft", draft);
    }

    let app_handle = app.handle().clone();
    std::thread::spawn(move || loop {
        // External edits arrive from other editors, so a few seconds of latency
        // is fine. The scan itself is mtime-gated, so this loop is nearly free
        // when nothing changed.
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Ok(notes) = app_handle
            .state::<WorkspaceStore>()
            .detect_external_changes()
        {
            let tray_menu_changed = !notes.is_empty();
            for note in notes {
                let _ = app_handle.emit(
                    "note_changed",
                    serde_json::json!({
                        "id": note.id,
                        "kind": "external",
                        "revision": note.revision,
                    }),
                );
            }
            if tray_menu_changed {
                refresh_tray_menu(&app_handle);
            }
        }
    });
    reminders::start_scheduler(app.handle().clone());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = WorkspaceStore::load().expect("无法初始化飞花 - PetalDesk 数据存储");
    let data_storage_path = store.data_storage_path();
    let reminder_store =
        ReminderStore::load(&data_storage_path).expect("无法初始化飞花 - PetalDesk 提醒");
    let gantt_store =
        GanttStore::load(&data_storage_path).expect("无法初始化飞花 - PetalDesk 任务甘特图");
    let timer_store =
        TimerStore::load(&data_storage_path).expect("无法初始化飞花 - PetalDesk 计时器");
    let screenshot_store =
        ScreenshotStore::load(&data_storage_path).expect("无法初始化飞花 - PetalDesk 截图工具");
    let long_screenshot_store = LongScreenshotStore::load(&data_storage_path)
        .expect("无法初始化飞花 - PetalDesk 长截图工具");
    let mfa_store =
        MfaStore::load(&data_storage_path).expect("无法初始化飞花 - PetalDesk MFA 验证器");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                trace_activation("single_instance:callback_start");
                spawn_show_last_note_or_main(app);
                trace_activation("single_instance:callback_end");
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(store)
        .manage(reminder_store)
        .manage(gantt_store)
        .manage(timer_store)
        .manage(screenshot_store)
        .manage(long_screenshot_store)
        .manage(mfa_store)
        .on_page_load(|webview, payload| {
            if webview.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
                && INITIAL_ACTIVATION_SCHEDULED
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                // `RunEvent::Ready` can arrive before WebView2 has finished
                // initializing its first controller. Waiting for the hidden
                // main page to finish loading avoids losing the first note
                // window on fast, installed launches.
                trace_activation("main_page:finished");
                spawn_show_last_note_or_main(webview.app_handle());
            }
        })
        .setup(setup_tray)
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle().clone();
                let label = window.label().to_string();
                #[cfg(windows)]
                let window_instance = window.hwnd().ok().map(|handle| handle.0 as isize);
                #[cfg(not(windows))]
                let window_instance = None;
                if long_screenshot::handle_control_window_close_requested(
                    &app,
                    &label,
                    window_instance,
                ) {
                    api.prevent_close();
                    return;
                }
                if screenshot::handle_window_close_requested(&app, &label, window_instance) {
                    api.prevent_close();
                    return;
                }
                trace_activation(&format!("window_close:{label}:requested"));
                if let (Ok(position), Ok(size), Ok(scale), Ok(maximized)) = (
                    window.outer_position(),
                    window.inner_size(),
                    window.scale_factor(),
                    window.is_maximized(),
                ) {
                    let position = position.to_logical::<f64>(scale);
                    let size = size.to_logical::<f64>(scale);
                    let app = app.clone();
                    let label = label.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let _ = app.state::<WorkspaceStore>().save_window_state(
                            &label,
                            models::WindowState {
                                x: position.x,
                                y: position.y,
                                width: size.width,
                                height: size.height,
                                maximized,
                            },
                        );
                    });
                }
                if label == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                } else if let Some(note_id) = label.strip_prefix("note-") {
                    let app = app.clone();
                    let note_id = note_id.to_string();
                    tauri::async_runtime::spawn_blocking(move || {
                        let _ = app
                            .state::<WorkspaceStore>()
                            .set_note_window_open(&note_id, false);
                        refresh_tray_menu(&app);
                    });
                }
            } else if let WindowEvent::Destroyed = event {
                let app = window.app_handle().clone();
                let label = window.label().to_string();
                #[cfg(windows)]
                let window_instance = window.hwnd().ok().map(|handle| handle.0 as isize);
                #[cfg(not(windows))]
                let window_instance = None;
                long_screenshot::handle_control_window_destroyed(&app, &label, window_instance);
                screenshot::handle_window_destroyed(&app, &label, window_instance);
                if label == commands::MFA_WINDOW_LABEL {
                    // Invalidate queued reveal/copy/file workers synchronously;
                    // the blocking zeroize/clipboard cleanup follows off the
                    // event loop and is epoch-guarded against a quick reopen.
                    let closing_epoch = app.state::<MfaStore>().deactivate();
                    let mfa_app = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        mfa_app
                            .state::<MfaStore>()
                            .clear_deactivated_state(closing_epoch);
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::set_default_editor_mode,
            commands::set_data_storage_path,
            commands::restart_app,
            commands::list_notes,
            commands::reorder_notes,
            commands::create_note,
            commands::get_note,
            commands::commit_note,
            commands::delete_note,
            commands::list_trash,
            commands::restore_note,
            commands::empty_trash,
            commands::import_asset,
            commands::read_asset,
            commands::search_notes,
            commands::open_note_window,
            commands::open_tool_window,
            commands::close_note_window,
            commands::save_window_state,
            reminders::list_reminders,
            reminders::upsert_reminder,
            reminders::delete_reminder,
            reminders::set_reminder_enabled,
            gantt::list_gantt_tasks,
            gantt::upsert_gantt_task,
            gantt::delete_gantt_task,
            gantt::reorder_gantt_tasks,
            timer::get_timer_data,
            timer::save_timer_data,
            mfa::get_mfa_status,
            mfa::list_mfa_entries,
            mfa::configure_mfa_recovery_password,
            mfa::unlock_mfa_with_recovery_password,
            mfa::scan_mfa_screen_qr,
            mfa::preview_mfa_qr_image,
            mfa::preview_mfa_uri,
            mfa::preview_mfa_manual,
            mfa::commit_mfa_import,
            mfa::cancel_mfa_import,
            mfa::update_mfa_entry,
            mfa::delete_mfa_entry,
            mfa::reveal_mfa_code,
            mfa::copy_mfa_code,
            mfa::lock_mfa_vault,
            screenshot::get_screenshot_settings,
            screenshot::set_screenshot_shortcut,
            screenshot::update_screenshot_settings,
            screenshot::start_screenshot_capture,
            screenshot::get_screenshot_session,
            screenshot::get_screenshot_frame,
            screenshot::present_screenshot_capture,
            screenshot::cancel_screenshot_capture,
            screenshot::prepare_screenshot_export,
            screenshot::commit_screenshot_export,
            screenshot::get_pinned_screenshot,
            screenshot::copy_pinned_screenshot,
            screenshot::save_pinned_screenshot,
            screenshot::close_pinned_screenshot,
            long_screenshot::get_long_capture_capability,
            long_screenshot::start_long_capture,
            long_screenshot::pause_long_capture,
            long_screenshot::resume_long_capture,
            long_screenshot::retry_long_capture_segment,
            long_screenshot::undo_long_capture_segment,
            long_screenshot::finish_long_capture,
            long_screenshot::cancel_long_capture,
            long_screenshot::cancel_long_capture_session,
            long_screenshot::get_long_capture_status,
            long_screenshot::get_long_capture_tile,
            long_screenshot::export_long_capture,
            long_screenshot::prepare_long_capture_annotation_export,
            long_screenshot::upload_long_capture_annotation_strip,
            long_screenshot::finish_long_capture_annotation_export,
            long_screenshot::cancel_long_capture_annotation_export,
        ])
        .build(tauri::generate_context!())
        .expect("无法启动飞花 - PetalDesk 应用");

    app.run(|app, event| match event {
        RunEvent::Ready => {
            trace_activation("run_event:ready");
        }
        RunEvent::ExitRequested { api, code, .. } => {
            if code.is_none() {
                api.prevent_exit();
            } else {
                app.state::<MfaStore>().lock();
            }
            // Explicit quit and restart paths finish long-capture cleanup on a
            // worker thread before requesting process exit. Repeating that
            // blocking cleanup here can stall Tauri's event loop while Windows
            // is already tearing down the process.
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_fallback_only_runs_for_the_current_in_flight_request() {
        let in_flight = AtomicBool::new(true);
        let generation = AtomicU64::new(0);
        let first = next_activation_generation(&generation);
        assert!(activation_fallback_is_current(
            &in_flight,
            &generation,
            first
        ));

        let second = next_activation_generation(&generation);
        assert!(!activation_fallback_is_current(
            &in_flight,
            &generation,
            first
        ));
        assert!(activation_fallback_is_current(
            &in_flight,
            &generation,
            second
        ));

        in_flight.store(false, Ordering::Release);
        assert!(!activation_fallback_is_current(
            &in_flight,
            &generation,
            second
        ));
    }

    #[test]
    fn tray_note_labels_are_single_line_and_bounded() {
        assert_eq!(
            tray_note_label("  第一行\n第二行\t第三行  "),
            "第一行 第二行 第三行"
        );
        assert_eq!(tray_note_label(" \n\t "), models::DEFAULT_NOTE_TITLE);

        let label = tray_note_label(&"便".repeat(MAX_TRAY_NOTE_TITLE_CHARS + 20));
        assert_eq!(label.chars().count(), MAX_TRAY_NOTE_TITLE_CHARS);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn tray_note_menu_ids_only_accept_canonical_uuids() {
        let note_id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            note_id_from_tray_menu_id(&format!("{OPEN_NOTE_MENU_PREFIX}{note_id}")),
            Some(note_id)
        );
        assert_eq!(note_id_from_tray_menu_id("open-note:../notes"), None);
        assert_eq!(note_id_from_tray_menu_id("show"), None);
    }

    #[test]
    fn tray_tool_menu_ids_only_accept_known_tools() {
        assert_eq!(
            tool_from_tray_menu_id(OPEN_TIMER_MENU_ID),
            Some(models::ToolName::Timer)
        );
        assert_eq!(
            tool_from_tray_menu_id(OPEN_REMINDER_MENU_ID),
            Some(models::ToolName::Reminder)
        );
        assert_eq!(
            tool_from_tray_menu_id(OPEN_GANTT_MENU_ID),
            Some(models::ToolName::Gantt)
        );
        assert_eq!(
            tool_from_tray_menu_id(OPEN_MFA_MENU_ID),
            Some(models::ToolName::Mfa)
        );
        assert_eq!(
            tool_from_tray_menu_id(OPEN_SCREENSHOT_MENU_ID),
            Some(models::ToolName::Screenshot)
        );
        assert_eq!(tool_from_tray_menu_id("open-tool:../timer"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:../reminder"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:../gantt"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:../mfa"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:Timer"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:Gantt"), None);
        assert_eq!(tool_from_tray_menu_id("open-tool:Mfa"), None);
        assert_eq!(tool_from_tray_menu_id("timer"), None);
        assert_eq!(tool_from_tray_menu_id("gantt"), None);
        assert_eq!(tool_from_tray_menu_id("mfa"), None);
    }

    #[test]
    fn about_message_contains_product_version_and_time_fallback() {
        let message = about_message(None);
        assert!(message.contains("名称：飞花 - PetalDesk"));
        assert!(message.contains(&format!("版本：{}", env!("CARGO_PKG_VERSION"))));
        assert!(message.contains("最后更新时间：未知"));
    }
}

pub mod browser_bridge;
pub mod browser_native_host;
mod browser_secret_bridge;
mod commands;
mod diagnostics;
mod error;
mod gantt;
mod launch_at_login;
mod long_screenshot;
mod long_screenshot_input;
mod mfa;
mod models;
mod network;
mod password_browser;
mod passwords;
mod phase_match;
mod recovery;
mod reminders;
mod screenshot;
mod storage;
mod timer;
mod updater;
#[cfg(windows)]
mod window_activation;

use crate::gantt::GanttStore;
use crate::long_screenshot::LongScreenshotStore;
use crate::mfa::MfaStore;
use crate::password_browser::PasswordBrowserService;
use crate::passwords::PasswordStore;
use crate::reminders::ReminderStore;
use crate::screenshot::ScreenshotStore;
use crate::storage::WorkspaceStore;
use crate::timer::TimerStore;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

const TRAY_ID: &str = "petaldesk-tray";
const OPEN_NOTE_MENU_PREFIX: &str = "open-note:";
const OPEN_TIMER_MENU_ID: &str = "open-tool:timer";
const OPEN_REMINDER_MENU_ID: &str = "open-tool:reminder";
const OPEN_GANTT_MENU_ID: &str = "open-tool:gantt";
const OPEN_MFA_MENU_ID: &str = "open-tool:mfa";
const OPEN_PASSWORD_MENU_ID: &str = "open-tool:passwords";
const OPEN_SCREENSHOT_MENU_ID: &str = "open-tool:screenshot";
const OPEN_NETWORK_MENU_ID: &str = "open-tool:network";
const ABOUT_MENU_ID: &str = "about";
const MAX_TRAY_NOTE_TITLE_CHARS: usize = 80;
const ACTIVATION_FALLBACK_DELAY: Duration = Duration::from_secs(2);
/// Minimum spacing between two native tray menu rebuilds. Rebuilding hops to
/// the UI thread once per menu item, so bursts of autosaves must not translate
/// into bursts of synchronous main-thread round trips.
const TRAY_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(2);
static ACTIVATION_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static ACTIVATION_GENERATION: AtomicU64 = AtomicU64::new(0);
static INITIAL_ACTIVATION_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Signature of everything the tray menu actually displays. Rebuilds are
/// skipped when it is unchanged, so body-only autosaves cost nothing.
static TRAY_MENU_SIGNATURE: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Tray refreshes run on one dedicated long-lived thread instead of Tauri's
/// shared `spawn_blocking` pool: a refresh can block for a while on the UI
/// thread, and starving that pool would also stall every note save.
static TRAY_REFRESH_SENDER: OnceLock<SyncSender<()>> = OnceLock::new();

/// Clears `ACTIVATION_IN_FLIGHT` on drop.
///
/// Construct this on the caller's side of `spawn_blocking` and move it into the
/// closure. Built inside the closure instead, a task dropped before its body
/// ever runs (a saturated blocking pool, or shutdown) would leak the flag as
/// `true` and silently swallow every later activation.
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
        OPEN_PASSWORD_MENU_ID => Some(models::ToolName::Passwords),
        OPEN_SCREENSHOT_MENU_ID => Some(models::ToolName::Screenshot),
        OPEN_NETWORK_MENU_ID => Some(models::ToolName::Network),
        _ => None,
    }
}

fn tray_action_for_modifiers(
    settings: models::TrayShortcutSettings,
    alt_pressed: bool,
    ctrl_pressed: bool,
    shift_pressed: bool,
) -> Option<models::TrayShortcutAction> {
    match (alt_pressed, ctrl_pressed, shift_pressed) {
        (false, false, false) => Some(settings.double_click),
        (true, false, false) => Some(settings.alt_double_click),
        (false, true, false) => Some(settings.ctrl_double_click),
        (false, false, true) => Some(settings.shift_double_click),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn current_tray_double_click_action(
    settings: models::TrayShortcutSettings,
) -> Option<models::TrayShortcutAction> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };

    let pressed = |key| unsafe { GetAsyncKeyState(i32::from(key)) < 0 };
    tray_action_for_modifiers(
        settings,
        pressed(VK_MENU),
        pressed(VK_CONTROL),
        pressed(VK_SHIFT),
    )
}

#[cfg(not(target_os = "windows"))]
fn current_tray_double_click_action(
    settings: models::TrayShortcutSettings,
) -> Option<models::TrayShortcutAction> {
    tray_action_for_modifiers(settings, false, false, false)
}

pub(crate) fn show_main_window(app: &tauri::AppHandle) {
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
    let store = app.state::<WorkspaceStore>();
    if let Ok(Some(note_id)) = store.last_or_recent_note_id() {
        if show_note_and_hide_main(app, &note_id) {
            return;
        }
    }
    show_main_window(app);
}

fn show_first_note_or_main(app: &tauri::AppHandle) {
    let store = app.state::<WorkspaceStore>();
    if let Ok(Some(note_id)) = store.first_note_id() {
        if show_note_and_hide_main(app, &note_id) {
            return;
        }
    }
    show_main_window(app);
}

// Tray and single-instance callbacks run on Tauri's event-loop thread. A new
// WebView must be built on a worker thread or its synchronous channel deadlocks.
fn spawn_show_last_note_or_main(app: &tauri::AppHandle) {
    diagnostics::record(diagnostics::DiagnosticEvent::ActivationRequested);
    let generation = next_activation_generation(&ACTIVATION_GENERATION);
    if ACTIVATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationCoalesced);
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
            diagnostics::record(diagnostics::DiagnosticEvent::ActivationFallback);
            show_main_window(&fallback_app);
        }
    });
    let in_flight = ActivationInFlightGuard;
    tauri::async_runtime::spawn_blocking(move || {
        let _in_flight = in_flight;
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerStarted);
        show_last_note_or_main(&app);
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerFinished);
    });
}

fn spawn_show_first_note_or_main(app: &tauri::AppHandle) {
    diagnostics::record(diagnostics::DiagnosticEvent::ActivationRequested);
    let generation = next_activation_generation(&ACTIVATION_GENERATION);
    if ACTIVATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationCoalesced);
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
            diagnostics::record(diagnostics::DiagnosticEvent::ActivationFallback);
            show_main_window(&fallback_app);
        }
    });
    let in_flight = ActivationInFlightGuard;
    tauri::async_runtime::spawn_blocking(move || {
        let _in_flight = in_flight;
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerStarted);
        show_first_note_or_main(&app);
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerFinished);
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
    tauri::async_runtime::spawn_blocking(move || {
        let result = match tool {
            models::ToolName::Timer => {
                commands::open_timer_window_inner(&app, &app.state()).map(|_| ())
            }
            models::ToolName::Reminder => {
                commands::open_reminder_window_inner(&app, &app.state()).map(|_| ())
            }
            models::ToolName::Gantt => {
                commands::open_gantt_window_inner(&app, &app.state()).map(|_| ())
            }
            models::ToolName::Mfa => {
                commands::open_mfa_window_inner(&app, &app.state()).map(|_| ())
            }
            models::ToolName::Passwords => commands::open_password_window(&app),
            models::ToolName::Screenshot => screenshot::start_capture_inner(&app).map(|_| ()),
            models::ToolName::Network => {
                commands::open_network_window_inner(&app, &app.state()).map(|_| ())
            }
        };
        if let Err(error) = result {
            if error.code == commands::SENSITIVE_TOOL_REMOTE_SESSION_CODE {
                app.dialog()
                    .message(error.message)
                    .title("飞花安全提示")
                    .kind(MessageDialogKind::Warning)
                    .blocking_show();
            } else {
                eprintln!("托盘打开小工具失败: {error}");
            }
        }
    });
}

fn spawn_tray_action(app: &tauri::AppHandle, action: models::TrayShortcutAction) {
    match action {
        models::TrayShortcutAction::FirstNote => spawn_show_first_note_or_main(app),
        models::TrayShortcutAction::RecentNote => spawn_show_last_note_or_main(app),
        models::TrayShortcutAction::MainWindow => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || show_main_window(&app));
        }
        models::TrayShortcutAction::Timer => spawn_open_tool(app, models::ToolName::Timer),
        models::TrayShortcutAction::Reminder => spawn_open_tool(app, models::ToolName::Reminder),
        models::TrayShortcutAction::Gantt => spawn_open_tool(app, models::ToolName::Gantt),
        models::TrayShortcutAction::Mfa => spawn_open_tool(app, models::ToolName::Mfa),
        models::TrayShortcutAction::Passwords => spawn_open_tool(app, models::ToolName::Passwords),
        models::TrayShortcutAction::Screenshot => {
            spawn_open_tool(app, models::ToolName::Screenshot)
        }
    }
}

/// Reads the configured action on a worker, then performs it.
///
/// This must never run on the event-loop thread. The Windows single-instance
/// handshake delivers `WM_COPYDATA` synchronously on the primary's message
/// pump, and the secondary's `SendMessageW` has no timeout: it only reaches
/// `exit(0)` after the primary's callback returns. Blocking here — even on an
/// uncontended-looking `RwLock` read, which parks behind any queued writer —
/// stalls the pump, so relaunches pile up as live processes that never got as
/// far as creating a tray icon, while every queued click goes undispatched.
fn spawn_primary_activation_action(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerStarted);
        let action = app
            .state::<WorkspaceStore>()
            .tray_shortcut_settings()
            .double_click;
        spawn_tray_action(&app, action);
        diagnostics::record(diagnostics::DiagnosticEvent::ActivationWorkerFinished);
    });
}

pub(crate) fn spawn_create_note(app: &tauri::AppHandle) {
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

fn build_tray_menu(
    app: &tauri::AppHandle,
    notes: Vec<models::NoteSummary>,
    screenshot_shortcut: &str,
) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "打开主界面", true, None::<&str>)?;
    let notes_menu = Submenu::with_id(app, "notes", "便签", true)?;
    let new_note = MenuItem::with_id(app, "new-note", "新建便签", true, None::<&str>)?;
    let note_separator = PredefinedMenuItem::separator(app)?;
    notes_menu.append(&new_note)?;
    notes_menu.append(&note_separator)?;
    if notes.is_empty() {
        let empty = MenuItem::with_id(app, "no-notes", "暂无便签", false, None::<&str>)?;
        notes_menu.append(&empty)?;
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
            notes_menu.append(&item)?;
        }
    }
    let tools = Submenu::with_id(app, "tools", "小工具", true)?;
    let timer = MenuItem::with_id(app, OPEN_TIMER_MENU_ID, "计时器", true, None::<&str>)?;
    let reminder = MenuItem::with_id(app, OPEN_REMINDER_MENU_ID, "提醒", true, None::<&str>)?;
    let gantt = MenuItem::with_id(app, OPEN_GANTT_MENU_ID, "任务甘特图", true, None::<&str>)?;
    let mfa = MenuItem::with_id(app, OPEN_MFA_MENU_ID, "MFA 验证器", true, None::<&str>)?;
    let passwords =
        MenuItem::with_id(app, OPEN_PASSWORD_MENU_ID, "密码管理器", true, None::<&str>)?;
    let screenshot_label = format!("截图({screenshot_shortcut})");
    let screenshot = MenuItem::with_id(
        app,
        OPEN_SCREENSHOT_MENU_ID,
        &screenshot_label,
        true,
        None::<&str>,
    )?;
    let network = MenuItem::with_id(app, OPEN_NETWORK_MENU_ID, "网络体检", true, None::<&str>)?;
    tools.append(&timer)?;
    tools.append(&reminder)?;
    tools.append(&gantt)?;
    tools.append(&mfa)?;
    tools.append(&passwords)?;
    tools.append(&screenshot)?;
    tools.append(&network)?;
    let about = MenuItem::with_id(app, ABOUT_MENU_ID, "关于", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    Ok(Menu::with_items(
        app,
        &[&show, &notes_menu, &tools, &about, &quit],
    )?)
}

/// Requests a tray menu rebuild. Cheap and safe to call from event callbacks:
/// the actual work happens on a dedicated thread, is throttled, and is skipped
/// entirely when the rendered menu would be identical.
pub(crate) fn refresh_tray_menu(app: &tauri::AppHandle) {
    let sender = TRAY_REFRESH_SENDER.get_or_init(|| start_tray_refresh_worker(app.clone()));
    // Capacity 1: a queued request already means "rebuild once more after the
    // current pass", so dropping further requests loses no information.
    let _ = sender.try_send(());
}

fn start_tray_refresh_worker(app: tauri::AppHandle) -> SyncSender<()> {
    let (sender, receiver) = sync_channel::<()>(1);
    let spawn_result = std::thread::Builder::new()
        .name("petaldesk-tray-refresh".to_string())
        .spawn(move || {
            let mut last_refresh: Option<Instant> = None;
            while receiver.recv().is_ok() {
                // Space out consecutive rebuilds. Autosave bursts collapse into
                // a single trailing refresh instead of one refresh per keystroke
                // pause.
                if let Some(elapsed) = last_refresh.map(|at| at.elapsed()) {
                    if let Some(wait) = TRAY_REFRESH_MIN_INTERVAL.checked_sub(elapsed) {
                        std::thread::sleep(wait);
                    }
                }
                // Drain requests that arrived while throttling so they collapse
                // into this single rebuild.
                while receiver.try_recv().is_ok() {}
                refresh_tray_menu_now(&app);
                last_refresh = Some(Instant::now());
            }
        });
    if let Err(error) = spawn_result {
        eprintln!("无法启动托盘菜单刷新线程: {error}");
    }
    sender
}

/// Describes the visible tray menu so an unchanged menu can be left in place.
/// Only what the menu renders belongs here: note order, note titles and the
/// screenshot shortcut label.
fn tray_menu_signature(notes: &[models::NoteSummary], screenshot_shortcut: &str) -> String {
    let mut signature = String::with_capacity(notes.len() * 48);
    signature.push_str(screenshot_shortcut);
    for note in notes {
        signature.push('\u{1f}');
        signature.push_str(&note.id);
        signature.push('\u{1e}');
        signature.push_str(&tray_note_label(&note.title));
    }
    signature
}

fn refresh_tray_menu_now(app: &tauri::AppHandle) {
    let started = Instant::now();
    diagnostics::record(diagnostics::DiagnosticEvent::TrayRefreshStarted);
    // Building the menu costs one synchronous UI-thread hop per item, plus one
    // deferred main-thread drop per item of the outgoing menu. Skipping
    // unchanged rebuilds keeps body-only autosaves off the UI thread entirely.
    let Ok(notes) = app.state::<WorkspaceStore>().list_notes() else {
        diagnostics::record_warning(diagnostics::DiagnosticEvent::TrayRefreshFailed);
        return;
    };
    let shortcut = app.state::<ScreenshotStore>().settings().shortcut;
    let signature = tray_menu_signature(&notes, &shortcut);
    {
        let current = TRAY_MENU_SIGNATURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.as_deref() == Some(signature.as_str()) {
            diagnostics::record_duration(
                diagnostics::DiagnosticEvent::TrayRefreshSkipped,
                started.elapsed(),
            );
            return;
        }
    }
    let Ok(menu) = build_tray_menu(app, notes, &shortcut) else {
        diagnostics::record_warning(diagnostics::DiagnosticEvent::TrayRefreshFailed);
        return;
    };
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        diagnostics::record_warning(diagnostics::DiagnosticEvent::TrayRefreshFailed);
        return;
    };
    if tray.set_menu(Some(menu)).is_err() {
        diagnostics::record_warning(diagnostics::DiagnosticEvent::TrayRefreshFailed);
        return;
    }
    // Record only after the menu is actually installed, so a failed rebuild is
    // retried by the next request instead of being cached as current.
    *TRAY_MENU_SIGNATURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(signature);
    diagnostics::record_duration(
        diagnostics::DiagnosticEvent::TrayRefreshCompleted,
        started.elapsed(),
    );
}

fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::Setup);
    diagnostics::record(diagnostics::DiagnosticEvent::SetupStarted);
    diagnostics::start_main_loop_watchdog(app.handle().clone());
    let data_storage_path = app.state::<WorkspaceStore>().data_storage_path();
    recovery::recover_interrupted_shared_recovery(&data_storage_path)?;
    app.manage(MfaStore::load(&data_storage_path)?);
    app.manage(PasswordStore::load(&data_storage_path)?);
    // Start the secret endpoint only after the single-instance plugin has
    // accepted this process as primary. A secondary launch exits during plugin
    // setup and must never overwrite the primary process endpoint.
    app.manage(PasswordBrowserService::start());
    diagnostics::record(diagnostics::DiagnosticEvent::BrowserServiceStarted);
    screenshot::setup(app)?;
    password_browser::start_event_dispatcher(app.handle().clone());
    let notes = app.state::<WorkspaceStore>().list_notes()?;
    let shortcut = app.state::<ScreenshotStore>().settings().shortcut;
    // Seed the signature with what the initial menu renders so the first
    // refresh request does not rebuild an identical menu.
    *TRAY_MENU_SIGNATURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(tray_menu_signature(&notes, &shortcut));
    let menu = build_tray_menu(app.handle(), notes, &shortcut)?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(!cfg!(windows))
        .tooltip("飞花 - PetalDesk")
        .on_menu_event(|app, event| {
            let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::TrayMenu);
            diagnostics::mark_main_loop_progress();
            match event.id.as_ref() {
                "show" => {
                    diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuShow);
                    show_main_window(app);
                }
                "new-note" => {
                    diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuNewNote);
                    spawn_create_note(app);
                }
                ABOUT_MENU_ID => {
                    diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuAbout);
                    show_main_window(app);
                    let _ = app.emit_to("main", "open_about_dialog", ());
                }
                "quit" => {
                    diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuQuit);
                    let app = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        app.state::<MfaStore>().lock();
                        app.state::<PasswordStore>().lock();
                        long_screenshot::shutdown(&app);
                        app.exit(0);
                    });
                }
                id => {
                    if let Some(tool) = tool_from_tray_menu_id(id) {
                        diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuOpenTool);
                        spawn_open_tool(app, tool);
                        return;
                    }
                    if let Some(note_id) = note_id_from_tray_menu_id(id) {
                        diagnostics::record(diagnostics::DiagnosticEvent::TrayMenuOpenNote);
                        spawn_show_note(app, note_id.to_string());
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::TrayIcon);
                diagnostics::mark_main_loop_progress();
                diagnostics::record(diagnostics::DiagnosticEvent::TrayDoubleClick);
                // Runs on the event-loop thread, so read the settings on a
                // worker: blocking here stops the message pump and with it
                // every tray click and single-instance handshake.
                let app = tray.app_handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let settings = app.state::<WorkspaceStore>().tray_shortcut_settings();
                    if let Some(action) = current_tray_double_click_action(settings) {
                        spawn_tray_action(&app, action);
                    }
                });
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    diagnostics::record(diagnostics::DiagnosticEvent::TrayBuilt);

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
    updater::start_scheduler(app.handle().clone());
    diagnostics::record(diagnostics::DiagnosticEvent::SetupCompleted);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let autostart_launch = launch_at_login::current_launch_is_autostart();
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
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            // Keep this body free of file I/O and locks: it runs synchronously
            // on the message pump while the relaunched process waits inside
            // SendMessageW. Tracing happens on the worker instead.
            |app, arguments, _cwd| {
                let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::SingleInstance);
                if !launch_at_login::args_contain_autostart(arguments.iter()) {
                    spawn_primary_activation_action(app);
                }
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
        .manage(updater::UpdaterManager::default());
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    let app = builder
        .on_page_load(move |webview, payload| {
            if webview.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
                && INITIAL_ACTIVATION_SCHEDULED
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::PageLoad);
                diagnostics::mark_main_loop_progress();
                // `RunEvent::Ready` can arrive before WebView2 has finished
                // initializing its first controller. Waiting for the hidden
                // main page to finish loading avoids losing the configured
                // activation target on fast, installed launches.
                diagnostics::record(diagnostics::DiagnosticEvent::MainPageLoaded);
                if autostart_launch {
                    diagnostics::record(
                        diagnostics::DiagnosticEvent::ActivationAutostartSuppressed,
                    );
                } else {
                    spawn_primary_activation_action(webview.app_handle());
                }
            }
        })
        .setup(setup_tray)
        .on_window_event(|window, event| {
            let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::WindowEvent);
            diagnostics::mark_main_loop_progress();
            if let WindowEvent::CloseRequested { api, .. } = event {
                diagnostics::record(diagnostics::DiagnosticEvent::WindowCloseRequested);
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
                diagnostics::record(diagnostics::DiagnosticEvent::WindowDestroyed);
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
                } else if label == commands::PASSWORD_WINDOW_LABEL {
                    let closing_epoch = app.state::<PasswordStore>().deactivate();
                    let password_app = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        password_app
                            .state::<PasswordBrowserService>()
                            .finish_password_window_close(
                                &password_app.state::<PasswordStore>(),
                                closing_epoch,
                            );
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::set_default_editor_mode,
            commands::set_protect_sensitive_windows,
            commands::get_launch_at_login_status,
            commands::set_launch_at_login_enabled,
            commands::set_tray_shortcut_settings,
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
            network::collect_network_report,
            commands::close_note_window,
            commands::save_window_state,
            updater::get_update_settings,
            updater::set_update_settings,
            updater::get_update_state,
            updater::check_for_updates,
            updater::download_update,
            updater::postpone_update,
            updater::register_update_install_window,
            updater::unregister_update_install_window,
            updater::acknowledge_update_install,
            updater::install_update_and_restart,
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
            mfa::reorder_mfa_entries,
            mfa::set_mfa_entry_pinned,
            mfa::configure_mfa_recovery_password,
            mfa::unlock_mfa_with_recovery_password,
            mfa::scan_mfa_screen_qr,
            mfa::preview_mfa_qr_image,
            mfa::preview_mfa_uri,
            mfa::preview_mfa_uris,
            mfa::preview_mfa_manual,
            mfa::commit_mfa_import,
            mfa::commit_mfa_imports,
            mfa::cancel_mfa_import,
            mfa::update_mfa_entry,
            mfa::delete_mfa_entry,
            mfa::list_mfa_trash,
            mfa::restore_mfa_entry,
            mfa::permanently_delete_mfa_entry,
            mfa::empty_mfa_trash,
            mfa::reveal_mfa_code,
            mfa::export_mfa_entry,
            mfa::copy_mfa_code,
            mfa::lock_mfa_vault,
            passwords::get_password_status,
            passwords::list_password_entries,
            passwords::list_password_mfa_candidates,
            passwords::create_password_entry,
            passwords::update_password_entry,
            passwords::delete_password_entry,
            passwords::reveal_password,
            passwords::copy_password_username,
            passwords::copy_password_secret,
            passwords::copy_password_mfa_code,
            passwords::generate_password,
            passwords::set_password_capture_enabled,
            passwords::evaluate_password_capture,
            passwords::configure_password_recovery_password,
            passwords::unlock_passwords_with_recovery_password,
            passwords::lock_password_vault,
            password_browser::get_password_browser_status,
            password_browser::start_password_fill,
            password_browser::cancel_password_fill,
            password_browser::start_password_template_recording,
            password_browser::cancel_password_template_recording,
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
    // The single-instance plugin exits secondary launches during `build`.
    // Start the fixed-name logger only after that ownership check, otherwise a
    // blocked secondary launch could corrupt or rotate the primary's evidence.
    let _ = diagnostics::init();
    diagnostics::record(diagnostics::DiagnosticEvent::PrimaryInstanceConfirmed);
    diagnostics::record(diagnostics::DiagnosticEvent::StoresLoaded);
    diagnostics::record(diagnostics::DiagnosticEvent::AppBuilt);
    diagnostics::set_main_phase(diagnostics::MainPhase::Idle);

    app.run(|app, event| {
        diagnostics::mark_main_loop_progress();
        match event {
            RunEvent::Ready => {
                diagnostics::set_main_phase(diagnostics::MainPhase::Idle);
                diagnostics::record(diagnostics::DiagnosticEvent::RunReady);
            }
            RunEvent::Resumed => {
                diagnostics::record(diagnostics::DiagnosticEvent::RunResumed);
            }
            RunEvent::ExitRequested { api, code, .. } => {
                let _phase = diagnostics::enter_main_phase(diagnostics::MainPhase::Exit);
                diagnostics::record(diagnostics::DiagnosticEvent::ExitRequested);
                if code.is_none() {
                    #[cfg(not(target_os = "macos"))]
                    api.prevent_exit();
                    #[cfg(target_os = "macos")]
                    {
                        let _ = api;
                        app.state::<MfaStore>().lock();
                        app.state::<PasswordStore>().lock();
                        long_screenshot::shutdown(app);
                    }
                } else {
                    app.state::<MfaStore>().lock();
                    app.state::<PasswordStore>().lock();
                }
                // Explicit quit and restart paths finish long-capture cleanup on a
                // worker thread before requesting process exit. Repeating that
                // blocking cleanup here can stall Tauri's event loop while Windows
                // is already tearing down the process.
            }
            RunEvent::Exit => {
                diagnostics::set_main_phase(diagnostics::MainPhase::Exit);
                diagnostics::record_urgent(diagnostics::DiagnosticEvent::Exit);
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen {
                has_visible_windows,
                ..
            } => {
                if !has_visible_windows {
                    show_main_window(app);
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tauri 2 按窗口 label 授权窗口 API。工具窗口如果没进 capabilities 的
    /// `windows` 白名单，页面照常渲染、自定义命令照常可用，但 `close()`、
    /// `destroy()` 和标题栏拖动会被静默拒绝 —— 编译和其它单测都发现不了。
    /// 每加一个工具窗口都必须同步这个白名单。
    #[test]
    fn every_tool_window_label_is_in_the_capability_scope() {
        let capability: serde_json::Value = serde_json::from_str(include_str!(
            "../capabilities/default.json"
        ))
        .expect("capabilities/default.json 必须是合法 JSON");
        let scope: Vec<&str> = capability["windows"]
            .as_array()
            .expect("capabilities 必须声明 windows 数组")
            .iter()
            .map(|entry| entry.as_str().expect("窗口 label 必须是字符串"))
            .collect();

        for label in [
            commands::TIMER_WINDOW_LABEL,
            commands::REMINDER_WINDOW_LABEL,
            commands::GANTT_WINDOW_LABEL,
            commands::MFA_WINDOW_LABEL,
            commands::PASSWORD_WINDOW_LABEL,
            commands::NETWORK_WINDOW_LABEL,
        ] {
            assert!(
                scope.contains(&label),
                "工具窗口 `{label}` 不在 capabilities/default.json 的 windows 白名单里，\
                 该窗口将无法关闭或拖动"
            );
        }
    }

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

    fn summary(id: &str, title: &str) -> models::NoteSummary {
        models::NoteSummary {
            id: id.to_string(),
            title: title.to_string(),
            excerpt: String::new(),
            editor_mode: models::DEFAULT_EDITOR_MODE.to_string(),
            color: "yellow".to_string(),
            pinned: false,
            read_only: false,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            schema_version: models::SCHEMA_VERSION,
            revision: 1,
        }
    }

    #[test]
    fn tray_signature_ignores_changes_the_menu_does_not_render() {
        let notes = vec![summary("a", "第一"), summary("b", "第二")];
        let baseline = tray_menu_signature(&notes, "F1");

        // A body-only autosave bumps revision and updated_at but renders the
        // same menu, so it must not trigger a rebuild.
        let mut edited = notes.clone();
        edited[0].revision = 9;
        edited[0].updated_at = "2026-08-04T10:00:00Z".to_string();
        edited[0].excerpt = "新的正文".to_string();
        edited[0].color = "blue".to_string();
        assert_eq!(tray_menu_signature(&edited, "F1"), baseline);

        // Anything the menu shows must change it.
        let mut renamed = notes.clone();
        renamed[0].title = "改名".to_string();
        assert_ne!(tray_menu_signature(&renamed, "F1"), baseline);
        assert_ne!(tray_menu_signature(&notes, "F2"), baseline);
        assert_ne!(
            tray_menu_signature(&[notes[1].clone(), notes[0].clone()], "F1"),
            baseline
        );
        assert_ne!(tray_menu_signature(&notes[..1], "F1"), baseline);
    }

    #[test]
    fn tray_signature_separates_adjacent_note_fields() {
        // Without delimiters these two lists would collapse to the same string.
        let left = vec![summary("a", "bc")];
        let right = vec![summary("ab", "c")];
        assert_ne!(
            tray_menu_signature(&left, "F1"),
            tray_menu_signature(&right, "F1")
        );

        let split = vec![summary("a", ""), summary("", "b")];
        let joined = vec![summary("a", "b")];
        assert_ne!(
            tray_menu_signature(&split, "F1"),
            tray_menu_signature(&joined, "F1")
        );
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
    fn tray_modifier_actions_use_defaults_and_reject_multiple_modifiers() {
        let settings = models::TrayShortcutSettings::default();
        assert_eq!(
            tray_action_for_modifiers(settings, false, false, false),
            Some(models::TrayShortcutAction::FirstNote)
        );
        assert_eq!(
            tray_action_for_modifiers(settings, true, false, false),
            Some(models::TrayShortcutAction::Gantt)
        );
        assert_eq!(
            tray_action_for_modifiers(settings, false, true, false),
            Some(models::TrayShortcutAction::Mfa)
        );
        assert_eq!(
            tray_action_for_modifiers(settings, false, false, true),
            Some(models::TrayShortcutAction::MainWindow)
        );
        assert_eq!(tray_action_for_modifiers(settings, true, true, false), None);
    }
}

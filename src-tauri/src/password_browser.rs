use crate::browser_bridge::BrowserFamily;
use crate::browser_secret_bridge::{BrowserSecretBridge, BrowserSecretEvent, DiagEntry};
use crate::error::{AppError, AppResult};
use crate::passwords::{
    PasswordCaptureAccount, PasswordCaptureAction, PasswordCaptureCandidate, PasswordEntryInput,
    PasswordEntrySummary, PasswordEntryUpdateInput, PasswordStore, PasswordTemplateDefinition,
    SensitiveText,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const FILL_TTL: Duration = Duration::from_secs(5 * 60);
const CAPTURE_TTL: Duration = Duration::from_secs(30);
const TEMPLATE_RECORDING_TTL: Duration = Duration::from_secs(5 * 60);
const TEMPLATE_RECORDING_TIMEOUT: Duration = Duration::from_secs(15);
const BADGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BADGE_ACCOUNTS: usize = 16;
const STATUS_DIAGNOSTIC_LIMIT: usize = 20;
/// The native host rewrites its session heartbeat every 2s; anything older is
/// treated as a dead stdio layer.
const STDIO_SESSION_MAX_AGE: Duration = Duration::from_secs(6);
#[cfg(windows)]
const FIREFOX_AMO_URL: &str = "https://starsliao.github.io/PetalDesk/firefox.html";
static EVENT_DISPATCHER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct FillSession {
    connection_id: String,
    entry_id: String,
    origin: String,
    offer_id: Option<String>,
    tab_id: Option<i64>,
    frame_id: Option<i64>,
    document_id: Option<String>,
    expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum TemplateRecordingState {
    Opening,
    Recording,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone)]
struct TemplateRecordingSession {
    connection_id: String,
    entry_id: String,
    origin: String,
    tab_id: Option<i64>,
    frame_id: Option<i64>,
    document_id: Option<String>,
    state: TemplateRecordingState,
    expires_at: Instant,
}

struct PendingCapture {
    connection_id: String,
    entry_id: Option<String>,
    account_choices: Vec<PasswordCaptureAccount>,
    matched_action: String,
    origin: String,
    username: String,
    password: String,
    allow_insecure_http: bool,
    tab_id: i64,
    frame_id: i64,
    document_id: String,
    prompt_origin: String,
    created_at: Instant,
    save_pending: bool,
}

impl Drop for PendingCapture {
    fn drop(&mut self) {
        self.username.zeroize();
        self.password.zeroize();
    }
}

pub struct PasswordBrowserService {
    bridge: BrowserSecretBridge,
    fills: Mutex<HashMap<String, FillSession>>,
    captures: Mutex<HashMap<String, PendingCapture>>,
    recordings: Mutex<HashMap<String, TemplateRecordingSession>>,
    // Last setCaptureEnabled payload sent per connection; suppresses duplicate
    // broadcasts triggered by password-status polling.
    synced_capture: Mutex<HashMap<String, (bool, Vec<String>)>>,
    // Active tab origins per connection, reported by the extension for badge
    // account counts.
    badge_tabs: Mutex<HashMap<String, HashMap<i64, String>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordBrowserStatus {
    browser: &'static str,
    connection: &'static str,
    extension_installed: bool,
    native_host_installed: bool,
    extension_version: Option<String>,
    install_url: Option<String>,
    capture_permission: &'static str,
    authentication_consent: bool,
    stdio_connected: bool,
    pipe_connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    diagnostics: Vec<DiagEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_request_outcome: Option<DiagEntry>,
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordFillTicket {
    session_id: String,
    entry_id: String,
    browser: &'static str,
    origin: String,
    expires_at: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordTemplateRecordingTicket {
    session_id: String,
    entry_id: String,
    origin: String,
    state: TemplateRecordingState,
    expires_at: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl PasswordBrowserService {
    pub fn start() -> Self {
        let bridge = BrowserSecretBridge::start().unwrap_or_else(|error| {
            eprintln!("密码浏览器安全通道未启动: {error}");
            BrowserSecretBridge::disabled()
        });
        Self {
            bridge,
            fills: Mutex::new(HashMap::new()),
            captures: Mutex::new(HashMap::new()),
            recordings: Mutex::new(HashMap::new()),
            synced_capture: Mutex::new(HashMap::new()),
            badge_tabs: Mutex::new(HashMap::new()),
        }
    }

    fn status(&self) -> PasswordBrowserStatus {
        #[cfg(not(windows))]
        {
            return PasswordBrowserStatus {
                browser: "firefox",
                connection: "unsupported",
                extension_installed: false,
                native_host_installed: false,
                extension_version: None,
                install_url: None,
                capture_permission: "unavailable",
                authentication_consent: false,
                stdio_connected: false,
                pipe_connected: false,
                connection_id: None,
                diagnostics: Vec::new(),
                last_request_outcome: None,
                message: Some("密码浏览器集成首版仅支持 Windows。".to_string()),
            };
        }
        // The stdio layer health comes from the native host's session
        // heartbeat file; the pipe layer from this process's secret bridge.
        #[cfg(windows)]
        let bridge_session = latest_firefox_bridge_session();
        #[cfg(windows)]
        let stdio_connected = bridge_session.as_ref().is_some_and(|session| {
            unix_time_ms().saturating_sub(session.last_seen_unix_ms)
                <= STDIO_SESSION_MAX_AGE.as_millis()
        });
        #[cfg(windows)]
        let connection_id = self.bridge.latest_connection_id(BrowserFamily::Firefox);
        #[cfg(windows)]
        let had_connection = connection_id.is_some();
        #[cfg(windows)]
        let extension_status_result = connection_id.as_deref().map(|connection_id| {
            self.bridge.request_connection(
                connection_id,
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::from_secs(2),
            )
        });
        #[cfg(windows)]
        let (extension_status, status_error) = match extension_status_result {
            Some(Ok(status)) => (Some(status), None),
            Some(Err(error)) => {
                eprintln!("读取 Firefox 密码扩展状态失败: {error}");
                (None, Some(error))
            }
            None => (None, None),
        };
        // A failed send may retire a half-open connection, so check this after
        // the request instead of preserving a stale pre-request snapshot.
        #[cfg(windows)]
        let connected = self.bridge.is_connected(BrowserFamily::Firefox);
        #[cfg(windows)]
        let authentication_consent = extension_status
            .as_ref()
            .and_then(|status| status.get("authenticationConsent"))
            .and_then(Value::as_bool);
        #[cfg(windows)]
        let unsupported = status_error
            .as_deref()
            .is_some_and(password_status_error_is_unsupported);
        #[cfg(windows)]
        let known_extension = had_connection || connected;
        #[cfg(windows)]
        PasswordBrowserStatus {
            browser: "firefox",
            connection: if connected {
                "connected"
            } else {
                "disconnected"
            },
            extension_installed: known_extension,
            native_host_installed: known_extension,
            extension_version: bridge_session.and_then(|session| session.extension_version),
            install_url: (!known_extension).then(|| FIREFOX_AMO_URL.to_string()),
            capture_permission: match (authentication_consent, known_extension, unsupported) {
                (_, _, true) => "unavailable",
                (Some(true), _, _) => "granted",
                (Some(false), _, _) => "unknown",
                (None, true, _) => "unknown",
                _ => "unavailable",
            },
            authentication_consent: authentication_consent.unwrap_or(false),
            stdio_connected,
            pipe_connected: connected,
            connection_id,
            diagnostics: self.bridge.diag_snapshot(STATUS_DIAGNOSTIC_LIMIT),
            last_request_outcome: self.bridge.last_request_outcome(),
            message: if unsupported {
                Some("当前 Firefox 扩展不支持密码功能，请更新扩展。".to_string())
            } else if status_error.is_some() {
                Some("Firefox 密码通道通信异常；请稍后重试，必要时重启飞花或 Firefox。".to_string())
            } else if !connected {
                Some("Firefox 扩展或本机通信组件尚未连接；仍可复制账号和密码。".to_string())
            } else if authentication_consent == Some(false) {
                Some("Firefox 扩展的密码权限状态异常；请更新或重新安装扩展。".to_string())
            } else {
                None
            },
        }
    }

    fn start_fill(&self, app: &AppHandle, entry_id: &str) -> AppResult<PasswordFillTicket> {
        let data = app.state::<PasswordStore>().browser_fill_data(entry_id)?;
        let connection_id = self.single_firefox_connection()?;
        let session_id = Uuid::new_v4().to_string();
        let expires_at = Instant::now() + FILL_TTL;
        lock_unpoisoned(&self.fills).insert(
            session_id.clone(),
            FillSession {
                connection_id: connection_id.clone(),
                entry_id: data.entry_id.clone(),
                origin: data.origin.clone(),
                offer_id: None,
                tab_id: None,
                frame_id: None,
                document_id: None,
                expires_at,
            },
        );
        let result = self.bridge.request_connection(
            &connection_id,
            "password.open",
            serde_json::json!({
                "sessionId": session_id,
                "entryId": data.entry_id,
                "url": data.login_url,
                "origin": data.origin,
                "allowedOrigins": [data.origin],
                "allowInsecureHttp": data.allow_insecure_http,
            }),
            REQUEST_TIMEOUT,
        );
        result
            .map_err(|error| {
                lock_unpoisoned(&self.fills).remove(&session_id);
                browser_error("password_fill_start_failed", error)
            })?;
        Ok(PasswordFillTicket {
            session_id,
            entry_id: entry_id.to_string(),
            browser: "firefox",
            origin: data.origin.clone(),
            expires_at: unix_time_ms().saturating_add(FILL_TTL.as_millis()),
        })
    }

    fn cancel_fill(&self, session_id: &str) -> AppResult<()> {
        let session = lock_unpoisoned(&self.fills).remove(session_id);
        let Some(session) = session else {
            return Ok(());
        };
        self.bridge
            .request_connection(
                &session.connection_id,
                "password.cancelFill",
                serde_json::json!({ "sessionId": session_id }),
                REQUEST_TIMEOUT,
            )
            .map(|_| ())
            .map_err(|error| browser_error("password_fill_cancel_failed", error))
    }

    fn start_template_recording(
        &self,
        app: &AppHandle,
        entry_id: &str,
    ) -> AppResult<PasswordTemplateRecordingTicket> {
        let data = app.state::<PasswordStore>().browser_fill_data(entry_id)?;
        let connection_id = self.single_firefox_connection()?;
        let session_id = Uuid::new_v4().to_string();
        let expires_at = Instant::now() + TEMPLATE_RECORDING_TTL;
        let session = TemplateRecordingSession {
            connection_id: connection_id.clone(),
            entry_id: data.entry_id.clone(),
            origin: data.origin.clone(),
            tab_id: None,
            frame_id: None,
            document_id: None,
            state: TemplateRecordingState::Opening,
            expires_at,
        };
        lock_unpoisoned(&self.recordings).insert(session_id.clone(), session);
        let response = self.bridge.request_connection(
            &connection_id,
            "password.startTemplateRecording",
            serde_json::json!({
                "sessionId": session_id,
                "entryId": data.entry_id,
                "url": data.login_url,
                "origin": data.origin,
                "allowInsecureHttp": data.allow_insecure_http,
            }),
            TEMPLATE_RECORDING_TIMEOUT,
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                lock_unpoisoned(&self.recordings).remove(&session_id);
                return Err(browser_error("password_template_recording_failed", error));
            }
        };
        if response
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|value| value != session_id)
            || response
                .get("origin")
                .and_then(Value::as_str)
                .is_some_and(|value| value != data.origin)
        {
            lock_unpoisoned(&self.recordings).remove(&session_id);
            return Err(browser_error(
                "password_template_recording_binding",
                "Firefox 返回了不匹配的模板录制会话。",
            ));
        }
        let tab_id = response.get("tabId").and_then(Value::as_i64);
        let frame_id = response.get("frameId").and_then(Value::as_i64);
        if tab_id.is_some_and(|value| value < 0) || frame_id.is_some_and(|value| value != 0) {
            lock_unpoisoned(&self.recordings).remove(&session_id);
            return Err(browser_error(
                "password_template_recording_binding",
                "模板只能绑定 Firefox 的有效标签页和顶层文档。",
            ));
        }
        let mut recordings = lock_unpoisoned(&self.recordings);
        let recording = recordings.get_mut(&session_id).ok_or_else(|| {
            browser_error(
                "password_template_recording_expired",
                "模板录制会话已失效。",
            )
        })?;
        if let Some(tab_id) = tab_id {
            recording.tab_id = Some(tab_id);
        }
        if let Some(frame_id) = frame_id {
            recording.frame_id = Some(frame_id);
        }
        recording.state = match response.get("state").and_then(Value::as_str) {
            Some("recording") => TemplateRecordingState::Recording,
            _ => TemplateRecordingState::Opening,
        };
        Ok(template_recording_ticket(&session_id, recording, None))
    }

    fn cancel_template_recording(&self, session_id: &str) -> AppResult<()> {
        let session = lock_unpoisoned(&self.recordings).remove(session_id);
        let Some(session) = session else {
            return Ok(());
        };
        self.bridge
            .request_connection(
                &session.connection_id,
                "password.cancelTemplateRecording",
                serde_json::json!({ "sessionId": session_id }),
                REQUEST_TIMEOUT,
            )
            .map(|_| ())
            .map_err(|error| browser_error("password_template_cancel_failed", error))
    }

    pub(crate) fn suspend_capture(&self) {
        let connection_ids = self.bridge.connection_ids(BrowserFamily::Firefox);
        for connection_id in connection_ids {
            let _ = self.bridge.request_connection(
                &connection_id,
                "password.setCaptureEnabled",
                serde_json::json!({ "enabled": false, "insecureOrigins": [] }),
                Duration::from_secs(2),
            );
        }
        // The extension-side state no longer matches the cache, so the next
        // successful sync must broadcast again.
        lock_unpoisoned(&self.synced_capture).clear();
        lock_unpoisoned(&self.captures).clear();
        let fills = lock_unpoisoned(&self.fills).drain().collect::<Vec<_>>();
        for (session_id, session) in fills {
            let _ = self.bridge.request_connection(
                &session.connection_id,
                "password.cancelFill",
                serde_json::json!({ "sessionId": session_id }),
                Duration::from_secs(2),
            );
        }
        let recordings = lock_unpoisoned(&self.recordings)
            .drain()
            .collect::<Vec<_>>();
        for (session_id, session) in recordings {
            let _ = self.bridge.request_connection(
                &session.connection_id,
                "password.cancelTemplateRecording",
                serde_json::json!({ "sessionId": session_id }),
                Duration::from_secs(2),
            );
        }
    }

    fn single_firefox_connection(&self) -> AppResult<String> {
        let connections = self.bridge.connection_ids(BrowserFamily::Firefox);
        match connections.as_slice() {
            [] => Err(browser_error(
                "password_extension_missing",
                "Firefox 扩展尚未连接。",
            )),
            [connection_id] => Ok(connection_id.clone()),
            _ => Err(browser_error(
                "password_extension_ambiguous",
                "检测到多个 Firefox 扩展配置，请只保留一个 Firefox 配置后重试。",
            )),
        }
    }

    pub(crate) fn sync_capture_from_store(&self, store: &PasswordStore) {
        let Ok(epoch) = store.require_any_epoch() else {
            self.suspend_capture();
            return;
        };
        let Ok(status) = store.status_at(epoch) else {
            self.suspend_capture();
            return;
        };
        if status.locked
            || !status.available
            || status.recovery_state != crate::passwords::PasswordRecoveryState::Ready
        {
            self.suspend_capture();
            return;
        }
        let insecure_origins = store
            .list_entries_at(epoch)
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.allow_insecure_http)
            .map(|entry| entry.origin)
            .collect::<Vec<_>>();
        let connection_ids = self.bridge.connection_ids(BrowserFamily::Firefox);
        if connection_ids.is_empty() {
            return;
        }
        for connection_id in connection_ids {
            // Login detection is mandatory since 0.7.2: the stored setting
            // is ignored and capture is always pushed as enabled. Skip the
            // broadcast when nothing changed since the last successful sync.
            let desired = (true, insecure_origins.clone());
            if lock_unpoisoned(&self.synced_capture).get(&connection_id) == Some(&desired) {
                continue;
            }
            let result = self.bridge.request_connection(
                &connection_id,
                "password.setCaptureEnabled",
                serde_json::json!({
                    "enabled": desired.0,
                    "insecureOrigins": desired.1,
                }),
                Duration::from_secs(2),
            );
            let mut synced = lock_unpoisoned(&self.synced_capture);
            if result.is_ok() {
                synced.insert(connection_id, desired);
            } else {
                synced.remove(&connection_id);
            }
        }
    }

    fn handle_event(&self, app: &AppHandle, event: BrowserSecretEvent) {
        if event.browser != BrowserFamily::Firefox {
            return;
        }
        self.prune(app);
        match event.event.as_str() {
            "connectionReady" => {
                // A live extension keeps the decrypted vault usable for the
                // background session even while the password window is closed.
                let store = app.state::<PasswordStore>();
                store.activate_browser_session();
                self.sync_capture_from_store(&store);
            }
            "connectionClosed" => {
                self.clear_connection_state(app, &event.connection_id);
                self.sync_capture_from_store(&app.state());
            }
            "originActive" => self.handle_origin_active(&app.state(), &event),
            "fillRequest" => self.handle_fill_request(&app.state(), &event),
            "copySecret" => self.handle_copy_secret(&app.state(), &event),
            "deleteEntry" => self.handle_delete_entry(app, &event),
            "openPasswordManager" => {
                if let Err(error) = crate::commands::open_password_window(app) {
                    eprintln!("打开密码管理器窗口失败: {error}");
                }
            }
            "tabReady" => self.handle_tab_ready(app, &event),
            "fillConfirm" => self.handle_fill_confirm(&app.state(), &event),
            "fillResult" => self.handle_fill_result(&event),
            "captureCandidate" => self.handle_capture_candidate(&app.state(), &event),
            "pageClosed" => self.handle_page_closed(&event),
            "saveDecision" => self.handle_save_decision(app, &event),
            "templateRecordingReady" => self.handle_template_recording_ready(app, &event),
            "templateRecordingResult" => self.handle_template_recording_result(app, &event),
            "templateRecordingCancelled" => self.handle_template_recording_cancelled(app, &event),
            "consentChanged" => {
                if event.payload.get("granted").and_then(Value::as_bool) == Some(true) {
                    self.sync_capture_from_store(&app.state());
                    self.resume_pending_fills(app, &event.connection_id);
                }
            }
            _ => {}
        }
    }

    fn clear_connection_state(&self, app: &AppHandle, connection_id: &str) {
        let removed_recordings = self.clear_connection_state_data(connection_id);
        for (session_id, session) in removed_recordings {
            emit_template_recording_status(
                app,
                template_recording_ticket(
                    &session_id,
                    &session,
                    Some("Firefox 连接已断开，模板录制已取消。".to_string()),
                ),
                TemplateRecordingState::Failed,
            );
        }
    }

    fn clear_connection_state_data(
        &self,
        connection_id: &str,
    ) -> Vec<(String, TemplateRecordingSession)> {
        // Dropping PendingCapture immediately zeroizes its username and
        // password. Do this before rebroadcasting capture settings so a stale
        // page cannot submit a candidate after its native connection closed.
        lock_unpoisoned(&self.fills).retain(|_, session| session.connection_id != connection_id);
        lock_unpoisoned(&self.captures).retain(|_, capture| capture.connection_id != connection_id);
        lock_unpoisoned(&self.badge_tabs).remove(connection_id);
        lock_unpoisoned(&self.synced_capture).remove(connection_id);

        let recording_ids = {
            let recordings = lock_unpoisoned(&self.recordings);
            recordings
                .iter()
                .filter(|(_, session)| session.connection_id == connection_id)
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>()
        };
        let mut recordings = lock_unpoisoned(&self.recordings);
        recording_ids
            .into_iter()
            .filter_map(|session_id| {
                recordings
                    .remove(&session_id)
                    .map(|session| (session_id, session))
            })
            .collect::<Vec<_>>()
    }

    fn handle_page_closed(&self, event: &BrowserSecretEvent) {
        let Some(tab_id) = event.payload.get("tabId").and_then(Value::as_i64) else {
            return;
        };
        let document_id = match event.payload.get("documentId") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() && value.len() <= 256 => {
                Some(value.as_str())
            }
            _ => return,
        };

        // The content script clears its own references on pagehide, while this
        // event removes the native copy that already crossed the bridge. Drop
        // matching values under the connection/tab/document binding so
        // PendingCapture's Drop implementation zeroizes username and password.
        lock_unpoisoned(&self.fills).retain(|_, session| {
            !(session.connection_id == event.connection_id
                && session.tab_id == Some(tab_id)
                && document_id.map_or(true, |value| session.document_id.as_deref() == Some(value)))
        });
        lock_unpoisoned(&self.captures).retain(|_, capture| {
            !(capture.connection_id == event.connection_id
                && capture.tab_id == tab_id
                && document_id.map_or(true, |value| capture.document_id == value))
        });
        lock_unpoisoned(&self.recordings).retain(|_, recording| {
            !(recording.connection_id == event.connection_id && recording.tab_id == Some(tab_id))
        });
    }

    fn handle_tab_ready(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        {
            let mut fills = lock_unpoisoned(&self.fills);
            let Some(session) = fills.get_mut(session_id) else {
                return;
            };
            if session.connection_id != event.connection_id {
                return;
            }
            if !bind_fill_tab_ready(session, &event.payload) {
                fills.remove(session_id);
                return;
            }
            session.offer_id = None;
            session.expires_at = Instant::now() + FILL_TTL;
        }
        self.offer_fill(app, session_id);
    }

    fn offer_fill(&self, app: &AppHandle, session_id: &str) {
        let session = lock_unpoisoned(&self.fills).get(session_id).cloned();
        let Some(mut session) = session else { return };
        let offer_id = Uuid::new_v4().to_string();
        session.offer_id = Some(offer_id.clone());
        if let Some(current) = lock_unpoisoned(&self.fills).get_mut(session_id) {
            if current.connection_id != session.connection_id
                || current.tab_id != session.tab_id
                || current.frame_id != session.frame_id
                || current.document_id != session.document_id
            {
                return;
            }
            current.offer_id = Some(offer_id);
        } else {
            return;
        }
        let store = app.state::<PasswordStore>();
        let Ok(data) = store
            .require_any_epoch()
            .and_then(|epoch| store.browser_fill_data_at(&session.entry_id, epoch))
        else {
            lock_unpoisoned(&self.fills).remove(session_id);
            return;
        };
        let result = self.bridge.request_connection(
            &session.connection_id,
            "password.offerFill",
            serde_json::json!({
                "sessionId": session_id,
                "entryId": data.entry_id,
                "offerId": session.offer_id,
                "origin": session.origin,
                "tabId": session.tab_id,
                "frameId": session.frame_id,
                "documentId": session.document_id,
                "username": data.username,
                "userTemplate": data.user_template,
                "allowInsecureHttp": data.allow_insecure_http,
            }),
            REQUEST_TIMEOUT,
        );
        if result.is_err() {
            lock_unpoisoned(&self.fills).remove(session_id);
        }
    }

    fn resume_pending_fills(&self, app: &AppHandle, connection_id: &str) {
        let sessions = lock_unpoisoned(&self.fills)
            .iter()
            .filter(|(_, session)| {
                session.connection_id == connection_id
                    && session.offer_id.is_none()
                    && session.tab_id.is_some()
                    && session.frame_id == Some(0)
                    && session.document_id.is_some()
            })
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        for session_id in sessions {
            self.offer_fill(app, &session_id);
        }
    }

    fn handle_fill_confirm(&self, store: &PasswordStore, event: &BrowserSecretEvent) {
        let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let session = lock_unpoisoned(&self.fills).get(session_id).cloned();
        let Some(session) = session else { return };
        if session.connection_id != event.connection_id {
            return;
        }
        if event.payload.get("offerId").and_then(Value::as_str) != session.offer_id.as_deref()
            || !fill_confirmation_matches(&session, &event.payload)
        {
            lock_unpoisoned(&self.fills).remove(session_id);
            return;
        }
        let Ok(data) = store
            .require_any_epoch()
            .and_then(|epoch| store.browser_fill_data_at(&session.entry_id, epoch))
        else {
            lock_unpoisoned(&self.fills).remove(session_id);
            return;
        };
        // Bind the session to the frame that confirmed the fill: for a
        // same-site iframe fill this is the first moment the real frame is
        // known, and the credentials must go back to that frame.
        let frame_id = event.payload.get("frameId").and_then(Value::as_i64);
        {
            let mut fills = lock_unpoisoned(&self.fills);
            let Some(current) = fills.get_mut(session_id) else {
                return;
            };
            current.frame_id = frame_id;
        }
        let result = self.bridge.request_connection(
            &session.connection_id,
            "password.provideCredentials",
            serde_json::json!({
                "sessionId": session_id,
                "offerId": session.offer_id,
                "origin": session.origin,
                "tabId": session.tab_id,
                "frameId": frame_id,
                "documentId": session.document_id,
                "username": data.username,
                "password": data.password,
            }),
            REQUEST_TIMEOUT,
        );
        let needs_next_step = result
            .as_ref()
            .ok()
            .and_then(|value| value.get("needsNextStep"))
            .and_then(Value::as_bool)
            == Some(true);
        let mut fills = lock_unpoisoned(&self.fills);
        if needs_next_step {
            if let Some(current) = fills.get_mut(session_id) {
                current.offer_id = None;
                current.expires_at = Instant::now() + FILL_TTL;
            }
        } else {
            fills.remove(session_id);
        }
    }

    fn handle_fill_result(&self, event: &BrowserSecretEvent) {
        if let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) {
            let mut fills = lock_unpoisoned(&self.fills);
            let Some(session) = fills.get_mut(session_id) else {
                return;
            };
            if session.connection_id != event.connection_id {
                return;
            }
            if !fill_target_matches(session, &event.payload) {
                fills.remove(session_id);
                return;
            }
            if event.payload.get("needsNextStep").and_then(Value::as_bool) == Some(true) {
                session.offer_id = None;
                session.expires_at = Instant::now() + FILL_TTL;
            } else {
                fills.remove(session_id);
            }
        }
    }

    /// The popup asks to fill one entry into the current tab. Unlike
    /// `password.open` the page is already known, so the session is bound to
    /// the reported tab/document before the offer is sent.
    fn handle_fill_request(&self, store: &PasswordStore, event: &BrowserSecretEvent) {
        let Some(entry_id) = event
            .payload
            .get("entryId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
        else {
            return;
        };
        let Some(tab_id) = event
            .payload
            .get("tabId")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
        else {
            return;
        };
        // Popup fills always target the top-level document.
        if event
            .payload
            .get("frameId")
            .and_then(Value::as_i64)
            .is_some_and(|value| value != 0)
        {
            return;
        }
        let Some(document_id) = event
            .payload
            .get("documentId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .map(str::to_string)
        else {
            return;
        };
        let Some(origin) = event
            .payload
            .get("origin")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 2048)
        else {
            return;
        };
        let data = store
            .require_any_epoch()
            .and_then(|epoch| store.browser_fill_data_at(entry_id, epoch));
        let data = match data {
            Ok(data) => data,
            Err(error) => {
                self.bridge.record_event(
                    "fill",
                    "fill-request-rejected",
                    format!("tabId={tab_id} origin={origin} reason={}", error.code),
                );
                // Locked vault or a stale entry id: push a locked badge so the
                // popup re-reads the state instead of waiting for the fill.
                self.push_badge(&event.connection_id, tab_id, origin, true, Vec::new());
                return;
            }
        };
        if data.origin != origin {
            self.bridge.record_event(
                "fill",
                "fill-request-rejected",
                format!("tabId={tab_id} origin={origin} reason=origin-mismatch"),
            );
            return;
        }
        let session_id = Uuid::new_v4().to_string();
        let offer_id = Uuid::new_v4().to_string();
        lock_unpoisoned(&self.fills).insert(
            session_id.clone(),
            FillSession {
                connection_id: event.connection_id.clone(),
                entry_id: data.entry_id.clone(),
                origin: data.origin.clone(),
                offer_id: Some(offer_id.clone()),
                tab_id: Some(tab_id),
                frame_id: Some(0),
                document_id: Some(document_id.clone()),
                expires_at: Instant::now() + FILL_TTL,
            },
        );
        let result = self.bridge.request_connection(
            &event.connection_id,
            "password.offerFillDirect",
            serde_json::json!({
                "sessionId": session_id,
                "entryId": data.entry_id,
                "offerId": offer_id,
                "tabId": tab_id,
                "frameId": 0,
                "documentId": document_id,
                "origin": data.origin,
                "username": data.username,
                "userTemplate": data.user_template,
                "allowInsecureHttp": data.allow_insecure_http,
            }),
            REQUEST_TIMEOUT,
        );
        self.bridge.record_event(
            "fill",
            "offer-fill-direct",
            format!("tabId={tab_id} origin={origin} ok={}", result.is_ok()),
        );
        if result.is_err() {
            lock_unpoisoned(&self.fills).remove(&session_id);
        }
    }

    /// The popup's account menu asks to copy one entry field into the system
    /// clipboard. `copy_field_at` validates the entry id format and
    /// existence, and reuses the store's clipboard lease, so the copy keeps
    /// the same auto-clear and window-close semantics as the password
    /// window's copy commands. Failures only land in the diagnostic log and
    /// never contain secret material.
    fn handle_copy_secret(&self, store: &PasswordStore, event: &BrowserSecretEvent) {
        let Some(entry_id) = event
            .payload
            .get("entryId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
        else {
            self.bridge
                .record_event("popup", "copy-secret-failed", "reason=invalid-entry-id");
            return;
        };
        let password = match event.payload.get("field").and_then(Value::as_str) {
            Some("username") => false,
            Some("password") => true,
            _ => {
                self.bridge
                    .record_event("popup", "copy-secret-failed", "reason=invalid-field");
                return;
            }
        };
        let result = store
            .require_any_epoch()
            .and_then(|epoch| store.copy_field_at(entry_id, password, epoch));
        if let Err(error) = result {
            self.bridge.record_event(
                "popup",
                "copy-secret-failed",
                format!(
                    "field={} reason={}",
                    if password { "password" } else { "username" },
                    error.code
                ),
            );
        }
    }

    /// The popup's account menu asks to delete one vault entry. On success
    /// the password window is notified and badges are recomputed, mirroring
    /// the save-decision flow.
    fn handle_delete_entry(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let store = app.state::<PasswordStore>();
        if let Some(entry_id) = self.delete_entry_from_event(&store, event) {
            let _ = app.emit_to(
                "passwords",
                "password_entries_changed",
                serde_json::json!({ "entryId": entry_id, "action": "delete" }),
            );
            self.refresh_badges(&store);
        }
    }

    /// Applies the popup's delete request. Returns the deleted entry id so
    /// the dispatcher can notify the password window and refresh badges.
    fn delete_entry_from_event(
        &self,
        store: &PasswordStore,
        event: &BrowserSecretEvent,
    ) -> Option<String> {
        let Some(entry_id) = event
            .payload
            .get("entryId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
        else {
            self.bridge
                .record_event("popup", "delete-entry-failed", "reason=invalid-entry-id");
            return None;
        };
        match store
            .require_any_epoch()
            .and_then(|epoch| store.delete_entry_at(entry_id, epoch))
        {
            Ok(()) => Some(entry_id.to_string()),
            Err(error) => {
                self.bridge.record_event(
                    "popup",
                    "delete-entry-failed",
                    format!("reason={}", error.code),
                );
                None
            }
        }
    }

    fn handle_origin_active(&self, store: &PasswordStore, event: &BrowserSecretEvent) {
        let Some(tab_id) = event
            .payload
            .get("tabId")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
        else {
            return;
        };
        let origin = event
            .payload
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if origin.is_empty() {
            // The tab has no fillable origin (about: pages, the new-tab page,
            // ...), so only the tracking entry is dropped.
            if let Some(tabs) = lock_unpoisoned(&self.badge_tabs).get_mut(&event.connection_id) {
                tabs.remove(&tab_id);
            }
            return;
        }
        if origin.len() > 2048
            || !(origin.starts_with("https://") || origin.starts_with("http://"))
        {
            return;
        }
        lock_unpoisoned(&self.badge_tabs)
            .entry(event.connection_id.clone())
            .or_default()
            .insert(tab_id, origin.to_string());
        let (locked, accounts) = badge_accounts(store, origin);
        self.bridge.record_event(
            "badge",
            "origin-active",
            format!(
                "tabId={tab_id} origin={origin} locked={locked} accounts={}",
                accounts.len()
            ),
        );
        let result = self.push_badge_result(&event.connection_id, tab_id, origin, locked, accounts);
        self.bridge.record_event(
            "badge",
            "push",
            format!("tabId={tab_id} ok={}", result.is_ok()),
        );
    }

    fn push_badge(
        &self,
        connection_id: &str,
        tab_id: i64,
        origin: &str,
        locked: bool,
        accounts: Vec<PasswordCaptureAccount>,
    ) {
        let _ = self.push_badge_result(connection_id, tab_id, origin, locked, accounts);
    }

    fn push_badge_result(
        &self,
        connection_id: &str,
        tab_id: i64,
        origin: &str,
        locked: bool,
        accounts: Vec<PasswordCaptureAccount>,
    ) -> Result<Value, String> {
        self.bridge.request_connection(
            connection_id,
            "password.updateBadge",
            serde_json::json!({
                "tabId": tab_id,
                "origin": origin,
                "locked": locked,
                "accounts": accounts,
            }),
            BADGE_REQUEST_TIMEOUT,
        )
    }

    /// Recomputes and pushes the badge for every tracked tab. Called after
    /// vault mutations (entry create/update/delete, lock, unlock).
    pub fn refresh_badges(&self, store: &PasswordStore) {
        let tracked = lock_unpoisoned(&self.badge_tabs)
            .iter()
            .flat_map(|(connection_id, tabs)| {
                tabs.iter()
                    .map(|(tab_id, origin)| (connection_id.clone(), *tab_id, origin.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if tracked.is_empty() {
            return;
        }
        for (connection_id, tab_id, origin) in tracked {
            let (locked, accounts) = badge_accounts(store, &origin);
            self.push_badge(&connection_id, tab_id, &origin, locked, accounts);
        }
    }

    fn handle_capture_candidate(&self, store: &PasswordStore, event: &BrowserSecretEvent) {
        let Some(candidate_id) = event.payload.get("candidateId").and_then(Value::as_str) else {
            return;
        };
        let origin = event
            .payload
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let username = event
            .payload
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let password = event
            .payload
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(tab_id) = event
            .payload
            .get("tabId")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
        else {
            return;
        };
        let Some(frame_id) = event.payload.get("frameId").and_then(Value::as_i64) else {
            return;
        };
        if frame_id < 0 {
            return;
        }
        if frame_id > 0 {
            // A candidate submitted inside an iframe is only acceptable when
            // the frame reports its own origin and that origin is same-site
            // with the top-level one; anything else is dropped silently.
            let accepted = event
                .payload
                .get("frameOrigin")
                .and_then(Value::as_str)
                .is_some_and(|frame_origin| same_site(frame_origin, &origin));
            if !accepted {
                return;
            }
        }
        let Some(document_id) = event
            .payload
            .get("documentId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .map(str::to_string)
        else {
            return;
        };
        let prompt_origin = event
            .payload
            .get("promptOrigin")
            .and_then(Value::as_str)
            .unwrap_or(origin.as_str())
            .to_string();
        if prompt_origin != origin {
            return;
        }
        let allow_insecure_http = origin.starts_with("http://");
        let decision = store.require_any_epoch().and_then(|epoch| {
            store.capture_decision_at(
                PasswordCaptureCandidate {
                    origin: origin.clone(),
                    username: SensitiveText::new(username.clone()),
                    password: SensitiveText::new(password.clone()),
                    allow_insecure_http,
                },
                epoch,
            )
        });
        let (action, entry_id, account_choices, decision_origin, decision_insecure) = match decision
        {
            Ok(decision) => match decision.action {
                PasswordCaptureAction::Create => (
                    "new",
                    decision.entry_id,
                    decision.account_choices,
                    decision.origin,
                    decision.insecure_http,
                ),
                PasswordCaptureAction::Update => (
                    "update",
                    decision.entry_id,
                    decision.account_choices,
                    decision.origin,
                    decision.insecure_http,
                ),
                PasswordCaptureAction::SelectAccount => (
                    "select",
                    None,
                    decision.account_choices,
                    decision.origin,
                    decision.insecure_http,
                ),
                PasswordCaptureAction::UsernameRequired => (
                    "username-required",
                    None,
                    decision.account_choices,
                    decision.origin,
                    decision.insecure_http,
                ),
                PasswordCaptureAction::Disabled | PasswordCaptureAction::NoPrompt => (
                    "same",
                    None,
                    decision.account_choices,
                    decision.origin,
                    decision.insecure_http,
                ),
            },
            Err(error) if error.code == "password_vault_locked" => (
                // A manually locked vault is a definite answer, not a sync
                // failure: tell the popup so it can offer unlock instead of
                // pretending the login is already saved.
                "locked",
                None,
                Vec::new(),
                origin.clone(),
                allow_insecure_http,
            ),
            Err(_) => (
                "same",
                None,
                Vec::new(),
                origin.clone(),
                allow_insecure_http,
            ),
        };
        if !matches!(action, "same" | "username-required" | "locked") {
            lock_unpoisoned(&self.captures).insert(
                candidate_id.to_string(),
                PendingCapture {
                    connection_id: event.connection_id.clone(),
                    entry_id,
                    account_choices: account_choices.clone(),
                    matched_action: action.to_string(),
                    origin: decision_origin.clone(),
                    username: username.clone(),
                    password: password.clone(),
                    allow_insecure_http: decision_insecure,
                    tab_id,
                    frame_id,
                    document_id,
                    prompt_origin,
                    created_at: Instant::now(),
                    save_pending: false,
                },
            );
        }
        let _ = self.bridge.request_connection(
            &event.connection_id,
            "password.captureMatch",
            serde_json::json!({
                "candidateId": candidate_id,
                "action": action,
                "origin": decision_origin,
                "accounts": account_choices,
                "username": if username.is_empty() { Value::Null } else { Value::String(username.clone()) },
            }),
            REQUEST_TIMEOUT,
        );
    }

    fn handle_save_decision(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let store = app.state::<PasswordStore>();
        if let Some((entry_id, action)) = self.save_decision_from_event(&store, event) {
            let _ = app.emit_to(
                "passwords",
                "password_entries_changed",
                serde_json::json!({ "entryId": entry_id, "action": action }),
            );
            self.refresh_badges(&store);
        }
    }

    /// Applies the popup's save decision. On a successful write the entry id
    /// and action come back so the dispatcher can notify the password window
    /// and refresh badges.
    fn save_decision_from_event(
        &self,
        store: &PasswordStore,
        event: &BrowserSecretEvent,
    ) -> Option<(String, String)> {
        let Some(candidate_id) = event.payload.get("candidateId").and_then(Value::as_str) else {
            return None;
        };
        let action = event
            .payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("ignore");

        let mut captures = lock_unpoisoned(&self.captures);
        let Some(candidate) = captures.get_mut(candidate_id) else {
            return None;
        };
        if candidate.connection_id != event.connection_id {
            return None;
        }
        if !capture_decision_matches(candidate, &event.payload) {
            captures.remove(candidate_id);
            return None;
        }
        if action == "ignore" {
            captures.remove(candidate_id);
            return None;
        }

        let selected_entry_id = event
            .payload
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let valid_action = match (candidate.matched_action.as_str(), action) {
            ("new", "new") => candidate.entry_id.is_none() && !candidate.username.is_empty(),
            ("update", "update") => candidate.entry_id.is_some(),
            ("new", "replace") | ("select", "replace") => {
                selected_entry_id.as_ref().is_some_and(|entry_id| {
                    candidate
                        .account_choices
                        .iter()
                        .any(|choice| &choice.entry_id == entry_id)
                })
            }
            _ => false,
        };
        if !valid_action {
            let payload = save_result_payload(
                candidate_id,
                action,
                false,
                None,
                Some((
                    "PASSWORD_PROTOCOL_INVALID",
                    "保存操作与当前账户匹配结果不一致。",
                )),
            );
            drop(captures);
            let _ = self.bridge.request_connection(
                &event.connection_id,
                "password.saveResult",
                payload,
                REQUEST_TIMEOUT,
            );
            return None;
        }
        if candidate.save_pending {
            let payload = save_result_payload(
                candidate_id,
                action,
                false,
                None,
                Some(("PASSWORD_SAVE_BUSY", "该登录信息正在保存，请稍候。")),
            );
            drop(captures);
            let _ = self.bridge.request_connection(
                &event.connection_id,
                "password.saveResult",
                payload,
                REQUEST_TIMEOUT,
            );
            return None;
        }
        candidate.save_pending = true;
        let pending = captures.remove(candidate_id);
        drop(captures);
        let Some(pending) = pending else { return None };
        let epoch = store.require_any_epoch();
        let result: AppResult<PasswordEntrySummary> = match epoch {
            Err(error) => Err(error),
            Ok(epoch) if action == "update" || action == "replace" => {
                let target_id = if action == "replace" {
                    selected_entry_id.as_deref()
                } else {
                    pending.entry_id.as_deref()
                };
                match target_id {
                    Some(target_id) => match store.list_entries_at(epoch) {
                        Ok(entries) => {
                            match entries.into_iter().find(|entry| entry.id == target_id) {
                                Some(entry) if entry.origin == pending.origin => {
                                    // A replace adopts the candidate username
                                    // together with the password; a candidate
                                    // without a username (password-change
                                    // pages) keeps the stored one.
                                    let username = if pending.username.is_empty() {
                                        entry.username.clone()
                                    } else {
                                        pending.username.clone()
                                    };
                                    store.update_entry_at(
                                        PasswordEntryUpdateInput {
                                            id: entry.id,
                                            site_name: entry.site_name,
                                            login_url: entry.login_url,
                                            username: SensitiveText::new(username),
                                            password: Some(SensitiveText::new(
                                                pending.password.clone(),
                                            )),
                                            notes: entry.notes,
                                            template_id: entry.template_id,
                                            allow_insecure_http: entry.allow_insecure_http,
                                        },
                                        epoch,
                                    )
                                }
                                Some(_) => Err(browser_error(
                                    "PASSWORD_ORIGIN_MISMATCH",
                                    "所选账户不属于当前登录 origin。",
                                )),
                                None => Err(browser_error(
                                    "PASSWORD_ENTRY_NOT_FOUND",
                                    "没有找到要更新的站点账户。",
                                )),
                            }
                        }
                        Err(error) => Err(error),
                    },
                    None => Err(browser_error(
                        "PASSWORD_ENTRY_NOT_FOUND",
                        "没有找到要更新的站点账户。",
                    )),
                }
            }
            Ok(epoch) => {
                let site_name = Url::parse(&pending.origin)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| pending.origin.clone());
                store.create_entry_at(
                    PasswordEntryInput {
                        site_name,
                        login_url: format!("{}/", pending.origin.trim_end_matches('/')),
                        username: SensitiveText::new(pending.username.clone()),
                        password: SensitiveText::new(pending.password.clone()),
                        notes: String::new(),
                        template_id: None,
                        allow_insecure_http: pending.allow_insecure_http,
                    },
                    epoch,
                )
            }
        };

        match result {
            Ok(entry) => {
                let receipt = self.bridge.request_connection(
                    &event.connection_id,
                    "password.saveResult",
                    save_result_payload(candidate_id, action, true, Some(&entry.id), None),
                    REQUEST_TIMEOUT,
                );
                // A legacy extension does not know password.saveResult.  The
                // write has already completed, so discard the in-memory secret
                // even when that old extension rejects the receipt command.
                let _ = receipt;
                Some((entry.id, action.to_string()))
            }
            Err(error) => {
                let receipt = self.bridge.request_connection(
                    &event.connection_id,
                    "password.saveResult",
                    save_result_payload(
                        candidate_id,
                        action,
                        false,
                        None,
                        Some((&error.code, &error.message)),
                    ),
                    REQUEST_TIMEOUT,
                );
                if receipt.is_ok() {
                    let mut captures = lock_unpoisoned(&self.captures);
                    if !captures.contains_key(candidate_id) {
                        let mut retry = pending;
                        retry.save_pending = false;
                        retry.created_at = Instant::now();
                        captures.insert(candidate_id.to_string(), retry);
                    }
                }
                // If the connection disappeared or the old extension rejected
                // the command, dropping `pending` clears the secret promptly.
                None
            }
        }
    }

    fn handle_template_recording_ready(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let mut recordings = lock_unpoisoned(&self.recordings);
        let Some(session) = recordings.get_mut(session_id) else {
            return;
        };
        if session.connection_id != event.connection_id {
            return;
        }
        if !bind_recording_event(session, &event.payload) {
            let failed = recordings
                .remove(session_id)
                .expect("recording session disappeared");
            drop(recordings);
            emit_template_recording_status(
                app,
                template_recording_ticket(
                    session_id,
                    &failed,
                    Some("Firefox 返回了不匹配的模板录制页面。".to_string()),
                ),
                TemplateRecordingState::Failed,
            );
            return;
        }
        session.state = TemplateRecordingState::Recording;
        session.expires_at = Instant::now() + TEMPLATE_RECORDING_TTL;
        let ticket = template_recording_ticket(session_id, session, None);
        drop(recordings);
        emit_template_recording_status(app, ticket, TemplateRecordingState::Recording);
    }

    fn handle_template_recording_result(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let mut recordings = lock_unpoisoned(&self.recordings);
        let Some(session) = recordings.get_mut(session_id) else {
            return;
        };
        if session.connection_id != event.connection_id {
            return;
        }
        if !bind_recording_event(session, &event.payload) {
            let failed = recordings
                .remove(session_id)
                .expect("recording session disappeared");
            drop(recordings);
            emit_template_recording_status(
                app,
                template_recording_ticket(
                    session_id,
                    &failed,
                    Some("模板录制结果来自错误的标签页或站点。".to_string()),
                ),
                TemplateRecordingState::Failed,
            );
            return;
        }
        let session = recordings
            .remove(session_id)
            .expect("recording session disappeared");
        drop(recordings);
        if event.payload.get("status").and_then(Value::as_str) == Some("failed") {
            let message = event
                .payload
                .get("error")
                .and_then(Value::as_str)
                .map(|value| value.chars().take(512).collect())
                .unwrap_or_else(|| "Firefox 未能完成模板录制。".to_string());
            emit_template_recording_status(
                app,
                template_recording_ticket(session_id, &session, Some(message)),
                TemplateRecordingState::Failed,
            );
            return;
        }
        let template = event
            .payload
            .get("template")
            .cloned()
            .ok_or_else(|| browser_error("password_template_invalid", "模板录制结果缺少模板定义。"))
            .and_then(|value| {
                serde_json::from_value::<PasswordTemplateDefinition>(value).map_err(|_| {
                    browser_error("password_template_invalid", "模板录制结果格式无效。")
                })
            });
        let result = template.and_then(|template| {
            let store = app.state::<PasswordStore>();
            let epoch = store.require_active_epoch()?;
            store.set_recorded_template_at(&session.entry_id, template, epoch)
        });
        match result {
            Ok(entry) => {
                let _ = app.emit_to(
                    "passwords",
                    "password_entries_changed",
                    serde_json::json!({ "entryId": entry.id, "action": "template" }),
                );
                emit_template_recording_status(
                    app,
                    template_recording_ticket(session_id, &session, None),
                    TemplateRecordingState::Completed,
                );
            }
            Err(error) => emit_template_recording_status(
                app,
                template_recording_ticket(session_id, &session, Some(error.message)),
                TemplateRecordingState::Failed,
            ),
        }
    }

    fn handle_template_recording_cancelled(&self, app: &AppHandle, event: &BrowserSecretEvent) {
        let Some(session_id) = event.payload.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let mut recordings = lock_unpoisoned(&self.recordings);
        let Some(session) = recordings.get_mut(session_id) else {
            return;
        };
        if session.connection_id != event.connection_id {
            return;
        }
        if !bind_recording_event(session, &event.payload) {
            return;
        }
        let session = recordings
            .remove(session_id)
            .expect("recording session disappeared");
        drop(recordings);
        emit_template_recording_status(
            app,
            template_recording_ticket(session_id, &session, None),
            TemplateRecordingState::Cancelled,
        );
    }

    fn prune(&self, app: &AppHandle) {
        let now = Instant::now();
        lock_unpoisoned(&self.fills).retain(|_, session| session.expires_at > now);
        lock_unpoisoned(&self.captures)
            .retain(|_, capture| now.duration_since(capture.created_at) <= CAPTURE_TTL);
        let expired = {
            let mut recordings = lock_unpoisoned(&self.recordings);
            let ids = recordings
                .iter()
                .filter(|(_, session)| session.expires_at <= now)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| recordings.remove(&id).map(|session| (id, session)))
                .collect::<Vec<_>>()
        };
        for (session_id, session) in expired {
            let _ = self.bridge.request_connection(
                &session.connection_id,
                "password.cancelTemplateRecording",
                serde_json::json!({ "sessionId": session_id }),
                Duration::from_secs(2),
            );
            emit_template_recording_status(
                app,
                template_recording_ticket(
                    &session_id,
                    &session,
                    Some("模板录制会话已超时。".to_string()),
                ),
                TemplateRecordingState::Failed,
            );
        }
    }
}

fn password_status_error_is_unsupported(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("unsupported") || normalized.contains("does not support")
}

pub fn start_event_dispatcher(app: AppHandle) {
    if EVENT_DISPATCHER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("petaldesk-password-events".to_string())
        .spawn(move || loop {
            let service = app.state::<PasswordBrowserService>();
            if let Some(event) = service.bridge.receive_event(Duration::from_secs(1)) {
                service.handle_event(&app, event);
            }
            service.prune(&app);
        });
}

pub(crate) fn sync_capture_from_store(app: &AppHandle) {
    let store = app.state::<PasswordStore>();
    app.state::<PasswordBrowserService>()
        .sync_capture_from_store(&store);
}

pub(crate) fn refresh_password_badges(app: &AppHandle) {
    let store = app.state::<PasswordStore>();
    app.state::<PasswordBrowserService>().refresh_badges(&store);
}

/// Counts the vault accounts bound to one origin for the extension badge. Any
/// store error (locked vault, closed session, ...) maps to the locked badge so
/// the popup offers unlock instead of an empty account list.
fn badge_accounts(store: &PasswordStore, origin: &str) -> (bool, Vec<PasswordCaptureAccount>) {
    let entries = store
        .require_any_epoch()
        .and_then(|epoch| store.list_entries_at(epoch));
    match entries {
        Ok(entries) => (
            false,
            entries
                .into_iter()
                .filter(|entry| entry.origin == origin)
                .take(MAX_BADGE_ACCOUNTS)
                .map(|entry| PasswordCaptureAccount {
                    entry_id: entry.id,
                    site_name: entry.site_name,
                    username: entry.username,
                })
                .collect(),
        ),
        Err(_) => (true, Vec::new()),
    }
}

/// Newest native-host session file for Firefox. The host rewrites the
/// heartbeat every couple of seconds and deletes the file on exit, so its
/// freshness describes the stdio layer's health.
#[cfg(windows)]
struct FirefoxBridgeSession {
    extension_version: Option<String>,
    last_seen_unix_ms: u128,
}

#[cfg(windows)]
fn latest_firefox_bridge_session() -> Option<FirefoxBridgeSession> {
    let sessions = crate::browser_secret_bridge::bridge_root()
        .ok()?
        .join("sessions");
    let mut latest: Option<FirefoxBridgeSession> = None;
    for entry in std::fs::read_dir(sessions).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if value.get("browser").and_then(Value::as_str) != Some(BrowserFamily::Firefox.as_str()) {
            continue;
        }
        let Some(last_seen_unix_ms) = value.get("lastSeenUnixMs").and_then(Value::as_u64) else {
            continue;
        };
        let session = FirefoxBridgeSession {
            extension_version: value
                .get("extensionVersion")
                .and_then(Value::as_str)
                .map(str::to_string),
            last_seen_unix_ms: u128::from(last_seen_unix_ms),
        };
        if latest
            .as_ref()
            .is_none_or(|current| session.last_seen_unix_ms >= current.last_seen_unix_ms)
        {
            latest = Some(session);
        }
    }
    latest
}

fn ensure_password_window(window: &WebviewWindow) -> AppResult<()> {
    if window.label() == "passwords" || window.label() == "password-manager" {
        Ok(())
    } else {
        Err(browser_error(
            "password_window_required",
            "此操作只能在密码管理器窗口中执行。",
        ))
    }
}

fn browser_error(code: &str, message: impl Into<String>) -> AppError {
    AppError::new(code, message)
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Multi-level public suffixes shared with the extension's sameSite check.
/// When a host's last two labels are listed here, its registrable domain
/// spans the last three labels instead of two. Keep in sync with the
/// extension side byte for byte.
const MULTI_LEVEL_PUBLIC_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "com.cn", "net.cn", "org.cn", "gov.cn", "edu.cn", "ac.cn",
    "com.au", "net.au", "org.au", "co.nz", "com.hk", "com.tw", "co.jp", "or.jp", "com.sg",
    "com.my", "co.kr", "com.br", "com.mx", "com.tr", "co.in", "firm.in",
];

/// Reports whether two origins belong to the same site: equal origins always
/// match; otherwise both must parse to non-IP hosts whose registrable
/// domains match. Scheme and port are ignored. Mirrors the extension's
/// sameSite check so iframe fills and captures accept exactly the same
/// frames on both sides.
pub(crate) fn same_site(origin_a: &str, origin_b: &str) -> bool {
    if origin_a == origin_b {
        return true;
    }
    match (registrable_domain(origin_a), registrable_domain(origin_b)) {
        (Some(domain_a), Some(domain_b)) => domain_a == domain_b,
        _ => false,
    }
}

fn registrable_domain(origin: &str) -> Option<String> {
    let url = Url::parse(origin).ok()?;
    let host = match url.host() {
        Some(url::Host::Domain(host)) if !host.is_empty() => host,
        _ => return None,
    };
    let labels = host.split('.').collect::<Vec<_>>();
    let last_two = labels[labels.len().saturating_sub(2)..].join(".");
    if MULTI_LEVEL_PUBLIC_SUFFIXES.contains(&last_two.as_str()) {
        Some(labels[labels.len().saturating_sub(3)..].join("."))
    } else {
        Some(last_two)
    }
}

/// Frame policy for fill events: the top-level frame (0) keeps the exact
/// binding; any other frame must report its own origin and it must be
/// same-site with the session's top-level origin. A session still bound to
/// the top level adopts the confirming frame; once bound, only that frame
/// matches.
fn fill_frame_origin_allowed(session: &FillSession, payload: &Value) -> bool {
    match payload.get("frameId").and_then(Value::as_i64) {
        Some(0) => session.frame_id == Some(0),
        Some(frame_id) if frame_id > 0 => {
            matches!(session.frame_id, Some(current) if current == 0 || current == frame_id)
                && payload
                    .get("frameOrigin")
                    .and_then(Value::as_str)
                    .is_some_and(|frame_origin| same_site(frame_origin, &session.origin))
        }
        _ => false,
    }
}

fn fill_target_matches(session: &FillSession, payload: &Value) -> bool {
    payload.get("origin").and_then(Value::as_str) == Some(session.origin.as_str())
        && payload.get("tabId").and_then(Value::as_i64) == session.tab_id
        && payload.get("frameId").and_then(Value::as_i64) == session.frame_id
        && fill_frame_origin_allowed(session, payload)
}

fn bind_fill_tab_ready(session: &mut FillSession, payload: &Value) -> bool {
    let tab_id = payload.get("tabId").and_then(Value::as_i64);
    let frame_id = payload.get("frameId").and_then(Value::as_i64);
    let document_id = payload
        .get("documentId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256);
    if payload.get("origin").and_then(Value::as_str) != Some(session.origin.as_str())
        || tab_id.is_none_or(|value| value < 0)
        || frame_id != Some(0)
        || document_id.is_none()
        || session
            .tab_id
            .is_some_and(|expected| Some(expected) != tab_id)
        || session
            .frame_id
            .is_some_and(|expected| Some(expected) != frame_id)
    {
        return false;
    }
    session.tab_id = tab_id;
    session.frame_id = frame_id;
    session.document_id = document_id.map(str::to_string);
    true
}

fn fill_confirmation_matches(session: &FillSession, payload: &Value) -> bool {
    payload.get("origin").and_then(Value::as_str) == Some(session.origin.as_str())
        && payload.get("tabId").and_then(Value::as_i64) == session.tab_id
        && payload.get("documentId").and_then(Value::as_str) == session.document_id.as_deref()
        && fill_frame_origin_allowed(session, payload)
}

fn capture_decision_matches(capture: &PendingCapture, payload: &Value) -> bool {
    payload.get("origin").and_then(Value::as_str) == Some(capture.origin.as_str())
        && payload.get("promptOrigin").and_then(Value::as_str)
            == Some(capture.prompt_origin.as_str())
        && payload.get("tabId").and_then(Value::as_i64) == Some(capture.tab_id)
        && payload.get("frameId").and_then(Value::as_i64) == Some(capture.frame_id)
        && payload.get("documentId").and_then(Value::as_str) == Some(capture.document_id.as_str())
}

fn save_result_payload(
    candidate_id: &str,
    action: &str,
    success: bool,
    entry_id: Option<&str>,
    error: Option<(&str, &str)>,
) -> Value {
    let mut payload = serde_json::json!({
        "candidateId": candidate_id,
        "action": action,
        "success": success,
    });
    if let Some(entry_id) = entry_id {
        payload["entryId"] = Value::String(entry_id.to_string());
    }
    if let Some((code, message)) = error {
        payload["error"] = serde_json::json!({ "code": code, "message": message });
    }
    payload
}

fn bind_recording_event(session: &mut TemplateRecordingSession, payload: &Value) -> bool {
    if payload.get("origin").and_then(Value::as_str) != Some(session.origin.as_str()) {
        return false;
    }
    let tab_id = payload.get("tabId").and_then(Value::as_i64);
    if tab_id.is_none_or(|value| value < 0)
        || session
            .tab_id
            .is_some_and(|expected| Some(expected) != tab_id)
    {
        return false;
    }
    let frame_id = payload.get("frameId").and_then(Value::as_i64);
    if frame_id.is_some_and(|value| value != 0)
        || session
            .frame_id
            .is_some_and(|expected| frame_id.is_some() && Some(expected) != frame_id)
    {
        return false;
    }
    let document_id = match payload.get("documentId") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 256 => Some(value),
        _ => return false,
    };
    if session.document_id.is_some()
        && session.document_id.as_deref() != document_id.map(String::as_str)
    {
        return false;
    }
    session.tab_id = tab_id;
    if frame_id.is_some() {
        session.frame_id = frame_id;
    }
    if let Some(document_id) = document_id {
        session.document_id = Some(document_id.to_string());
    }
    session.frame_id.is_none_or(|value| value == 0)
}

fn template_recording_ticket(
    session_id: &str,
    session: &TemplateRecordingSession,
    message: Option<String>,
) -> PasswordTemplateRecordingTicket {
    let remaining = session.expires_at.saturating_duration_since(Instant::now());
    PasswordTemplateRecordingTicket {
        session_id: session_id.to_string(),
        entry_id: session.entry_id.clone(),
        origin: session.origin.clone(),
        state: session.state,
        expires_at: unix_time_ms().saturating_add(remaining.as_millis()),
        message,
    }
}

fn emit_template_recording_status(
    app: &AppHandle,
    mut ticket: PasswordTemplateRecordingTicket,
    state: TemplateRecordingState,
) {
    ticket.state = state;
    let _ = app.emit_to("passwords", "password-template-recording-status", ticket);
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
pub async fn get_password_browser_status(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<PasswordBrowserStatus> {
    ensure_password_window(&window)?;
    crate::commands::run_background("读取 Firefox 扩展状态", move || {
        Ok(app.state::<PasswordBrowserService>().status())
    })
    .await
}

#[tauri::command]
pub async fn start_password_fill(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<PasswordFillTicket> {
    ensure_password_window(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordBrowserService>()
            .start_fill(&app, &entry_id)
    })
    .await
    .map_err(|error| {
        browser_error(
            "password_task_error",
            format!("打开并填充任务异常结束: {error}"),
        )
    })?
}

#[tauri::command]
pub async fn cancel_password_fill(
    app: AppHandle,
    window: WebviewWindow,
    session_id: String,
) -> AppResult<()> {
    ensure_password_window(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordBrowserService>()
            .cancel_fill(&session_id)
    })
    .await
    .map_err(|error| {
        browser_error(
            "password_task_error",
            format!("取消填充任务异常结束: {error}"),
        )
    })?
}

#[tauri::command]
pub async fn start_password_template_recording(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<PasswordTemplateRecordingTicket> {
    ensure_password_window(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordBrowserService>()
            .start_template_recording(&app, &entry_id)
    })
    .await
    .map_err(|error| {
        browser_error(
            "password_task_error",
            format!("开始模板录制任务异常结束: {error}"),
        )
    })?
}

#[tauri::command]
pub async fn cancel_password_template_recording(
    app: AppHandle,
    window: WebviewWindow,
    session_id: String,
) -> AppResult<()> {
    ensure_password_window(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordBrowserService>()
            .cancel_template_recording(&session_id)
    })
    .await
    .map_err(|error| {
        browser_error(
            "password_task_error",
            format!("取消模板录制任务异常结束: {error}"),
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> PasswordBrowserService {
        PasswordBrowserService {
            bridge: BrowserSecretBridge::disabled(),
            fills: Mutex::new(HashMap::new()),
            captures: Mutex::new(HashMap::new()),
            recordings: Mutex::new(HashMap::new()),
            synced_capture: Mutex::new(HashMap::new()),
            badge_tabs: Mutex::new(HashMap::new()),
        }
    }

    fn fill_session() -> FillSession {
        FillSession {
            connection_id: "connection-a".to_string(),
            entry_id: "entry-a".to_string(),
            origin: "https://accounts.google.com".to_string(),
            offer_id: Some("offer-a".to_string()),
            tab_id: None,
            frame_id: None,
            document_id: None,
            expires_at: Instant::now() + FILL_TTL,
        }
    }

    #[test]
    fn status_errors_only_mark_explicitly_unsupported_extensions() {
        assert!(password_status_error_is_unsupported(
            "Unsupported password command: password.getStatus"
        ));
        assert!(password_status_error_is_unsupported(
            "firefox browser extension does not support command password.getStatus"
        ));
        assert!(!password_status_error_is_unsupported(
            "password browser request timed out after 2000 ms"
        ));
        assert!(!password_status_error_is_unsupported(
            "password browser connection is busy"
        ));
    }

    #[test]
    fn fill_binding_allows_a_new_document_only_in_the_same_top_level_tab() {
        let mut session = fill_session();
        assert!(bind_fill_tab_ready(
            &mut session,
            &serde_json::json!({
                "origin": "https://accounts.google.com",
                "tabId": 42,
                "frameId": 0,
                "documentId": "document-1",
            }),
        ));
        assert!(fill_confirmation_matches(
            &session,
            &serde_json::json!({
                "origin": session.origin,
                "tabId": 42,
                "frameId": 0,
                "documentId": "document-1",
            }),
        ));
        assert!(bind_fill_tab_ready(
            &mut session,
            &serde_json::json!({
                "origin": "https://accounts.google.com",
                "tabId": 42,
                "frameId": 0,
                "documentId": "document-2",
            }),
        ));
        assert_eq!(session.document_id.as_deref(), Some("document-2"));
        for rejected in [
            serde_json::json!({
                "origin": session.origin,
                "tabId": 43,
                "frameId": 0,
                "documentId": "document-3",
            }),
            serde_json::json!({
                "origin": session.origin,
                "tabId": 42,
                "frameId": 1,
                "documentId": "document-3",
            }),
            serde_json::json!({
                "origin": "https://evil.example",
                "tabId": 42,
                "frameId": 0,
                "documentId": "document-3",
            }),
        ] {
            assert!(!bind_fill_tab_ready(&mut session, &rejected));
        }
    }

    #[test]
    fn two_step_fill_result_keeps_the_bound_session_and_cross_connection_is_ignored() {
        let service = service();
        let mut session = fill_session();
        assert!(bind_fill_tab_ready(
            &mut session,
            &serde_json::json!({
                "origin": "https://accounts.google.com",
                "tabId": 42,
                "frameId": 0,
                "documentId": "document-1",
            }),
        ));
        lock_unpoisoned(&service.fills).insert("session-a".to_string(), session);
        let payload = serde_json::json!({
            "sessionId": "session-a",
            "origin": "https://accounts.google.com",
            "tabId": 42,
            "frameId": 0,
            "needsNextStep": true,
        });
        service.handle_fill_result(&BrowserSecretEvent {
            connection_id: "connection-b".to_string(),
            browser: BrowserFamily::Firefox,
            event: "fillResult".to_string(),
            payload: payload.clone(),
        });
        assert!(lock_unpoisoned(&service.fills).contains_key("session-a"));
        service.handle_fill_result(&BrowserSecretEvent {
            connection_id: "connection-a".to_string(),
            browser: BrowserFamily::Firefox,
            event: "fillResult".to_string(),
            payload,
        });
        let fills = lock_unpoisoned(&service.fills);
        let session = fills.get("session-a").unwrap();
        assert!(session.offer_id.is_none());
        drop(fills);

        service.handle_fill_result(&BrowserSecretEvent {
            connection_id: "connection-a".to_string(),
            browser: BrowserFamily::Firefox,
            event: "fillResult".to_string(),
            payload: serde_json::json!({
                "sessionId": "session-a",
                "origin": "https://accounts.google.com",
                "tabId": 99,
                "frameId": 0,
                "needsNextStep": true,
            }),
        });
        assert!(!lock_unpoisoned(&service.fills).contains_key("session-a"));
    }

    #[test]
    fn template_recording_binding_rejects_cross_tab_frame_and_origin_and_has_a_ttl() {
        let mut session = TemplateRecordingSession {
            connection_id: "connection-a".to_string(),
            entry_id: "entry-a".to_string(),
            origin: "https://example.com".to_string(),
            tab_id: Some(7),
            frame_id: Some(0),
            document_id: None,
            state: TemplateRecordingState::Recording,
            expires_at: Instant::now() + TEMPLATE_RECORDING_TTL,
        };
        assert!(bind_recording_event(
            &mut session,
            &serde_json::json!({
                "origin": "https://example.com",
                "tabId": 7,
                "frameId": 0,
            }),
        ));
        assert!(!bind_recording_event(
            &mut session,
            &serde_json::json!({
                "origin": "https://example.com",
                "tabId": 8,
                "frameId": 0,
            }),
        ));
        assert!(!bind_recording_event(
            &mut session,
            &serde_json::json!({
                "origin": "https://example.com",
                "tabId": 7,
                "frameId": 1,
            }),
        ));
        assert!(!bind_recording_event(
            &mut session,
            &serde_json::json!({
                "origin": "https://other.example.com",
                "tabId": 7,
                "frameId": 0,
            }),
        ));
        session.expires_at = Instant::now() - Duration::from_millis(1);
        assert!(session.expires_at <= Instant::now());
    }

    #[test]
    fn capture_save_decision_is_bound_to_connection_page_and_top_frame() {
        let capture = PendingCapture {
            connection_id: "connection-a".to_string(),
            entry_id: None,
            account_choices: Vec::new(),
            matched_action: "new".to_string(),
            origin: "https://example.com".to_string(),
            username: "alice".to_string(),
            password: "secret".to_string(),
            allow_insecure_http: false,
            tab_id: 7,
            frame_id: 0,
            document_id: "document-a".to_string(),
            prompt_origin: "https://example.com".to_string(),
            created_at: Instant::now(),
            save_pending: false,
        };
        let matching = serde_json::json!({
            "origin": "https://example.com",
            "promptOrigin": "https://example.com",
            "tabId": 7,
            "frameId": 0,
            "documentId": "document-a",
        });
        assert!(capture_decision_matches(&capture, &matching));
        for (field, value) in [
            ("origin", serde_json::json!("https://other.example.com")),
            (
                "promptOrigin",
                serde_json::json!("https://other.example.com"),
            ),
            ("tabId", serde_json::json!(8)),
            ("frameId", serde_json::json!(1)),
            ("documentId", serde_json::json!("document-b")),
        ] {
            let mut rejected = matching.clone();
            rejected[field] = value;
            assert!(!capture_decision_matches(&capture, &rejected));
        }
    }

    #[test]
    fn page_closed_drops_only_the_matching_native_capture() {
        let service = service();
        let capture = |connection_id: &str, document_id: &str| PendingCapture {
            connection_id: connection_id.to_string(),
            entry_id: None,
            account_choices: Vec::new(),
            matched_action: "new".to_string(),
            origin: "https://example.com".to_string(),
            username: "alice".to_string(),
            password: "secret".to_string(),
            allow_insecure_http: false,
            tab_id: 7,
            frame_id: 0,
            document_id: document_id.to_string(),
            prompt_origin: "https://example.com".to_string(),
            created_at: Instant::now(),
            save_pending: false,
        };
        lock_unpoisoned(&service.captures).insert(
            "matching".to_string(),
            capture("connection-a", "document-a"),
        );
        lock_unpoisoned(&service.captures).insert(
            "other-document".to_string(),
            capture("connection-a", "document-b"),
        );
        lock_unpoisoned(&service.captures).insert(
            "other-connection".to_string(),
            capture("connection-b", "document-a"),
        );

        service.handle_page_closed(&BrowserSecretEvent {
            connection_id: "connection-a".to_string(),
            browser: BrowserFamily::Firefox,
            event: "pageClosed".to_string(),
            payload: serde_json::json!({ "tabId": 7, "documentId": "document-a" }),
        });

        let captures = lock_unpoisoned(&service.captures);
        assert!(!captures.contains_key("matching"));
        assert!(captures.contains_key("other-document"));
        assert!(captures.contains_key("other-connection"));
        drop(captures);

        service.handle_page_closed(&BrowserSecretEvent {
            connection_id: "connection-a".to_string(),
            browser: BrowserFamily::Firefox,
            event: "pageClosed".to_string(),
            payload: serde_json::json!({ "tabId": 7, "documentId": null }),
        });
        let captures = lock_unpoisoned(&service.captures);
        assert!(!captures.contains_key("other-document"));
        assert!(captures.contains_key("other-connection"));
    }

    const TEST_RECOVERY_PASSWORD: &str = "petaldesk-browser-test-recovery";

    /// Answers bridge requests from a background thread while recording them,
    /// so service methods that block on `request_connection` can be tested
    /// without a real extension.
    struct RecordedBridge {
        requests: std::sync::Arc<Mutex<Vec<Value>>>,
        responses: std::sync::Arc<Mutex<HashMap<String, Value>>>,
        stop: std::sync::Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl RecordedBridge {
        fn spawn(bridge: BrowserSecretBridge, receiver: std::sync::mpsc::Receiver<Value>) -> Self {
            let requests = std::sync::Arc::new(Mutex::new(Vec::new()));
            let responses = std::sync::Arc::new(Mutex::new(HashMap::new()));
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let thread = {
                let requests = requests.clone();
                let responses = responses.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        match receiver.recv_timeout(Duration::from_millis(20)) {
                            Ok(request) => {
                                let command = request
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                lock_unpoisoned(&requests).push(request.clone());
                                let response = lock_unpoisoned(&responses)
                                    .get(&command)
                                    .cloned()
                                    .unwrap_or_else(|| Value::Object(Default::default()));
                                bridge.test_answer_request(&request, response);
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
            };
            Self {
                requests,
                responses,
                stop,
                thread: Some(thread),
            }
        }

        fn set_response(&self, command: &str, response: Value) {
            lock_unpoisoned(&self.responses).insert(command.to_string(), response);
        }

        fn requests_for(&self, command: &str) -> Vec<Value> {
            lock_unpoisoned(&self.requests)
                .iter()
                .filter(|request| request.get("command").and_then(Value::as_str) == Some(command))
                .cloned()
                .collect()
        }
    }

    impl Drop for RecordedBridge {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn connected_service(connection_id: &str) -> (PasswordBrowserService, RecordedBridge) {
        let (bridge, receiver) = crate::browser_secret_bridge::test_bridge(connection_id);
        let recorded = RecordedBridge::spawn(bridge.clone(), receiver);
        (
            PasswordBrowserService {
                bridge,
                fills: Mutex::new(HashMap::new()),
                captures: Mutex::new(HashMap::new()),
                recordings: Mutex::new(HashMap::new()),
                synced_capture: Mutex::new(HashMap::new()),
                badge_tabs: Mutex::new(HashMap::new()),
            },
            recorded,
        )
    }

    fn test_store() -> (tempfile::TempDir, PasswordStore) {
        let root = tempfile::tempdir().unwrap();
        let store = PasswordStore::load(root.path()).unwrap();
        store.activate();
        let epoch = store.require_active_epoch().unwrap();
        store
            .configure_recovery_password_at(TEST_RECOVERY_PASSWORD, None, epoch)
            .unwrap();
        (root, store)
    }

    fn create_entry(
        store: &PasswordStore,
        login_url: &str,
        username: &str,
        password: &str,
    ) -> PasswordEntrySummary {
        let epoch = store.require_active_epoch().unwrap();
        store
            .create_entry_at(
                PasswordEntryInput {
                    site_name: "Example".to_string(),
                    login_url: login_url.to_string(),
                    username: SensitiveText::new(username.to_string()),
                    password: SensitiveText::new(password.to_string()),
                    notes: String::new(),
                    template_id: None,
                    allow_insecure_http: false,
                },
                epoch,
            )
            .unwrap()
    }

    fn capture_account(entry: &PasswordEntrySummary) -> PasswordCaptureAccount {
        PasswordCaptureAccount {
            entry_id: entry.id.clone(),
            site_name: entry.site_name.clone(),
            username: entry.username.clone(),
        }
    }

    fn secret_event(connection_id: &str, event: &str, payload: Value) -> BrowserSecretEvent {
        BrowserSecretEvent {
            connection_id: connection_id.to_string(),
            browser: BrowserFamily::Firefox,
            event: event.to_string(),
            payload,
        }
    }

    fn pending_capture(account_choices: Vec<PasswordCaptureAccount>) -> PendingCapture {
        PendingCapture {
            connection_id: "connection-a".to_string(),
            entry_id: None,
            account_choices,
            matched_action: "new".to_string(),
            origin: "https://example.com".to_string(),
            username: "carol".to_string(),
            password: "three".to_string(),
            allow_insecure_http: false,
            tab_id: 7,
            frame_id: 0,
            document_id: "document-1".to_string(),
            prompt_origin: "https://example.com".to_string(),
            created_at: Instant::now(),
            save_pending: false,
        }
    }

    fn save_decision_event(candidate_id: &str, entry_id: Option<&str>) -> BrowserSecretEvent {
        secret_event(
            "connection-a",
            "saveDecision",
            serde_json::json!({
                "candidateId": candidate_id,
                "action": "replace",
                "entryId": entry_id,
                "origin": "https://example.com",
                "promptOrigin": "https://example.com",
                "tabId": 7,
                "frameId": 0,
                "documentId": "document-1",
            }),
        )
    }

    #[test]
    fn origin_active_pushes_badge_with_matching_accounts() {
        let (_root, store) = test_store();
        let first = create_entry(&store, "https://example.com/login", "alice", "one");
        let second = create_entry(&store, "https://example.com/signin", "bob", "two");
        create_entry(&store,  "https://other.example.com/login", "carol", "three");
        let (service, recorded) = connected_service("connection-a");

        service.handle_origin_active(
            &store,
            &secret_event(
                "connection-a",
                "originActive",
                serde_json::json!({ "tabId": 7, "origin": "https://example.com" }),
            ),
        );

        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 1);
        let payload = &badges[0]["payload"];
        assert_eq!(payload["tabId"], 7);
        assert_eq!(payload["origin"], "https://example.com");
        assert_eq!(payload["locked"], false);
        let accounts = payload["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        let entry_ids = accounts
            .iter()
            .map(|account| account["entryId"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(entry_ids.contains(&first.id.as_str()));
        assert!(entry_ids.contains(&second.id.as_str()));
    }

    #[test]
    fn origin_active_pushes_empty_and_locked_badges() {
        let (_root, store) = test_store();
        create_entry(&store,  "https://example.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");

        service.handle_origin_active(
            &store,
            &secret_event(
                "connection-a",
                "originActive",
                serde_json::json!({ "tabId": 8, "origin": "https://other.example.com" }),
            ),
        );
        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0]["payload"]["locked"], false);
        assert_eq!(
            badges[0]["payload"]["accounts"].as_array().unwrap().len(),
            0
        );

        store.lock_current_session().unwrap();
        service.handle_origin_active(
            &store,
            &secret_event(
                "connection-a",
                "originActive",
                serde_json::json!({ "tabId": 7, "origin": "https://example.com" }),
            ),
        );
        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 2);
        assert_eq!(badges[1]["payload"]["locked"], true);
        assert_eq!(
            badges[1]["payload"]["accounts"].as_array().unwrap().len(),
            0
        );

        // An empty origin only drops the tracked tab without a broadcast.
        service.handle_origin_active(
            &store,
            &secret_event(
                "connection-a",
                "originActive",
                serde_json::json!({ "tabId": 7, "origin": "" }),
            ),
        );
        assert_eq!(recorded.requests_for("password.updateBadge").len(), 2);
        assert!(!lock_unpoisoned(&service.badge_tabs)["connection-a"].contains_key(&7));
    }

    #[test]
    fn refresh_badges_pushes_updated_counts_after_entry_changes() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");
        service.handle_origin_active(
            &store,
            &secret_event(
                "connection-a",
                "originActive",
                serde_json::json!({ "tabId": 7, "origin": "https://example.com" }),
            ),
        );
        assert_eq!(recorded.requests_for("password.updateBadge").len(), 1);

        create_entry(&store,  "https://example.com/signin", "bob", "two");
        service.refresh_badges(&store);
        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 2);
        assert_eq!(
            badges[1]["payload"]["accounts"].as_array().unwrap().len(),
            2
        );

        let epoch = store.require_active_epoch().unwrap();
        store.delete_entry_at(&entry.id, epoch).unwrap();
        service.refresh_badges(&store);
        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 3);
        assert_eq!(
            badges[2]["payload"]["accounts"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn fill_request_binds_direct_offer_and_fill_confirm_provides_credentials() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");

        service.handle_fill_request(
            &store,
            &secret_event(
                "connection-a",
                "fillRequest",
                serde_json::json!({
                    "entryId": entry.id,
                    "tabId": 7,
                    "origin": "https://example.com",
                    "documentId": "document-9",
                }),
            ),
        );

        let offers = recorded.requests_for("password.offerFillDirect");
        assert_eq!(offers.len(), 1);
        let payload = &offers[0]["payload"];
        assert_eq!(payload["entryId"], entry.id);
        assert_eq!(payload["tabId"], 7);
        assert_eq!(payload["frameId"], 0);
        assert_eq!(payload["documentId"], "document-9");
        assert_eq!(payload["origin"], "https://example.com");
        assert_eq!(payload["username"], "alice");
        // The offer never carries the password; that waits for fillConfirm.
        assert!(payload.get("password").is_none());
        let session_id = payload["sessionId"].as_str().unwrap().to_string();
        let offer_id = payload["offerId"].as_str().unwrap().to_string();
        {
            let fills = lock_unpoisoned(&service.fills);
            let session = fills.get(&session_id).unwrap();
            assert_eq!(session.entry_id, entry.id);
            assert_eq!(session.tab_id, Some(7));
            assert_eq!(session.frame_id, Some(0));
            assert_eq!(session.document_id.as_deref(), Some("document-9"));
            assert_eq!(session.offer_id.as_deref(), Some(offer_id.as_str()));
        }

        service.handle_fill_confirm(
            &store,
            &secret_event(
                "connection-a",
                "fillConfirm",
                serde_json::json!({
                    "sessionId": session_id,
                    "offerId": offer_id,
                    "origin": "https://example.com",
                    "tabId": 7,
                    "frameId": 0,
                    "documentId": "document-9",
                }),
            ),
        );
        let credentials = recorded.requests_for("password.provideCredentials");
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0]["payload"]["username"], "alice");
        assert_eq!(credentials[0]["payload"]["password"], "one");
        assert_eq!(credentials[0]["payload"]["offerId"], offer_id);
        assert!(!lock_unpoisoned(&service.fills).contains_key(&session_id));
    }

    #[test]
    fn fill_request_rejects_origin_mismatch_and_reports_locked_vault() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");

        service.handle_fill_request(
            &store,
            &secret_event(
                "connection-a",
                "fillRequest",
                serde_json::json!({
                    "entryId": entry.id,
                    "tabId": 7,
                    "origin": "https://evil.example",
                    "documentId": "document-9",
                }),
            ),
        );
        assert!(recorded.requests_for("password.offerFillDirect").is_empty());
        assert!(lock_unpoisoned(&service.fills).is_empty());

        store.lock_current_session().unwrap();
        service.handle_fill_request(
            &store,
            &secret_event(
                "connection-a",
                "fillRequest",
                serde_json::json!({
                    "entryId": entry.id,
                    "tabId": 7,
                    "origin": "https://example.com",
                    "documentId": "document-9",
                }),
            ),
        );
        assert!(recorded.requests_for("password.offerFillDirect").is_empty());
        assert!(lock_unpoisoned(&service.fills).is_empty());
        let badges = recorded.requests_for("password.updateBadge");
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0]["payload"]["locked"], true);
        assert_eq!(badges[0]["payload"]["tabId"], 7);
    }

    #[test]
    fn same_site_matches_registrable_domains_and_rejects_ips() {
        for (origin_a, origin_b) in [
            // Exact equality always matches, even for IP origins.
            ("https://example.com", "https://example.com"),
            ("https://127.0.0.1:8080", "https://127.0.0.1:8080"),
            // The 163 mail pair: top-level page and login iframe.
            ("https://mail.163.com", "https://dl.reg.163.com"),
            ("https://a.example.com", "https://b.example.com"),
            // Scheme and port are ignored; only the registrable domain counts.
            ("http://example.com:8080", "https://example.com"),
            // Multi-level public suffixes extend the registrable domain.
            ("https://www.example.co.uk", "https://login.example.co.uk"),
            ("https://example.com.cn", "https://mail.example.com.cn"),
        ] {
            assert!(same_site(origin_a, origin_b), "{origin_a} vs {origin_b}");
            assert!(same_site(origin_b, origin_a), "{origin_b} vs {origin_a}");
        }
        for (origin_a, origin_b) in [
            // Different registrable domains never match.
            ("https://example.com", "https://other.com"),
            ("https://163.com", "https://126.com"),
            // co.uk is a public suffix: two co.uk domains are not same-site.
            ("https://example.co.uk", "https://other.co.uk"),
            ("https://example.com", "https://example.co.uk"),
            // IP literals and unparseable origins only match exactly.
            ("https://127.0.0.1:8080", "http://127.0.0.1"),
            ("https://10.0.0.1", "https://10.0.0.2"),
            ("not a url", "https://example.com"),
            ("https://example.com", ""),
        ] {
            assert!(!same_site(origin_a, origin_b), "{origin_a} vs {origin_b}");
            assert!(!same_site(origin_b, origin_a), "{origin_b} vs {origin_a}");
        }
    }

    #[test]
    fn fill_confirm_accepts_same_site_iframe_and_binds_the_session_frame() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://mail.163.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");
        recorded.set_response(
            "password.provideCredentials",
            serde_json::json!({ "needsNextStep": true }),
        );

        service.handle_fill_request(
            &store,
            &secret_event(
                "connection-a",
                "fillRequest",
                serde_json::json!({
                    "entryId": entry.id,
                    "tabId": 7,
                    "origin": "https://mail.163.com",
                    "documentId": "document-9",
                }),
            ),
        );
        let offers = recorded.requests_for("password.offerFillDirect");
        assert_eq!(offers.len(), 1);
        let session_id = offers[0]["payload"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let offer_id = offers[0]["payload"]["offerId"]
            .as_str()
            .unwrap()
            .to_string();

        // The login form lives in a same-site iframe; the confirm reports the
        // real frame together with the frame's own origin.
        service.handle_fill_confirm(
            &store,
            &secret_event(
                "connection-a",
                "fillConfirm",
                serde_json::json!({
                    "sessionId": session_id,
                    "offerId": offer_id,
                    "origin": "https://mail.163.com",
                    "tabId": 7,
                    "frameId": 9,
                    "frameOrigin": "https://dl.reg.163.com",
                    "documentId": "document-9",
                }),
            ),
        );
        let credentials = recorded.requests_for("password.provideCredentials");
        assert_eq!(credentials.len(), 1);
        let payload = &credentials[0]["payload"];
        assert_eq!(payload["tabId"], 7);
        assert_eq!(payload["frameId"], 9);
        assert_eq!(payload["documentId"], "document-9");
        assert_eq!(payload["username"], "alice");
        assert_eq!(payload["password"], "one");
        {
            let fills = lock_unpoisoned(&service.fills);
            let session = fills.get(&session_id).unwrap();
            assert_eq!(session.frame_id, Some(9));
        }

        // Subsequent results bind to the confirmed frame; the top-level frame
        // no longer matches this session.
        service.handle_fill_result(&secret_event(
            "connection-a",
            "fillResult",
            serde_json::json!({
                "sessionId": session_id,
                "origin": "https://mail.163.com",
                "tabId": 7,
                "frameId": 9,
                "frameOrigin": "https://dl.reg.163.com",
                "needsNextStep": true,
            }),
        ));
        assert!(lock_unpoisoned(&service.fills).contains_key(&session_id));
        service.handle_fill_result(&secret_event(
            "connection-a",
            "fillResult",
            serde_json::json!({
                "sessionId": session_id,
                "origin": "https://mail.163.com",
                "tabId": 7,
                "frameId": 0,
                "needsNextStep": true,
            }),
        ));
        assert!(!lock_unpoisoned(&service.fills).contains_key(&session_id));
    }

    #[test]
    fn fill_confirm_rejects_cross_site_or_missing_frame_origin() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://mail.163.com/login", "alice", "one");
        let (service, recorded) = connected_service("connection-a");

        for confirm in [
            // A cross-site frame must never receive credentials.
            serde_json::json!({
                "origin": "https://mail.163.com",
                "tabId": 7,
                "frameId": 9,
                "frameOrigin": "https://evil.example",
                "documentId": "document-9",
            }),
            // A non-zero frame without its own origin is rejected too.
            serde_json::json!({
                "origin": "https://mail.163.com",
                "tabId": 7,
                "frameId": 9,
                "documentId": "document-9",
            }),
        ] {
            service.handle_fill_request(
                &store,
                &secret_event(
                    "connection-a",
                    "fillRequest",
                    serde_json::json!({
                        "entryId": entry.id,
                        "tabId": 7,
                        "origin": "https://mail.163.com",
                        "documentId": "document-9",
                    }),
                ),
            );
            let offers = recorded.requests_for("password.offerFillDirect");
            let payload = &offers.last().unwrap()["payload"];
            let mut confirm = confirm.clone();
            confirm["sessionId"] = payload["sessionId"].clone();
            confirm["offerId"] = payload["offerId"].clone();
            let session_id = payload["sessionId"].as_str().unwrap().to_string();

            service.handle_fill_confirm(
                &store,
                &secret_event("connection-a", "fillConfirm", confirm),
            );
            assert!(!lock_unpoisoned(&service.fills).contains_key(&session_id));
        }
        assert!(recorded.requests_for("password.provideCredentials").is_empty());
    }

    #[test]
    fn capture_candidate_accepts_same_site_iframe_and_binds_the_frame() {
        let (_root, store) = test_store();
        let (service, recorded) = connected_service("connection-a");

        service.handle_capture_candidate(
            &store,
            &secret_event(
                "connection-a",
                "captureCandidate",
                serde_json::json!({
                    "candidateId": "candidate-frame",
                    "origin": "https://mail.163.com",
                    "frameOrigin": "https://dl.reg.163.com",
                    "username": "alice",
                    "password": "one",
                    "tabId": 7,
                    "frameId": 9,
                    "documentId": "document-1",
                }),
            ),
        );

        let matches = recorded.requests_for("password.captureMatch");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["payload"]["action"], "new");
        {
            let captures = lock_unpoisoned(&service.captures);
            let capture = captures.get("candidate-frame").unwrap();
            assert_eq!(capture.frame_id, 9);
            // The save decision must come back from the real submit frame.
            let wrong_frame = serde_json::json!({
                "origin": "https://mail.163.com",
                "promptOrigin": "https://mail.163.com",
                "tabId": 7,
                "frameId": 0,
                "documentId": "document-1",
            });
            assert!(!capture_decision_matches(capture, &wrong_frame));
        }

        let saved = service.save_decision_from_event(
            &store,
            &secret_event(
                "connection-a",
                "saveDecision",
                serde_json::json!({
                    "candidateId": "candidate-frame",
                    "action": "new",
                    "origin": "https://mail.163.com",
                    "promptOrigin": "https://mail.163.com",
                    "tabId": 7,
                    "frameId": 9,
                    "documentId": "document-1",
                }),
            ),
        );
        let Some((entry_id, action)) = saved else {
            panic!("same-site iframe save decision must succeed");
        };
        assert_eq!(action, "new");
        let epoch = store.require_active_epoch().unwrap();
        let created = store.browser_fill_data_at(&entry_id, epoch).unwrap();
        assert_eq!(created.origin, "https://mail.163.com");
        assert_eq!(created.username, "alice");
        let results = recorded.requests_for("password.saveResult");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["payload"]["success"], true);
    }

    #[test]
    fn capture_candidate_drops_cross_site_iframe_submissions() {
        let (_root, store) = test_store();
        let (service, recorded) = connected_service("connection-a");

        for payload in [
            serde_json::json!({
                "candidateId": "candidate-cross-site",
                "origin": "https://mail.163.com",
                "frameOrigin": "https://evil.example",
                "username": "alice",
                "password": "one",
                "tabId": 7,
                "frameId": 9,
                "documentId": "document-1",
            }),
            serde_json::json!({
                "candidateId": "candidate-no-frame-origin",
                "origin": "https://mail.163.com",
                "username": "alice",
                "password": "one",
                "tabId": 7,
                "frameId": 9,
                "documentId": "document-1",
            }),
            serde_json::json!({
                "candidateId": "candidate-negative-frame",
                "origin": "https://mail.163.com",
                "frameOrigin": "https://dl.reg.163.com",
                "username": "alice",
                "password": "one",
                "tabId": 7,
                "frameId": -1,
                "documentId": "document-1",
            }),
        ] {
            service.handle_capture_candidate(
                &store,
                &secret_event("connection-a", "captureCandidate", payload),
            );
        }
        assert!(recorded.requests_for("password.captureMatch").is_empty());
        assert!(lock_unpoisoned(&service.captures).is_empty());
    }

    #[test]
    fn save_decision_new_replace_adopts_candidate_credentials() {
        let (_root, store) = test_store();
        let alice = create_entry(&store, "https://example.com/login", "alice", "one");
        let bob = create_entry(&store, "https://example.com/signin", "bob", "two");
        let (service, recorded) = connected_service("connection-a");
        lock_unpoisoned(&service.captures).insert(
            "candidate-1".to_string(),
            pending_capture(vec![capture_account(&alice), capture_account(&bob)]),
        );

        let saved = service.save_decision_from_event(
            &store,
            &save_decision_event("candidate-1", Some(&bob.id)),
        );
        assert_eq!(saved, Some((bob.id.clone(), "replace".to_string())));

        let results = recorded.requests_for("password.saveResult");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["payload"]["success"], true);
        assert_eq!(results[0]["payload"]["entryId"], bob.id);
        let epoch = store.require_active_epoch().unwrap();
        let updated = store.browser_fill_data_at(&bob.id, epoch).unwrap();
        assert_eq!(updated.username, "carol");
        assert_eq!(updated.password, "three");
        let entries = store.list_entries_at(epoch).unwrap();
        let untouched = entries
            .iter()
            .find(|entry| entry.id == alice.id)
            .unwrap();
        assert_eq!(untouched.username, "alice");
    }

    #[test]
    fn save_decision_new_replace_requires_a_listed_account() {
        let (_root, store) = test_store();
        let alice = create_entry(&store, "https://example.com/login", "alice", "one");
        let bob = create_entry(&store, "https://example.com/signin", "bob", "two");
        let (service, recorded) = connected_service("connection-a");
        lock_unpoisoned(&service.captures).insert(
            "candidate-2".to_string(),
            pending_capture(vec![capture_account(&alice)]),
        );

        let saved = service.save_decision_from_event(
            &store,
            &save_decision_event("candidate-2", Some(&bob.id)),
        );
        assert_eq!(saved, None);

        let results = recorded.requests_for("password.saveResult");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["payload"]["success"], false);
        assert_eq!(
            results[0]["payload"]["error"]["code"],
            "PASSWORD_PROTOCOL_INVALID"
        );
        let epoch = store.require_active_epoch().unwrap();
        let unchanged = store.browser_fill_data_at(&bob.id, epoch).unwrap();
        assert_eq!(unchanged.username, "bob");
        assert_eq!(unchanged.password, "two");
    }

    #[test]
    fn capture_candidate_reports_locked_vault() {
        let (_root, store) = test_store();
        {
            let epoch = store.require_active_epoch().unwrap();
            store.set_capture_enabled_at(true, epoch).unwrap();
            store.lock_current_session().unwrap();
        }
        let (service, recorded) = connected_service("connection-a");

        service.handle_capture_candidate(
            &store,
            &secret_event(
                "connection-a",
                "captureCandidate",
                serde_json::json!({
                    "candidateId": "candidate-locked",
                    "origin": "https://example.com",
                    "username": "alice",
                    "password": "one",
                    "tabId": 7,
                    "frameId": 0,
                    "documentId": "document-1",
                }),
            ),
        );

        let matches = recorded.requests_for("password.captureMatch");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["payload"]["action"], "locked");
        assert!(!lock_unpoisoned(&service.captures).contains_key("candidate-locked"));
    }

    #[test]
    fn sync_capture_is_idempotent_until_connection_state_is_cleared() {
        let (_root, store) = test_store();
        {
            let epoch = store.require_active_epoch().unwrap();
            store.set_capture_enabled_at(true, epoch).unwrap();
        }
        let (service, recorded) = connected_service("connection-a");

        service.sync_capture_from_store(&store);
        service.sync_capture_from_store(&store);
        let syncs = recorded.requests_for("password.setCaptureEnabled");
        assert_eq!(syncs.len(), 1);
        assert_eq!(syncs[0]["payload"]["enabled"], true);

        service.clear_connection_state_data("connection-a");
        service.sync_capture_from_store(&store);
        let syncs = recorded.requests_for("password.setCaptureEnabled");
        assert_eq!(syncs.len(), 2);
        assert_eq!(syncs[1]["payload"]["enabled"], true);
    }

    #[test]
    fn sync_capture_always_pushes_enabled_since_0_7_2() {
        let (_root, store) = test_store();
        {
            let epoch = store.require_active_epoch().unwrap();
            // The legacy toggle no longer disables capture.
            store.set_capture_enabled_at(false, epoch).unwrap();
        }
        let (service, recorded) = connected_service("connection-a");

        service.sync_capture_from_store(&store);
        let syncs = recorded.requests_for("password.setCaptureEnabled");
        assert_eq!(syncs.len(), 1);
        assert_eq!(syncs[0]["payload"]["enabled"], true);
    }

    #[cfg(windows)]
    #[test]
    fn copy_secret_copies_username_and_password() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, _recorded) = connected_service("connection-a");

        service.handle_copy_secret(
            &store,
            &secret_event(
                "connection-a",
                "copySecret",
                serde_json::json!({ "entryId": entry.id, "field": "username" }),
            ),
        );
        service.handle_copy_secret(
            &store,
            &secret_event(
                "connection-a",
                "copySecret",
                serde_json::json!({ "entryId": entry.id, "field": "password" }),
            ),
        );

        let diag = service.bridge.diag_snapshot(20);
        assert!(diag.iter().all(|entry| entry.event != "copy-secret-failed"));
        // The password copy replaced the username copy on the clipboard.
        assert_eq!(read_clipboard_text().as_deref(), Some("one"));
    }

    #[test]
    fn copy_secret_rejects_bad_field_unknown_entry_and_locked_vault() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, _recorded) = connected_service("connection-a");

        for payload in [
            serde_json::json!({ "entryId": entry.id, "field": "notes" }),
            serde_json::json!({ "entryId": Uuid::new_v4(), "field": "password" }),
            serde_json::json!({ "entryId": "not-a-uuid", "field": "password" }),
        ] {
            service.handle_copy_secret(&store, &secret_event("connection-a", "copySecret", payload));
        }
        store.lock_current_session().unwrap();
        service.handle_copy_secret(
            &store,
            &secret_event(
                "connection-a",
                "copySecret",
                serde_json::json!({ "entryId": entry.id, "field": "password" }),
            ),
        );

        let diag = service.bridge.diag_snapshot(20);
        let failures = diag
            .iter()
            .filter(|entry| entry.event == "copy-secret-failed")
            .collect::<Vec<_>>();
        assert_eq!(failures.len(), 4);
        // Diagnostics never carry secret material.
        assert!(failures
            .iter()
            .all(|entry| !entry.detail.contains("alice") && !entry.detail.contains("one")));
    }

    #[test]
    fn delete_entry_removes_the_vault_entry() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, _recorded) = connected_service("connection-a");

        let deleted = service.delete_entry_from_event(
            &store,
            &secret_event(
                "connection-a",
                "deleteEntry",
                serde_json::json!({ "entryId": entry.id }),
            ),
        );
        assert_eq!(deleted.as_deref(), Some(entry.id.as_str()));
        let epoch = store.require_active_epoch().unwrap();
        assert!(store.list_entries_at(epoch).unwrap().is_empty());
        let diag = service.bridge.diag_snapshot(20);
        assert!(diag
            .iter()
            .all(|entry| entry.event != "delete-entry-failed"));
    }

    #[test]
    fn delete_entry_rejects_unknown_malformed_and_locked_requests() {
        let (_root, store) = test_store();
        let entry = create_entry(&store, "https://example.com/login", "alice", "one");
        let (service, _recorded) = connected_service("connection-a");

        for payload in [
            serde_json::json!({ "entryId": Uuid::new_v4() }),
            serde_json::json!({ "entryId": "not-a-uuid" }),
        ] {
            let deleted = service.delete_entry_from_event(
                &store,
                &secret_event("connection-a", "deleteEntry", payload),
            );
            assert!(deleted.is_none());
        }
        // The rejected requests did not touch the vault.
        let epoch = store.require_active_epoch().unwrap();
        assert_eq!(store.list_entries_at(epoch).unwrap().len(), 1);

        store.lock_current_session().unwrap();
        let deleted = service.delete_entry_from_event(
            &store,
            &secret_event(
                "connection-a",
                "deleteEntry",
                serde_json::json!({ "entryId": entry.id }),
            ),
        );
        assert!(deleted.is_none());

        let diag = service.bridge.diag_snapshot(20);
        assert_eq!(
            diag.iter()
                .filter(|entry| entry.event == "delete-entry-failed")
                .count(),
            3
        );
    }

    /// Reads CF_UNICODETEXT from the system clipboard for the copy-secret
    /// test. Windows-only like the clipboard implementation itself.
    #[cfg(windows)]
    fn read_clipboard_text() -> Option<String> {
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        };
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        const CF_UNICODETEXT: u32 = 13;
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return None;
        }
        let result = (|| {
            if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
                return None;
            }
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
            if handle.is_null() {
                return None;
            }
            let size = unsafe { GlobalSize(handle) } as usize;
            let ptr = unsafe { GlobalLock(handle) } as *const u16;
            if ptr.is_null() || size < 2 {
                return None;
            }
            let units = unsafe { std::slice::from_raw_parts(ptr, size / 2) };
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..end]);
            unsafe {
                let _ = GlobalUnlock(handle);
            }
            Some(text)
        })();
        unsafe {
            CloseClipboard();
        }
        result
    }

    #[test]
    fn status_reports_layered_state_without_consent_actions() {
        let disconnected = service();
        let (service, recorded) = connected_service("connection-a");
        recorded.set_response(
            "password.getStatus",
            serde_json::json!({ "authenticationConsent": false }),
        );
        let status = service.status();
        assert_eq!(status.capture_permission, "unknown");
        assert!(status.message.as_deref().unwrap().contains("权限状态异常"));
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["capturePermission"], "unknown");
        assert_eq!(value["pipeConnected"], true);
        assert_eq!(value["connectionId"], "connection-a");
        assert!(value.get("consentArmed").is_none());
        assert!(value.get("consentActionRequired").is_none());

        recorded.set_response(
            "password.getStatus",
            serde_json::json!({ "authenticationConsent": true }),
        );
        assert_eq!(service.status().capture_permission, "granted");

        let status = disconnected.status();
        assert_eq!(status.capture_permission, "unavailable");
        assert!(!status.pipe_connected);
    }
}

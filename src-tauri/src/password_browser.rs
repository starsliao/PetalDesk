use crate::browser_bridge::BrowserFamily;
use crate::browser_secret_bridge::{BrowserSecretBridge, BrowserSecretEvent};
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
    consent_armed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    consent_action_required: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    action_required: Option<String>,
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
                consent_armed: false,
                consent_action_required: None,
                message: Some("密码浏览器集成首版仅支持 Windows。".to_string()),
            };
        }
        #[cfg(windows)]
        let connection_id = self.bridge.latest_connection_id(BrowserFamily::Firefox);
        #[cfg(windows)]
        let had_connection = connection_id.is_some();
        #[cfg(windows)]
        let extension_status_result = connection_id.map(|connection_id| {
            self.bridge.request_connection(
                &connection_id,
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
        let consent_armed = extension_status
            .as_ref()
            .and_then(|status| status.get("consentArmed"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        #[cfg(windows)]
        let consent_action_required = extension_status
            .as_ref()
            .and_then(|status| status.get("consentActionRequired"))
            .and_then(Value::as_str)
            .filter(|value| *value == "toolbar-click")
            .map(str::to_string);
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
            extension_version: None,
            install_url: (!known_extension).then(|| FIREFOX_AMO_URL.to_string()),
            capture_permission: match (authentication_consent, known_extension, unsupported) {
                (Some(true), _, _) => "granted",
                (Some(false), _, _) => "action-required",
                (None, true, false) => "unknown",
                _ => "unavailable",
            },
            authentication_consent: authentication_consent.unwrap_or(false),
            consent_armed,
            consent_action_required,
            message: if unsupported {
                Some("当前 Firefox 扩展不支持密码功能，请更新扩展。".to_string())
            } else if status_error.is_some() {
                Some("Firefox 密码通道通信异常；请稍后重试，必要时重启飞花或 Firefox。".to_string())
            } else if !connected {
                Some("Firefox 扩展或本机通信组件尚未连接；仍可复制账号和密码。".to_string())
            } else if authentication_consent == Some(false) {
                Some("登录信息检测等待 Firefox 工具栏中的扩展授权。".to_string())
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
        let response = result.map_err(|error| {
            lock_unpoisoned(&self.fills).remove(&session_id);
            browser_error("password_fill_start_failed", error)
        })?;
        let action_required = response
            .get("actionRequired")
            .and_then(Value::as_str)
            .filter(|value| *value == "toolbar-click")
            .map(str::to_string);
        Ok(PasswordFillTicket {
            session_id,
            entry_id: entry_id.to_string(),
            browser: "firefox",
            origin: data.origin.clone(),
            expires_at: unix_time_ms().saturating_add(FILL_TTL.as_millis()),
            action_required,
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

    pub(crate) fn sync_capture_from_store(&self, app: &AppHandle) {
        let store = app.state::<PasswordStore>();
        let Ok(epoch) = store.require_active_epoch() else {
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
            let granted = if status.capture_enabled {
                self.bridge
                    .request_connection(
                        &connection_id,
                        "password.requestConsent",
                        Value::Object(Default::default()),
                        Duration::from_secs(2),
                    )
                    .ok()
                    .and_then(|value| value.get("granted").and_then(Value::as_bool))
                    .unwrap_or(false)
            } else {
                false
            };
            let _ = self.bridge.request_connection(
                &connection_id,
                "password.setCaptureEnabled",
                serde_json::json!({
                    "enabled": status.capture_enabled && granted,
                    "insecureOrigins": insecure_origins.clone(),
                }),
                Duration::from_secs(2),
            );
        }
    }

    fn handle_event(&self, app: &AppHandle, event: BrowserSecretEvent) {
        if event.browser != BrowserFamily::Firefox {
            return;
        }
        self.prune(app);
        match event.event.as_str() {
            "connectionReady" => self.sync_capture_from_store(app),
            "connectionClosed" => {
                self.clear_connection_state(app, &event.connection_id);
                self.sync_capture_from_store(app);
            }
            "tabReady" => self.handle_tab_ready(app, &event),
            "fillConfirm" => self.handle_fill_confirm(app, &event),
            "fillResult" => self.handle_fill_result(&event),
            "captureCandidate" => self.handle_capture_candidate(app, &event),
            "pageClosed" => self.handle_page_closed(&event),
            "saveDecision" => self.handle_save_decision(app, &event),
            "templateRecordingReady" => self.handle_template_recording_ready(app, &event),
            "templateRecordingResult" => self.handle_template_recording_result(app, &event),
            "templateRecordingCancelled" => self.handle_template_recording_cancelled(app, &event),
            "consentChanged" => {
                if event.payload.get("granted").and_then(Value::as_bool) == Some(true) {
                    self.sync_capture_from_store(app);
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
        if self.authentication_consent(&session.connection_id) != Some(true) {
            return;
        }
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
        let Ok(data) = app
            .state::<PasswordStore>()
            .browser_fill_data(&session.entry_id)
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

    fn authentication_consent(&self, connection_id: &str) -> Option<bool> {
        self.bridge
            .request_connection(
                connection_id,
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::from_secs(2),
            )
            .ok()
            .and_then(|status| status.get("authenticationConsent").and_then(Value::as_bool))
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

    fn handle_fill_confirm(&self, app: &AppHandle, event: &BrowserSecretEvent) {
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
        let Ok(data) = app
            .state::<PasswordStore>()
            .browser_fill_data(&session.entry_id)
        else {
            lock_unpoisoned(&self.fills).remove(session_id);
            return;
        };
        let result = self.bridge.request_connection(
            &session.connection_id,
            "password.provideCredentials",
            serde_json::json!({
                "sessionId": session_id,
                "offerId": session.offer_id,
                "origin": session.origin,
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

    fn handle_capture_candidate(&self, app: &AppHandle, event: &BrowserSecretEvent) {
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
        if frame_id != 0 {
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
        let store = app.state::<PasswordStore>();
        let decision = store.require_active_epoch().and_then(|epoch| {
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
            Err(_) => (
                "same",
                None,
                Vec::new(),
                origin.clone(),
                allow_insecure_http,
            ),
        };
        if action != "same" && action != "username-required" {
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
        let Some(candidate_id) = event.payload.get("candidateId").and_then(Value::as_str) else {
            return;
        };
        let action = event
            .payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("ignore");

        let mut captures = lock_unpoisoned(&self.captures);
        let Some(candidate) = captures.get_mut(candidate_id) else {
            return;
        };
        if candidate.connection_id != event.connection_id {
            return;
        }
        if !capture_decision_matches(candidate, &event.payload) {
            captures.remove(candidate_id);
            return;
        }
        if action == "ignore" {
            captures.remove(candidate_id);
            return;
        }

        let selected_entry_id = event
            .payload
            .get("entryId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let valid_action = match (candidate.matched_action.as_str(), action) {
            ("new", "new") => candidate.entry_id.is_none() && !candidate.username.is_empty(),
            ("update", "update") => candidate.entry_id.is_some(),
            ("select", "replace") => selected_entry_id.as_ref().is_some_and(|entry_id| {
                candidate
                    .account_choices
                    .iter()
                    .any(|choice| &choice.entry_id == entry_id)
            }),
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
            return;
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
            return;
        }
        candidate.save_pending = true;
        let pending = captures.remove(candidate_id);
        drop(captures);
        let Some(pending) = pending else { return };
        let store = app.state::<PasswordStore>();
        let epoch = store.require_active_epoch();
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
                                    let username =
                                        if action == "replace" || pending.username.is_empty() {
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
                let _ = app.emit_to(
                    "passwords",
                    "password_entries_changed",
                    serde_json::json!({ "entryId": entry.id, "action": action }),
                );
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
    app.state::<PasswordBrowserService>()
        .sync_capture_from_store(app);
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

fn fill_target_matches(session: &FillSession, payload: &Value) -> bool {
    payload.get("origin").and_then(Value::as_str) == Some(session.origin.as_str())
        && payload.get("tabId").and_then(Value::as_i64) == session.tab_id
        && payload.get("frameId").and_then(Value::as_i64) == session.frame_id
        && session.frame_id == Some(0)
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
    fill_target_matches(session, payload)
        && payload.get("documentId").and_then(Value::as_str) == session.document_id.as_deref()
}

fn capture_decision_matches(capture: &PendingCapture, payload: &Value) -> bool {
    payload.get("origin").and_then(Value::as_str) == Some(capture.origin.as_str())
        && payload.get("promptOrigin").and_then(Value::as_str)
            == Some(capture.prompt_origin.as_str())
        && payload.get("tabId").and_then(Value::as_i64) == Some(capture.tab_id)
        && payload.get("frameId").and_then(Value::as_i64) == Some(capture.frame_id)
        && payload.get("documentId").and_then(Value::as_str) == Some(capture.document_id.as_str())
        && capture.frame_id == 0
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
}

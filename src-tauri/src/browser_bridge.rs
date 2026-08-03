use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const PROTOCOL_VERSION: u32 = 1;
const MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(5);
const SESSION_FUTURE_SKEW: Duration = Duration::from_secs(5);
const RESPONSE_POLL_INTERVAL: Duration = Duration::from_millis(25);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserFamily {
    Chrome,
    Edge,
    Firefox,
}

impl BrowserFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Firefox => "firefox",
        }
    }
}

impl FromStr for BrowserFamily {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chrome" => Ok(Self::Chrome),
            "edge" => Ok(Self::Edge),
            "firefox" => Ok(Self::Firefox),
            _ => Err(format!("unsupported browser family: {value}")),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConnectionStatus {
    pub connected: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_unix_ms: Option<u128>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBridgeStatus {
    pub chrome: BrowserConnectionStatus,
    pub edge: BrowserConnectionStatus,
    pub firefox: BrowserConnectionStatus,
}

#[derive(Debug, Clone)]
pub struct BrowserBridge {
    root: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetadata {
    version: u32,
    connection_id: String,
    browser: String,
    extension_version: String,
    #[serde(default)]
    extension_id: String,
    #[serde(default)]
    capabilities: Vec<String>,
    process_id: u32,
    last_seen_unix_ms: u128,
}

#[derive(Debug, Clone)]
struct LiveSession {
    connection_id: String,
    browser: BrowserFamily,
    extension_version: String,
    extension_id: String,
    capabilities: Vec<String>,
    process_id: u32,
    last_seen_unix_ms: u128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandEnvelope<'a> {
    protocol_version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    command: &'a str,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct ResponseEnvelope {
    version: u32,
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

struct RequestCleanup {
    command_path: PathBuf,
    response_path: PathBuf,
}

impl Drop for RequestCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.command_path);
        let _ = fs::remove_file(&self.response_path);
    }
}

impl BrowserBridge {
    pub fn start() -> Result<Self, String> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "LOCALAPPDATA is unavailable".to_string())?;
        Self::start_at(
            PathBuf::from(local_app_data)
                .join("PetalDesk")
                .join("browser-bridge"),
        )
    }

    fn start_at(root: PathBuf) -> Result<Self, String> {
        for directory in [
            root.join("sessions"),
            root.join("commands"),
            root.join("responses"),
        ] {
            fs::create_dir_all(&directory)
                .map_err(|error| format!("failed to create browser bridge directory: {error}"))?;
        }
        Ok(Self { root })
    }

    pub fn status(&self) -> Result<BrowserBridgeStatus, String> {
        let sessions = self.live_sessions()?;
        Ok(BrowserBridgeStatus {
            chrome: status_for(&sessions, BrowserFamily::Chrome),
            edge: status_for(&sessions, BrowserFamily::Edge),
            firefox: status_for(&sessions, BrowserFamily::Firefox),
        })
    }

    pub fn request(
        &self,
        browser: BrowserFamily,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let session = self
            .latest_session(browser)?
            .ok_or_else(|| format!("{} browser extension is not connected", browser.as_str()))?;
        self.request_session(browser, session, command, payload, timeout)
    }

    /// Sends a command to the exact native-host connection selected when a
    /// capture began. A reconnect must never move an in-flight capture to the
    /// newest tab/session implicitly.
    pub fn request_connection(
        &self,
        browser: BrowserFamily,
        connection_id: &str,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let session = self.live_session(connection_id)?;
        if session.browser != browser {
            return Err(format!(
                "browser extension connection {connection_id} belongs to {}, not {}",
                session.browser.as_str(),
                browser.as_str()
            ));
        }
        self.request_session(browser, session, command, payload, timeout)
    }

    /// Browser families can have one Native Messaging connection per profile.
    /// Without a browser-provided window identity, selecting one of several
    /// live profiles would risk scrolling the wrong page. Callers can fall
    /// back to the generic capture engine when this returns false.
    pub fn connection_is_unique(
        &self,
        browser: BrowserFamily,
        connection_id: &str,
    ) -> Result<bool, String> {
        if !is_safe_identifier(connection_id) {
            return Err("browser extension connection ID is invalid".to_string());
        }
        let entries = fs::read_dir(self.root.join("sessions"))
            .map_err(|error| format!("failed to read browser extension sessions: {error}"))?;
        let now = unix_time_ms();
        let mut count = 0_usize;
        let mut selected = false;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = read_limited(&path) else {
                continue;
            };
            let Ok(metadata) = serde_json::from_slice::<SessionMetadata>(&bytes) else {
                continue;
            };
            let Some(session) = live_session_from_metadata(&path, metadata, now) else {
                continue;
            };
            if session.browser == browser {
                count += 1;
                selected |= session.connection_id == connection_id;
            }
        }
        Ok(count == 1 && selected)
    }

    fn request_session(
        &self,
        browser: BrowserFamily,
        session: LiveSession,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        validate_command(command)?;
        if command != "ping"
            && !session.capabilities.is_empty()
            && !session
                .capabilities
                .iter()
                .any(|capability| capability == command)
        {
            return Err(format!(
                "{} browser extension does not support command {command}",
                browser.as_str()
            ));
        }
        let request_id = Uuid::new_v4().to_string();
        let command_directory = self.root.join("commands").join(&session.connection_id);
        let response_directory = self.root.join("responses").join(&session.connection_id);
        if !command_directory.is_dir() || !response_directory.is_dir() {
            return Err(format!(
                "{} browser extension session disconnected",
                browser.as_str()
            ));
        }

        let command_path = command_directory.join(format!("{request_id}.json"));
        let response_path = response_directory.join(format!("{request_id}.json"));
        let _cleanup = RequestCleanup {
            command_path: command_path.clone(),
            response_path: response_path.clone(),
        };
        atomic_write_json(
            &command_path,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                kind: "command",
                id: &request_id,
                command,
                payload,
            },
        )?;

        let started = Instant::now();
        let deadline = started
            .checked_add(timeout)
            .ok_or_else(|| "browser extension request timeout is too large".to_string())?;
        loop {
            if response_path.is_file() {
                return read_response(&response_path, &request_id);
            }

            let now = Instant::now();
            if now >= deadline {
                if response_path.is_file() {
                    return read_response(&response_path, &request_id);
                }
                return Err(format!(
                    "{} browser extension request timed out after {} ms",
                    browser.as_str(),
                    started.elapsed().as_millis()
                ));
            }
            thread::sleep(RESPONSE_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
        }
    }

    pub fn request_default(
        &self,
        browser: BrowserFamily,
        command: &str,
        payload: Value,
    ) -> Result<Value, String> {
        self.request(browser, command, payload, DEFAULT_REQUEST_TIMEOUT)
    }

    fn latest_session(&self, browser: BrowserFamily) -> Result<Option<LiveSession>, String> {
        Ok(self.live_sessions()?.remove(&browser))
    }

    fn live_session(&self, connection_id: &str) -> Result<LiveSession, String> {
        if !is_safe_identifier(connection_id) {
            return Err("browser extension connection ID is invalid".to_string());
        }
        let path = self
            .root
            .join("sessions")
            .join(format!("{connection_id}.json"));
        let bytes = read_limited(&path)
            .map_err(|_| format!("browser extension connection {connection_id} disconnected"))?;
        let metadata = serde_json::from_slice::<SessionMetadata>(&bytes)
            .map_err(|_| format!("browser extension connection {connection_id} is invalid"))?;
        live_session_from_metadata(&path, metadata, unix_time_ms()).ok_or_else(|| {
            format!("browser extension connection {connection_id} is stale or invalid")
        })
    }

    fn live_sessions(&self) -> Result<HashMap<BrowserFamily, LiveSession>, String> {
        let sessions_directory = self.root.join("sessions");
        let entries = fs::read_dir(&sessions_directory)
            .map_err(|error| format!("failed to read browser extension sessions: {error}"))?;
        let now = unix_time_ms();
        let mut sessions = HashMap::<BrowserFamily, LiveSession>::new();

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = read_limited(&path) else {
                continue;
            };
            let Ok(metadata) = serde_json::from_slice::<SessionMetadata>(&bytes) else {
                continue;
            };
            let Some(live) = live_session_from_metadata(&path, metadata, now) else {
                continue;
            };
            let browser = live.browser;
            let replace = sessions
                .get(&browser)
                .map(|current| live.last_seen_unix_ms > current.last_seen_unix_ms)
                .unwrap_or(true);
            if replace {
                sessions.insert(browser, live);
            }
        }

        Ok(sessions)
    }
}

fn live_session_from_metadata(
    path: &Path,
    metadata: SessionMetadata,
    now_unix_ms: u128,
) -> Option<LiveSession> {
    if metadata.version != PROTOCOL_VERSION
        || !is_safe_identifier(&metadata.connection_id)
        || path.file_stem().and_then(|value| value.to_str())
            != Some(metadata.connection_id.as_str())
        || !session_is_fresh(metadata.last_seen_unix_ms, now_unix_ms)
    {
        return None;
    }
    let browser = BrowserFamily::from_str(&metadata.browser).ok()?;
    Some(LiveSession {
        connection_id: metadata.connection_id,
        browser,
        extension_version: metadata.extension_version,
        extension_id: metadata.extension_id,
        capabilities: metadata.capabilities,
        process_id: metadata.process_id,
        last_seen_unix_ms: metadata.last_seen_unix_ms,
    })
}

fn status_for(
    sessions: &HashMap<BrowserFamily, LiveSession>,
    browser: BrowserFamily,
) -> BrowserConnectionStatus {
    let Some(session) = sessions.get(&browser) else {
        return BrowserConnectionStatus::default();
    };
    debug_assert_eq!(session.browser, browser);
    BrowserConnectionStatus {
        connected: true,
        ready: true,
        connection_id: Some(session.connection_id.clone()),
        extension_version: Some(session.extension_version.clone()),
        extension_id: (!session.extension_id.is_empty()).then(|| session.extension_id.clone()),
        capabilities: session.capabilities.clone(),
        process_id: Some(session.process_id),
        last_seen_unix_ms: Some(session.last_seen_unix_ms),
    }
}

fn session_is_fresh(last_seen_unix_ms: u128, now_unix_ms: u128) -> bool {
    let ttl = SESSION_TTL.as_millis();
    let future_skew = SESSION_FUTURE_SKEW.as_millis();
    last_seen_unix_ms <= now_unix_ms.saturating_add(future_skew)
        && now_unix_ms.saturating_sub(last_seen_unix_ms) <= ttl
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn validate_command(command: &str) -> Result<(), String> {
    if command.starts_with("password.")
        || command.is_empty()
        || command.len() > 64
        || !command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("browser extension command name is invalid".to_string());
    }
    Ok(())
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to inspect browser bridge message: {error}"))?;
    if metadata.len() == 0 || metadata.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "browser bridge message length is invalid: {}",
            metadata.len()
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read browser bridge message: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(format!(
            "browser bridge message length is invalid: {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "browser bridge message has no parent directory".to_string())?;
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("failed to serialize browser bridge message: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(format!(
            "browser bridge message length is invalid: {}",
            bytes.len()
        ));
    }

    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create browser bridge message: {error}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("failed to write browser bridge message: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to commit browser bridge message: {error}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn read_response(path: &Path, expected_id: &str) -> Result<Value, String> {
    let bytes = read_limited(path)?;
    let response = serde_json::from_slice::<ResponseEnvelope>(&bytes)
        .map_err(|error| format!("browser extension response is invalid: {error}"))?;
    if response.version != PROTOCOL_VERSION {
        return Err(format!(
            "browser extension protocol version is incompatible: {}",
            response.version
        ));
    }
    if response.id != expected_id {
        return Err("browser extension response ID does not match request".to_string());
    }
    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }
    Err(response
        .error
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| response.error.as_ref().and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| "browser extension request failed".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("petaldesk-browser-bridge-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(
        root: &Path,
        connection_id: &str,
        browser: &str,
        extension_version: &str,
        last_seen_unix_ms: u128,
    ) {
        let session = SessionMetadata {
            version: PROTOCOL_VERSION,
            connection_id: connection_id.to_string(),
            browser: browser.to_string(),
            extension_version: extension_version.to_string(),
            extension_id: format!("{browser}-extension-id"),
            capabilities: vec![
                "prepare".to_string(),
                "start".to_string(),
                "step".to_string(),
                "status".to_string(),
                "restore".to_string(),
                "cancel".to_string(),
            ],
            process_id: 42,
            last_seen_unix_ms,
        };
        atomic_write_json(
            &root.join("sessions").join(format!("{connection_id}.json")),
            &session,
        )
        .unwrap();
        fs::create_dir_all(root.join("commands").join(connection_id)).unwrap();
        fs::create_dir_all(root.join("responses").join(connection_id)).unwrap();
    }

    #[test]
    fn status_uses_freshest_live_session_per_browser() {
        let temporary = TestRoot::new();
        let bridge = BrowserBridge::start_at(temporary.0.clone()).unwrap();
        let now = unix_time_ms();
        session(&temporary.0, "chrome-old", "chrome", "0.1.0", now - 1_000);
        session(&temporary.0, "chrome-new", "chrome", "0.2.0", now);
        session(
            &temporary.0,
            "firefox-stale",
            "firefox",
            "0.1.0",
            now - SESSION_TTL.as_millis() - 1,
        );

        let status = bridge.status().unwrap();
        assert!(status.chrome.connected);
        assert!(status.chrome.ready);
        assert_eq!(status.chrome.connection_id.as_deref(), Some("chrome-new"));
        assert_eq!(status.chrome.extension_version.as_deref(), Some("0.2.0"));
        assert!(!status.edge.connected);
        assert!(!status.firefox.connected);
    }

    #[test]
    fn exact_connection_request_does_not_switch_to_a_newer_session() {
        let temporary = TestRoot::new();
        let bridge = BrowserBridge::start_at(temporary.0.clone()).unwrap();
        let connection_id = "firefox-session";
        session(
            &temporary.0,
            connection_id,
            "firefox",
            "0.1.0",
            unix_time_ms(),
        );
        session(
            &temporary.0,
            "firefox-newer",
            "firefox",
            "0.2.0",
            unix_time_ms().saturating_add(1),
        );

        let command_directory = temporary.0.join("commands").join(connection_id);
        let response_directory = temporary.0.join("responses").join(connection_id);
        let host = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                for entry in fs::read_dir(&command_directory)
                    .unwrap()
                    .filter_map(Result::ok)
                {
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let command: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    let fixture: Value = serde_json::from_str(include_str!(
                        "../../browser-extension/test/fixtures/native-protocol.json"
                    ))
                    .unwrap();
                    assert_eq!(
                        command["protocolVersion"],
                        fixture["command"]["protocolVersion"]
                    );
                    assert_eq!(command["type"], fixture["command"]["type"]);
                    assert_eq!(command["command"], fixture["command"]["command"]);
                    let id = command["id"].as_str().unwrap();
                    atomic_write_json(
                        &response_directory.join(format!("{id}.json")),
                        &serde_json::json!({
                            "version": PROTOCOL_VERSION,
                            "id": id,
                            "ok": true,
                            "result": { "state": "idle" }
                        }),
                    )
                    .unwrap();
                    fs::remove_file(path).unwrap();
                    return;
                }
                assert!(Instant::now() < deadline, "command file was not published");
                thread::sleep(Duration::from_millis(5));
            }
        });

        let response = bridge
            .request_connection(
                BrowserFamily::Firefox,
                connection_id,
                "status",
                serde_json::json!({}),
                Duration::from_secs(3),
            )
            .unwrap();
        host.join().unwrap();
        assert_eq!(response["state"], "idle");
    }

    #[test]
    fn exact_connection_request_rejects_wrong_browser_and_stale_heartbeat() {
        let temporary = TestRoot::new();
        let bridge = BrowserBridge::start_at(temporary.0.clone()).unwrap();
        let now = unix_time_ms();
        session(&temporary.0, "chrome-live", "chrome", "0.3.1", now);
        session(
            &temporary.0,
            "edge-stale",
            "edge",
            "0.3.1",
            now.saturating_sub(SESSION_TTL.as_millis() + 1),
        );

        let wrong_browser = bridge
            .request_connection(
                BrowserFamily::Firefox,
                "chrome-live",
                "status",
                serde_json::json!({}),
                Duration::from_millis(10),
            )
            .unwrap_err();
        assert!(wrong_browser.contains("belongs to chrome"));

        let stale = bridge
            .request_connection(
                BrowserFamily::Edge,
                "edge-stale",
                "status",
                serde_json::json!({}),
                Duration::from_millis(10),
            )
            .unwrap_err();
        assert!(stale.contains("stale or invalid"));
    }

    #[test]
    fn unique_connection_check_rejects_ambiguous_browser_profiles() {
        let temporary = TestRoot::new();
        let bridge = BrowserBridge::start_at(temporary.0.clone()).unwrap();
        let now = unix_time_ms();
        session(&temporary.0, "firefox-primary", "firefox", "0.3.1", now);
        session(&temporary.0, "chrome-only", "chrome", "0.3.1", now);

        assert!(bridge
            .connection_is_unique(BrowserFamily::Firefox, "firefox-primary")
            .unwrap());
        assert!(bridge
            .connection_is_unique(BrowserFamily::Chrome, "chrome-only")
            .unwrap());

        session(
            &temporary.0,
            "firefox-second-profile",
            "firefox",
            "0.3.1",
            now,
        );
        assert!(!bridge
            .connection_is_unique(BrowserFamily::Firefox, "firefox-primary")
            .unwrap());
        assert!(!bridge
            .connection_is_unique(BrowserFamily::Firefox, "firefox-second-profile")
            .unwrap());
    }

    #[test]
    fn request_timeout_cleans_its_spool_files() {
        let temporary = TestRoot::new();
        let bridge = BrowserBridge::start_at(temporary.0.clone()).unwrap();
        let connection_id = "edge-session";
        session(&temporary.0, connection_id, "edge", "0.1.0", unix_time_ms());

        let error = bridge
            .request(
                BrowserFamily::Edge,
                "status",
                serde_json::json!({}),
                Duration::from_millis(60),
            )
            .unwrap_err();
        assert!(error.contains("timed out"));
        assert_eq!(
            fs::read_dir(temporary.0.join("commands").join(connection_id))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(temporary.0.join("responses").join(connection_id))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn rejects_unsafe_session_and_command_identifiers() {
        assert!(is_safe_identifier("request-42_test"));
        assert!(!is_safe_identifier("../request"));
        assert!(validate_command("capture.status").is_ok());
        assert!(validate_command("password.provideCredentials").is_err());
        assert!(validate_command("../status").is_err());
    }
}

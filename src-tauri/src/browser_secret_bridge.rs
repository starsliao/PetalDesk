//! Memory-only browser channel for credentials and login-capture events.
//!
//! The existing browser bridge intentionally uses JSON files for long capture.
//! Password messages use a separate local named pipe so credentials never
//! enter that spool. The pipe rejects remote clients, has a current-user ACL,
//! and requires a random per-process token published with a short-lived endpoint.

use crate::browser_bridge::BrowserFamily;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SECRET_PROTOCOL_VERSION: u32 = 1;
const MAX_SECRET_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_EVENTS: usize = 128;
const ENDPOINT_TTL: Duration = Duration::from_secs(5 * 60);
const ENDPOINT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const CONNECTION_STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct BrowserSecretEvent {
    pub connection_id: String,
    pub browser: BrowserFamily,
    pub event: String,
    pub payload: Value,
}

#[derive(Clone)]
struct SecretConnection {
    browser: BrowserFamily,
    sender: SyncSender<Value>,
    connected_at: Instant,
    generation: Uuid,
    control: Arc<SecretConnectionControl>,
}

struct SecretConnectionControl {
    retired: AtomicBool,
    #[cfg(windows)]
    pipe: Mutex<Option<std::fs::File>>,
    #[cfg(windows)]
    reader_thread: Mutex<Option<std::os::windows::io::OwnedHandle>>,
    #[cfg(windows)]
    writer_thread: Mutex<Option<std::os::windows::io::OwnedHandle>>,
}

struct SecretInner {
    // The endpoint refresh thread rotates this value. Handshakes take a short
    // snapshot so an already-connected pipe is not interrupted by rotation.
    token: Mutex<String>,
    endpoint_path: PathBuf,
    connections: Mutex<HashMap<String, SecretConnection>>,
    pending: Mutex<HashMap<String, SyncSender<Result<Value, String>>>>,
    events: Mutex<VecDeque<BrowserSecretEvent>>,
    event_ready: Condvar,
}

impl SecretConnectionControl {
    fn new(pipe: &std::fs::File) -> Result<Self, String> {
        Ok(Self {
            retired: AtomicBool::new(false),
            #[cfg(windows)]
            pipe: Mutex::new(Some(pipe.try_clone().map_err(|error| {
                format!("failed to clone browser secret pipe shutdown handle: {error}")
            })?)),
            #[cfg(windows)]
            reader_thread: Mutex::new(None),
            #[cfg(windows)]
            writer_thread: Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn detached() -> Self {
        Self {
            retired: AtomicBool::new(false),
            #[cfg(windows)]
            pipe: Mutex::new(None),
            #[cfg(windows)]
            reader_thread: Mutex::new(None),
            #[cfg(windows)]
            writer_thread: Mutex::new(None),
        }
    }

    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    #[cfg(windows)]
    fn register_reader_thread(&self) -> Result<(), String> {
        self.register_current_thread(&self.reader_thread)
    }

    #[cfg(not(windows))]
    fn register_reader_thread(&self) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn register_writer_thread(&self) -> Result<(), String> {
        self.register_current_thread(&self.writer_thread)
    }

    #[cfg(not(windows))]
    fn register_writer_thread(&self) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn register_current_thread(
        &self,
        slot: &Mutex<Option<std::os::windows::io::OwnedHandle>>,
    ) -> Result<(), String> {
        let handle = current_thread_owned_handle()?;
        let mut slot = lock_unpoisoned(slot);
        *slot = Some(handle);
        if self.is_retired() {
            cancel_registered_thread(&mut slot);
        }
        Ok(())
    }

    fn retire(&self) -> bool {
        if self.retired.swap(true, Ordering::AcqRel) {
            return false;
        }
        #[cfg(windows)]
        {
            cancel_registered_thread(&mut lock_unpoisoned(&self.reader_thread));
            cancel_registered_thread(&mut lock_unpoisoned(&self.writer_thread));
            disconnect_registered_pipe(&self.pipe);
        }
        true
    }
}

#[cfg(windows)]
fn current_thread_owned_handle() -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE};

    let handle = unsafe { OpenThread(THREAD_TERMINATE, 0, GetCurrentThreadId()) };
    if handle.is_null() {
        return Err(format!(
            "failed to retain browser secret worker thread: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle as _) })
}

#[cfg(windows)]
fn cancel_registered_thread(slot: &mut Option<std::os::windows::io::OwnedHandle>) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    let Some(handle) = slot.take() else { return };
    if unsafe { CancelSynchronousIo(handle.as_raw_handle() as _) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
            eprintln!("failed to cancel browser secret worker I/O: {error}");
        }
    }
}

#[cfg(windows)]
fn disconnect_registered_pipe(pipe: &Mutex<Option<std::fs::File>>) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_PIPE_NOT_CONNECTED;
    use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;

    let mut pipe = lock_unpoisoned(pipe);
    let Some(handle) = pipe.as_ref() else { return };
    if unsafe { DisconnectNamedPipe(handle.as_raw_handle() as _) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_NOT_CONNECTED as i32) {
            eprintln!("failed to disconnect browser secret pipe: {error}");
        }
    }
    pipe.take();
}

#[derive(Clone)]
pub struct BrowserSecretBridge {
    inner: Arc<SecretInner>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretEndpoint {
    version: u32,
    pipe_name: String,
    token: String,
    process_id: u32,
    expires_at_unix_ms: u128,
}

impl BrowserSecretBridge {
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(SecretInner {
                token: Mutex::new(String::new()),
                endpoint_path: PathBuf::new(),
                connections: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                events: Mutex::new(VecDeque::new()),
                event_ready: Condvar::new(),
            }),
        }
    }

    pub fn start() -> Result<Self, String> {
        #[cfg(not(windows))]
        {
            return Err("password browser integration is available on Windows only".to_string());
        }

        #[cfg(windows)]
        {
            let root = bridge_root()?;
            fs::create_dir_all(&root)
                .map_err(|error| format!("failed to create browser secret directory: {error}"))?;
            let endpoint_path = root.join("secret-endpoint.json");
            let pipe_name = format!(r"\\.\pipe\PetalDesk-password-{}", Uuid::new_v4());
            let token = random_token()?;
            let endpoint = SecretEndpoint {
                version: SECRET_PROTOCOL_VERSION,
                pipe_name: pipe_name.clone(),
                token: token.clone(),
                process_id: std::process::id(),
                expires_at_unix_ms: unix_time_ms().saturating_add(ENDPOINT_TTL.as_millis()),
            };
            atomic_write_json(&endpoint_path, &endpoint)?;
            let inner = Arc::new(SecretInner {
                token: Mutex::new(token),
                endpoint_path,
                connections: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                events: Mutex::new(VecDeque::new()),
                event_ready: Condvar::new(),
            });
            let server_inner = inner.clone();
            std::thread::Builder::new()
                .name("petaldesk-password-pipe".to_string())
                .spawn(move || run_pipe_server(&pipe_name, server_inner))
                .map_err(|error| format!("failed to start browser secret pipe: {error}"))?;
            let endpoint_inner = Arc::downgrade(&inner);
            std::thread::Builder::new()
                .name("petaldesk-password-endpoint-refresh".to_string())
                .spawn(move || {
                    let mut endpoint = endpoint;
                    loop {
                        std::thread::sleep(ENDPOINT_REFRESH_INTERVAL);
                        let Some(inner) = endpoint_inner.upgrade() else {
                            break;
                        };
                        let Ok(next_token) = random_token() else {
                            continue;
                        };
                        let next_endpoint = SecretEndpoint {
                            version: SECRET_PROTOCOL_VERSION,
                            pipe_name: endpoint.pipe_name.clone(),
                            token: next_token.clone(),
                            process_id: endpoint.process_id,
                            expires_at_unix_ms: unix_time_ms()
                                .saturating_add(ENDPOINT_TTL.as_millis()),
                        };
                        // Publish the endpoint first. A failed write leaves the
                        // old endpoint and server token usable, so the native
                        // host can retry without a split-brain handshake.
                        if atomic_write_json(&inner.endpoint_path, &next_endpoint).is_ok() {
                            *lock_unpoisoned(&inner.token) = next_token;
                            endpoint = next_endpoint;
                        }
                    }
                })
                .map_err(|error| format!("failed to refresh browser secret endpoint: {error}"))?;
            Ok(Self { inner })
        }
    }

    pub fn request_connection(
        &self,
        connection_id: &str,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let connection = lock_unpoisoned(&self.inner.connections)
            .get(connection_id)
            .cloned()
            .ok_or_else(|| "password browser connection closed".to_string())?;
        self.request_to_connection(connection_id, connection, command, payload, timeout)
    }

    fn request_to_connection(
        &self,
        connection_id: &str,
        connection: SecretConnection,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        validate_command(command)?;
        if connection.control.is_retired() {
            return Err("password browser connection closed".to_string());
        }

        let id = Uuid::new_v4().to_string();
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        lock_unpoisoned(&self.inner.pending).insert(id.clone(), response_tx);
        let request = serde_json::json!({
            "version": SECRET_PROTOCOL_VERSION,
            "type": "secret.command",
            "id": id,
            "protocolVersion": 1,
            "command": command,
            "payload": payload,
        });
        if connection.control.is_retired() {
            lock_unpoisoned(&self.inner.pending).remove(&id);
            return Err("password browser connection closed".to_string());
        }
        if let Err(error) = connection.sender.try_send(request) {
            lock_unpoisoned(&self.inner.pending).remove(&id);
            close_connection_generation(
                &self.inner,
                connection_id,
                connection.browser,
                connection.generation,
                &connection.control,
            );
            return Err(match error {
                mpsc::TrySendError::Full(_) => "password browser connection is busy".to_string(),
                mpsc::TrySendError::Disconnected(_) => {
                    "password browser connection closed".to_string()
                }
            });
        }
        let result = match response_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                close_connection_generation(
                    &self.inner,
                    connection_id,
                    connection.browser,
                    connection.generation,
                    &connection.control,
                );
                Err(format!(
                    "password browser request timed out after {} ms",
                    timeout.as_millis()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                close_connection_generation(
                    &self.inner,
                    connection_id,
                    connection.browser,
                    connection.generation,
                    &connection.control,
                );
                Err("password browser connection closed".to_string())
            }
        };
        lock_unpoisoned(&self.inner.pending).remove(&id);
        result
    }

    pub fn receive_event(&self, timeout: Duration) -> Option<BrowserSecretEvent> {
        let mut events = lock_unpoisoned(&self.inner.events);
        if events.is_empty() {
            let (next, _) = self
                .inner
                .event_ready
                .wait_timeout(events, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            events = next;
        }
        events.pop_front()
    }

    pub fn is_connected(&self, browser: BrowserFamily) -> bool {
        lock_unpoisoned(&self.inner.connections)
            .values()
            .any(|connection| connection.browser == browser)
    }

    pub fn latest_connection_id(&self, browser: BrowserFamily) -> Option<String> {
        lock_unpoisoned(&self.inner.connections)
            .iter()
            .filter(|(_, connection)| connection.browser == browser)
            .max_by_key(|(_, connection)| connection.connected_at)
            .map(|(id, _)| id.clone())
    }

    pub fn connection_ids(&self, browser: BrowserFamily) -> Vec<String> {
        lock_unpoisoned(&self.inner.connections)
            .iter()
            .filter(|(_, connection)| connection.browser == browser)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

impl Drop for SecretInner {
    fn drop(&mut self) {
        if !self.endpoint_path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.endpoint_path);
        }
    }
}

fn bridge_root() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .ok_or_else(|| "unable to locate local application data".to_string())
        .map(|root| root.join("PetalDesk").join("browser-bridge"))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("failed to create browser secret token: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn validate_command(command: &str) -> Result<(), String> {
    if !command.starts_with("password.")
        || command.len() > 64
        || !command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("password browser command name is invalid".to_string());
    }
    Ok(())
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "browser secret endpoint has no parent directory".to_string())?;
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("failed to encode browser secret endpoint: {error}"))?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create browser secret endpoint: {error}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("failed to write browser secret endpoint: {error}"))?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| format!("failed to replace browser secret endpoint: {error}"))?;
        }
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to publish browser secret endpoint: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Value>, String> {
    let mut length = [0_u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("failed to read browser secret frame: {error}")),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_SECRET_MESSAGE_BYTES {
        return Err(format!("browser secret frame length is invalid: {length}"));
    }
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read browser secret frame: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("browser secret frame is invalid: {error}"))
}

fn write_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("failed to encode browser secret frame: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_SECRET_MESSAGE_BYTES {
        return Err(format!(
            "browser secret frame length is invalid: {}",
            bytes.len()
        ));
    }
    let length = (bytes.len() as u32).to_le_bytes();
    writer
        .write_all(&length)
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write browser secret frame: {error}"))
}

fn handle_pipe_connection(mut pipe: std::fs::File, inner: Arc<SecretInner>) -> Result<(), String> {
    let hello = read_frame(&mut pipe)?.ok_or_else(|| "browser secret pipe closed".to_string())?;
    let expected_token = lock_unpoisoned(&inner.token).clone();
    if hello.get("type").and_then(Value::as_str) != Some("secret.hello")
        || hello.get("version").and_then(Value::as_u64) != Some(SECRET_PROTOCOL_VERSION as u64)
        || hello.get("token").and_then(Value::as_str) != Some(expected_token.as_str())
    {
        return Err("browser secret handshake was rejected".to_string());
    }
    let connection_id = hello
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 96)
        .ok_or_else(|| "browser secret handshake has no connection ID".to_string())?
        .to_string();
    let browser = hello
        .get("browser")
        .and_then(Value::as_str)
        .ok_or_else(|| "browser secret handshake has no browser".to_string())?
        .parse::<BrowserFamily>()?;
    let declared_process_id = hello
        .get("processId")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| "browser secret handshake has no process ID".to_string())?;
    #[cfg(windows)]
    validate_named_pipe_client(&pipe, declared_process_id)?;
    write_frame(
        &mut pipe,
        &serde_json::json!({
            "version": SECRET_PROTOCOL_VERSION,
            "type": "secret.ready",
        }),
    )?;

    let mut reader = pipe
        .try_clone()
        .map_err(|error| format!("failed to clone browser secret pipe: {error}"))?;
    let control = Arc::new(SecretConnectionControl::new(&pipe)?);
    control.register_reader_thread()?;
    let (outbound_tx, outbound_rx) = mpsc::sync_channel::<Value>(32);
    let generation = Uuid::new_v4();
    insert_connection_generation(
        &inner.connections,
        connection_id.clone(),
        SecretConnection {
            browser,
            sender: outbound_tx,
            connected_at: Instant::now(),
            generation,
            control: control.clone(),
        },
    );
    {
        let mut events = lock_unpoisoned(&inner.events);
        while events.len() >= MAX_QUEUED_EVENTS {
            events.pop_front();
        }
        events.push_back(BrowserSecretEvent {
            connection_id: connection_id.clone(),
            browser,
            event: "connectionReady".to_string(),
            payload: serde_json::json!({ "generation": generation.to_string() }),
        });
        inner.event_ready.notify_one();
    }
    let writer_inner = inner.clone();
    let writer_connection_id = connection_id.clone();
    let writer_control = control.clone();
    let writer = std::thread::spawn(move || {
        if writer_control.register_writer_thread().is_err() {
            close_connection_generation(
                &writer_inner,
                &writer_connection_id,
                browser,
                generation,
                &writer_control,
            );
            return;
        }
        run_connection_writer(
            pipe,
            outbound_rx,
            writer_inner,
            writer_connection_id,
            browser,
            generation,
            writer_control,
        )
    });

    // Keep the read error until after connection cleanup. A browser process can
    // disappear with a BrokenPipe/ConnectionReset error instead of a clean EOF;
    // returning through `?` here would leave stale connection state behind.
    let read_result = (|| {
        while let Some(message) = read_frame(&mut reader)? {
            match message.get("type").and_then(Value::as_str) {
                Some("secret.response") => {
                    let Some(id) = message.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let sender = lock_unpoisoned(&inner.pending).remove(id);
                    let Some(sender) = sender else { continue };
                    let response = if message.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(message
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("password browser request failed")
                            .to_string())
                    };
                    let _ = sender.send(response);
                }
                Some("secret.event") => {
                    let Some(event) = message.get("event").and_then(Value::as_str) else {
                        continue;
                    };
                    let mut events = lock_unpoisoned(&inner.events);
                    while events.len() >= MAX_QUEUED_EVENTS {
                        events.pop_front();
                    }
                    events.push_back(BrowserSecretEvent {
                        connection_id: connection_id.clone(),
                        browser,
                        event: event.to_string(),
                        payload: message.get("payload").cloned().unwrap_or(Value::Null),
                    });
                    inner.event_ready.notify_one();
                }
                _ => {}
            }
        }
        Ok::<(), String>(())
    })();

    close_connection_generation(&inner, &connection_id, browser, generation, &control);
    let _ = writer.join();
    read_result
}

fn run_connection_writer<W: Write>(
    mut writer: W,
    outbound_rx: mpsc::Receiver<Value>,
    inner: Arc<SecretInner>,
    connection_id: String,
    browser: BrowserFamily,
    generation: Uuid,
    control: Arc<SecretConnectionControl>,
) {
    while !control.is_retired() {
        let message = match outbound_rx.recv_timeout(CONNECTION_STOP_POLL_INTERVAL) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if control.is_retired() {
            break;
        }
        if write_frame(&mut writer, &message).is_err() {
            close_connection_generation(&inner, &connection_id, browser, generation, &control);
            break;
        }
    }
}

fn close_connection_generation(
    inner: &SecretInner,
    connection_id: &str,
    browser: BrowserFamily,
    generation: Uuid,
    control: &SecretConnectionControl,
) -> bool {
    control.retire();
    if !remove_connection_generation(&inner.connections, connection_id, generation) {
        return false;
    }
    let mut events = lock_unpoisoned(&inner.events);
    while events.len() >= MAX_QUEUED_EVENTS {
        events.pop_front();
    }
    events.push_back(BrowserSecretEvent {
        connection_id: connection_id.to_string(),
        browser,
        event: "connectionClosed".to_string(),
        payload: serde_json::json!({ "generation": generation.to_string() }),
    });
    inner.event_ready.notify_one();
    true
}

fn remove_connection_generation(
    connections: &Mutex<HashMap<String, SecretConnection>>,
    connection_id: &str,
    generation: Uuid,
) -> bool {
    let mut connections = lock_unpoisoned(connections);
    if connections
        .get(connection_id)
        .is_some_and(|connection| connection.generation == generation)
    {
        connections.remove(connection_id);
        true
    } else {
        false
    }
}

fn insert_connection_generation(
    connections: &Mutex<HashMap<String, SecretConnection>>,
    connection_id: String,
    connection: SecretConnection,
) {
    let replaced = lock_unpoisoned(connections).insert(connection_id, connection);
    if let Some(replaced) = replaced {
        replaced.control.retire();
    }
}

#[cfg(windows)]
fn validate_named_pipe_client(
    pipe: &std::fs::File,
    declared_process_id: u32,
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut actual_process_id = 0_u32;
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle() as _, &mut actual_process_id) }
        == 0
        || actual_process_id == 0
        || actual_process_id != declared_process_id
    {
        return Err("browser secret client process ID was rejected".to_string());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, actual_process_id) };
    if process.is_null() {
        return Err("browser secret client process cannot be inspected".to_string());
    }
    let mut image_path = vec![0_u16; 32_768];
    let mut image_path_len = image_path.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(process, 0, image_path.as_mut_ptr(), &mut image_path_len)
    } != 0;
    unsafe {
        CloseHandle(process);
    }
    if !queried {
        return Err("browser secret client executable cannot be inspected".to_string());
    }
    let client_path = PathBuf::from(String::from_utf16_lossy(
        &image_path[..image_path_len as usize],
    ));
    let app_path = std::env::current_exe()
        .map_err(|error| format!("PetalDesk executable path is unavailable: {error}"))?;
    if !is_expected_native_host_path(&app_path, &client_path) {
        return Err("browser secret client executable was rejected".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn is_expected_native_host_path(app_path: &Path, client_path: &Path) -> bool {
    let client_name = client_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let app_parent = app_path.parent().map(|path| path.to_string_lossy());
    let client_parent = client_path.parent().map(|path| path.to_string_lossy());
    client_name.eq_ignore_ascii_case("petaldesk-browser-host.exe")
        && app_parent
            .zip(client_parent)
            .is_some_and(|(app, client)| app.eq_ignore_ascii_case(&client))
}

#[cfg(windows)]
fn run_pipe_server(pipe_name: &str, inner: Arc<SecretInner>) {
    loop {
        match create_and_connect_pipe(pipe_name) {
            Ok(pipe) => {
                let connection_inner = inner.clone();
                let _ = std::thread::Builder::new()
                    .name("petaldesk-password-connection".to_string())
                    .spawn(move || {
                        let _ = handle_pipe_connection(pipe, connection_inner);
                    });
            }
            Err(error) => {
                eprintln!("密码浏览器安全通道暂时不可用: {error}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

#[cfg(windows)]
fn create_and_connect_pipe(pipe_name: &str) -> Result<std::fs::File, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let name = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let security = CurrentUserSecurity::new()?;
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            5_000,
            &security.attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "CreateNamedPipeW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
        || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
    if !connected {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(format!(
            "ConnectNamedPipe failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle as _) })
}

#[cfg(windows)]
struct CurrentUserSecurity {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
impl CurrentUserSecurity {
    fn new() -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        };
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(format!(
                "OpenProcessToken failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut required = 0_u32;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required);
        }
        if required == 0 {
            unsafe { CloseHandle(token) };
            return Err(format!("GetTokenInformation failed: {}", unsafe {
                GetLastError()
            }));
        }
        let mut buffer = vec![0_u8; required as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == 0
        {
            unsafe { CloseHandle(token) };
            return Err(format!(
                "GetTokenInformation failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let mut sid_text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
            unsafe { CloseHandle(token) };
            return Err(format!(
                "ConvertSidToStringSidW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let sid_length = (0..)
            .take_while(|index| unsafe { *sid_text.add(*index) } != 0)
            .count();
        let sid_string =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid_text, sid_length) });
        unsafe {
            LocalFree(sid_text.cast());
            CloseHandle(token);
        }
        let sddl = format!("D:P(A;;GA;;;{sid_string})(A;;GA;;;SY)");
        let sddl = std::ffi::OsStr::new(&sddl)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "creating pipe ACL failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            descriptor,
            attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                    as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }
}

#[cfg(windows)]
impl Drop for CurrentUserSecurity {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(self.descriptor.cast());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_inner() -> Arc<SecretInner> {
        Arc::new(SecretInner {
            token: Mutex::new("test-token".to_string()),
            endpoint_path: PathBuf::new(),
            connections: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            event_ready: Condvar::new(),
        })
    }

    fn insert_test_connection(
        inner: &SecretInner,
        connection_id: &str,
        sender: SyncSender<Value>,
        generation: Uuid,
    ) -> Arc<SecretConnectionControl> {
        let control = Arc::new(SecretConnectionControl::detached());
        insert_connection_generation(
            &inner.connections,
            connection_id.to_string(),
            SecretConnection {
                browser: BrowserFamily::Firefox,
                sender,
                connected_at: Instant::now(),
                generation,
                control: control.clone(),
            },
        );
        control
    }

    #[test]
    fn password_channel_rejects_non_password_commands() {
        assert!(validate_command("password.open").is_ok());
        assert!(validate_command("password.provideCredentials").is_ok());
        assert!(validate_command("start").is_err());
        assert!(validate_command("password../escape").is_err());
    }

    #[test]
    fn secret_frames_round_trip_without_files() {
        let value = serde_json::json!({
            "type": "secret.command",
            "payload": { "password": "memory-only" }
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).unwrap();
        let decoded = read_frame(&mut std::io::Cursor::new(bytes))
            .unwrap()
            .unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn stale_duplicate_connection_cannot_remove_its_replacement() {
        let connections = Mutex::new(HashMap::new());
        let (sender, _receiver) = mpsc::sync_channel(1);
        let old_generation = Uuid::new_v4();
        let new_generation = Uuid::new_v4();
        let control = Arc::new(SecretConnectionControl::detached());
        lock_unpoisoned(&connections).insert(
            "same-id".to_string(),
            SecretConnection {
                browser: BrowserFamily::Firefox,
                sender,
                connected_at: Instant::now(),
                generation: new_generation,
                control,
            },
        );
        remove_connection_generation(&connections, "same-id", old_generation);
        assert!(lock_unpoisoned(&connections).contains_key("same-id"));
        remove_connection_generation(&connections, "same-id", new_generation);
        assert!(!lock_unpoisoned(&connections).contains_key("same-id"));
    }

    #[test]
    fn replacement_connection_retires_displaced_generation() {
        let inner = test_inner();
        let (old_sender, _old_receiver) = mpsc::sync_channel(1);
        let old_control = insert_test_connection(&inner, "same-id", old_sender, Uuid::new_v4());
        let (new_sender, _new_receiver) = mpsc::sync_channel(1);
        let new_generation = Uuid::new_v4();
        let new_control = insert_test_connection(&inner, "same-id", new_sender, new_generation);

        assert!(old_control.is_retired());
        assert!(!new_control.is_retired());
        let connections = lock_unpoisoned(&inner.connections);
        let current = connections.get("same-id").unwrap();
        assert_eq!(current.generation, new_generation);
        assert!(Arc::ptr_eq(&current.control, &new_control));
    }

    #[test]
    fn full_request_queue_fails_fast_and_closes_connection() {
        let inner = test_inner();
        let bridge = BrowserSecretBridge {
            inner: inner.clone(),
        };
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender
            .send(serde_json::json!({ "occupied": true }))
            .unwrap();
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "busy", sender, generation);

        let error = bridge
            .request_connection(
                "busy",
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert_eq!(error, "password browser connection is busy");
        assert!(lock_unpoisoned(&inner.pending).is_empty());
        assert!(!lock_unpoisoned(&inner.connections).contains_key("busy"));
        assert!(control.is_retired());
        let event = bridge.receive_event(Duration::ZERO).unwrap();
        assert_eq!(event.connection_id, "busy");
        assert_eq!(event.event, "connectionClosed");
        assert_eq!(event.payload["generation"], generation.to_string());
    }

    #[test]
    fn disconnected_request_sender_closes_connection() {
        let inner = test_inner();
        let bridge = BrowserSecretBridge {
            inner: inner.clone(),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "disconnected", sender, generation);

        let error = bridge
            .request_connection(
                "disconnected",
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert_eq!(error, "password browser connection closed");
        assert!(lock_unpoisoned(&inner.pending).is_empty());
        assert!(!lock_unpoisoned(&inner.connections).contains_key("disconnected"));
        assert!(control.is_retired());
        let event = bridge.receive_event(Duration::ZERO).unwrap();
        assert_eq!(event.connection_id, "disconnected");
        assert_eq!(event.event, "connectionClosed");
        assert_eq!(event.payload["generation"], generation.to_string());
    }

    #[test]
    fn timed_out_request_retires_connection_and_clears_pending_response() {
        let inner = test_inner();
        let bridge = BrowserSecretBridge {
            inner: inner.clone(),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "timed-out", sender, generation);

        let error = bridge
            .request_connection(
                "timed-out",
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::ZERO,
            )
            .unwrap_err();

        assert_eq!(error, "password browser request timed out after 0 ms");
        assert!(control.is_retired());
        assert!(lock_unpoisoned(&inner.pending).is_empty());
        assert!(!lock_unpoisoned(&inner.connections).contains_key("timed-out"));
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(matches!(
            receiver.recv_timeout(Duration::ZERO),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        let event = bridge.receive_event(Duration::ZERO).unwrap();
        assert_eq!(event.connection_id, "timed-out");
        assert_eq!(event.event, "connectionClosed");
    }

    #[test]
    fn stale_close_does_not_remove_replacement_or_emit_duplicate_event() {
        let inner = test_inner();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let old_generation = Uuid::new_v4();
        let new_generation = Uuid::new_v4();
        let replacement_control = insert_test_connection(&inner, "same-id", sender, new_generation);
        let old_control = SecretConnectionControl::detached();

        assert!(!close_connection_generation(
            &inner,
            "same-id",
            BrowserFamily::Firefox,
            old_generation,
            &old_control,
        ));
        assert!(old_control.is_retired());
        assert!(!replacement_control.is_retired());
        assert!(lock_unpoisoned(&inner.connections).contains_key("same-id"));
        assert!(lock_unpoisoned(&inner.events).is_empty());

        assert!(close_connection_generation(
            &inner,
            "same-id",
            BrowserFamily::Firefox,
            new_generation,
            &replacement_control,
        ));
        assert!(!close_connection_generation(
            &inner,
            "same-id",
            BrowserFamily::Firefox,
            new_generation,
            &replacement_control,
        ));
        assert_eq!(lock_unpoisoned(&inner.events).len(), 1);
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test writer failed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_failure_immediately_closes_connection() {
        let inner = test_inner();
        let (sender, receiver) = mpsc::sync_channel(1);
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "writer-failed", sender.clone(), generation);
        sender
            .send(serde_json::json!({ "type": "secret.command" }))
            .unwrap();
        drop(sender);

        run_connection_writer(
            FailingWriter,
            receiver,
            inner.clone(),
            "writer-failed".to_string(),
            BrowserFamily::Firefox,
            generation,
            control.clone(),
        );

        assert!(!lock_unpoisoned(&inner.connections).contains_key("writer-failed"));
        assert!(control.is_retired());
        let events = lock_unpoisoned(&inner.events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].connection_id, "writer-failed");
        assert_eq!(events[0].event, "connectionClosed");
        assert_eq!(events[0].payload["generation"], generation.to_string());
    }

    struct TrackingWriter(Arc<AtomicBool>);

    impl Write for TrackingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.store(true, Ordering::Release);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn retired_writer_exits_without_waiting_for_sender_disconnect() {
        let inner = test_inner();
        let (sender, receiver) = mpsc::sync_channel(1);
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "retired", sender.clone(), generation);
        let writer_inner = inner.clone();
        let writer_control = control.clone();
        let writer = std::thread::spawn(move || {
            run_connection_writer(
                std::io::sink(),
                receiver,
                writer_inner,
                "retired".to_string(),
                BrowserFamily::Firefox,
                generation,
                writer_control,
            );
        });

        assert!(close_connection_generation(
            &inner,
            "retired",
            BrowserFamily::Firefox,
            generation,
            &control,
        ));
        for _ in 0..20 {
            if writer.is_finished() {
                break;
            }
            std::thread::sleep(CONNECTION_STOP_POLL_INTERVAL);
        }
        assert!(
            writer.is_finished(),
            "retired writer did not release its queue"
        );
        writer.join().unwrap();
        assert!(sender.send(Value::Null).is_err());
    }

    #[test]
    fn retired_writer_drops_queued_secret_without_writing_it() {
        let inner = test_inner();
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(serde_json::json!({ "password": "queued-secret" }))
            .unwrap();
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "queued", sender.clone(), generation);
        assert!(close_connection_generation(
            &inner,
            "queued",
            BrowserFamily::Firefox,
            generation,
            &control,
        ));
        let wrote = Arc::new(AtomicBool::new(false));

        run_connection_writer(
            TrackingWriter(wrote.clone()),
            receiver,
            inner,
            "queued".to_string(),
            BrowserFamily::Firefox,
            generation,
            control,
        );

        assert!(!wrote.load(Ordering::Acquire));
        assert!(sender.send(Value::Null).is_err());
    }

    #[cfg(windows)]
    fn connected_test_named_pipe() -> (std::fs::File, std::fs::File) {
        let pipe_name = format!(r"\\.\pipe\PetalDesk-password-test-{}", Uuid::new_v4());
        let server_name = pipe_name.clone();
        let (server_tx, server_rx) = mpsc::sync_channel(1);
        let server_thread = std::thread::spawn(move || {
            let _ = server_tx.send(create_and_connect_pipe(&server_name));
        });
        let mut client = None;
        for _ in 0..500 {
            match OpenOptions::new().read(true).write(true).open(&pipe_name) {
                Ok(pipe) => {
                    client = Some(pipe);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let client = client.expect("test client did not connect to named pipe");
        let server = server_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("test named pipe server did not finish connecting")
            .expect("test named pipe server failed");
        server_thread.join().unwrap();
        (server, client)
    }

    #[cfg(windows)]
    #[test]
    fn retiring_real_named_pipe_cancels_blocked_generation_reader() {
        let (server, client) = connected_test_named_pipe();

        let control = Arc::new(SecretConnectionControl::new(&server).unwrap());
        let mut reader = server.try_clone().unwrap();
        let reader_control = control.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let reader_thread = std::thread::spawn(move || {
            reader_control.register_reader_thread().unwrap();
            started_tx.send(()).unwrap();
            let result = read_frame(&mut reader);
            let _ = finished_tx.send(result);
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(control.retire());
        let result = finished_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("retired named pipe reader remained blocked");
        assert!(result.is_err() || matches!(result, Ok(None)));
        reader_thread.join().unwrap();
        drop(client);
    }

    #[cfg(windows)]
    struct NotifyingPipeWriter {
        pipe: std::fs::File,
        started: Option<SyncSender<()>>,
    }

    #[cfg(windows)]
    impl Write for NotifyingPipeWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            self.pipe.write(buffer)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.pipe.flush()
        }
    }

    #[cfg(windows)]
    #[test]
    fn retiring_real_named_pipe_cancels_blocked_writer_and_drops_secret_queue() {
        let (server, client) = connected_test_named_pipe();
        let inner = test_inner();
        let control = Arc::new(SecretConnectionControl::new(&server).unwrap());
        let generation = Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(32);
        sender
            .send(serde_json::json!({ "password": "x".repeat(512 * 1024) }))
            .unwrap();
        sender
            .send(serde_json::json!({ "password": "queued-secret" }))
            .unwrap();
        insert_connection_generation(
            &inner.connections,
            "blocked-writer".to_string(),
            SecretConnection {
                browser: BrowserFamily::Firefox,
                sender: sender.clone(),
                connected_at: Instant::now(),
                generation,
                control: control.clone(),
            },
        );
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let writer_inner = inner.clone();
        let writer_control = control.clone();
        let writer = std::thread::spawn(move || {
            writer_control.register_writer_thread().unwrap();
            run_connection_writer(
                NotifyingPipeWriter {
                    pipe: server,
                    started: Some(started_tx),
                },
                receiver,
                writer_inner,
                "blocked-writer".to_string(),
                BrowserFamily::Firefox,
                generation,
                writer_control,
            );
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let _ = close_connection_generation(
            &inner,
            "blocked-writer",
            BrowserFamily::Firefox,
            generation,
            &control,
        );
        for _ in 0..100 {
            if writer.is_finished() {
                break;
            }
            std::thread::sleep(CONNECTION_STOP_POLL_INTERVAL);
        }
        assert!(
            writer.is_finished(),
            "blocked named pipe writer did not stop"
        );
        writer.join().unwrap();
        assert!(control.is_retired());
        assert!(!lock_unpoisoned(&inner.connections).contains_key("blocked-writer"));
        let events = lock_unpoisoned(&inner.events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].connection_id, "blocked-writer");
        assert_eq!(events[0].event, "connectionClosed");
        drop(events);
        assert!(sender.send(Value::Null).is_err());
        drop(client);
    }

    #[cfg(unix)]
    #[test]
    fn malformed_frame_still_emits_connection_closed_event() {
        use std::os::fd::{FromRawFd, IntoRawFd};
        use std::os::unix::net::UnixStream;

        let inner = test_inner();
        let (mut client, server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &serde_json::json!({
                "type": "secret.hello",
                "version": SECRET_PROTOCOL_VERSION,
                "token": "test-token",
                "connectionId": "broken-client",
                "browser": "firefox",
                "processId": std::process::id(),
            }),
        )
        .unwrap();
        // A frame length larger than the protocol limit makes read_frame return
        // an error, matching a reset/aborted browser connection rather than EOF.
        client.write_all(&(u32::MAX).to_le_bytes()).unwrap();

        let server_file = unsafe { std::fs::File::from_raw_fd(server.into_raw_fd()) };
        assert!(handle_pipe_connection(server_file, inner.clone()).is_err());
        assert!(!lock_unpoisoned(&inner.connections).contains_key("broken-client"));
        let events = lock_unpoisoned(&inner.events);
        assert!(events.iter().any(|event| event.event == "connectionReady"));
        assert!(events.iter().any(|event| event.event == "connectionClosed"));
    }
}

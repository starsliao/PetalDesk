use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
#[cfg(windows)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
#[cfg(windows)]
use std::thread::JoinHandle;
#[cfg(windows)]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const PROTOCOL_VERSION: u32 = 1;
const MAX_NATIVE_MESSAGE_BYTES: usize = 1024 * 1024;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(windows)]
const SECRET_ENDPOINT_MAX_FUTURE: Duration = Duration::from_secs(10 * 60);
#[cfg(windows)]
const SECRET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const SECRET_IO_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(windows)]
const SECRET_IO_STOP_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const SECRET_THREAD_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(windows)]
const HOST_DIAG_MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    protocol_version: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    browser: Option<String>,
    #[serde(default)]
    extension_version: Option<String>,
    #[serde(default)]
    extension_id: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
}

#[cfg(windows)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretEndpoint {
    version: u32,
    pipe_name: String,
    token: String,
    process_id: u32,
    expires_at_unix_ms: u128,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetadata<'a> {
    version: u32,
    connection_id: &'a str,
    browser: &'a str,
    extension_version: &'a str,
    extension_id: &'a str,
    capabilities: &'a [String],
    process_id: u32,
    last_seen_unix_ms: u128,
}

pub fn run() -> Result<(), String> {
    run_with_streams(io::stdin(), io::stdout())
}

fn run_with_streams<R, W>(mut input: R, mut output: W) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    let hello =
        read_native_message(&mut input)?.ok_or_else(|| "浏览器扩展未发送握手消息".to_string())?;
    if hello.kind != "extension.ready" {
        return Err("浏览器扩展的第一条消息必须是 extension.ready".to_string());
    }
    if hello.protocol_version != Some(PROTOCOL_VERSION) {
        return Err(format!(
            "浏览器扩展协议版本不兼容: {}",
            hello
                .protocol_version
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string())
        ));
    }
    let browser = normalize_browser(
        hello
            .browser
            .as_deref()
            .ok_or_else(|| "浏览器扩展握手缺少 browser".to_string())?,
    )?;
    let extension_version = hello.extension_version.as_deref().unwrap_or("unknown");
    let extension_id = hello.extension_id.as_deref().unwrap_or("unknown");
    let connection_id = Uuid::new_v4().to_string();
    let paths = BridgePaths::create(&connection_id)?;
    write_session(
        &paths.session,
        &connection_id,
        browser,
        extension_version,
        extension_id,
        &hello.capabilities,
    )?;
    #[cfg(windows)]
    {
        if let Ok(path) = host_diag_path() {
            let _ = trim_host_diag_log(&path);
        }
        log_host_diag("host-started", &format!("connectionId={connection_id}"));
    }

    let (incoming_tx, incoming_rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        match read_native_message(&mut input) {
            Ok(Some(message)) => {
                if incoming_tx.send(Ok(message)).is_err() {
                    break;
                }
            }
            Ok(None) => {
                let _ = incoming_tx.send(Err("浏览器扩展已断开".to_string()));
                break;
            }
            Err(error) => {
                let _ = incoming_tx.send(Err(error));
                break;
            }
        }
    });

    let (secret_command_tx, secret_command_rx) = mpsc::channel::<Value>();
    let (secret_outbound_tx, secret_outbound_rx) = mpsc::sync_channel::<Value>(128);
    #[cfg(windows)]
    let secret_fatal = Arc::new(OnceLock::new());
    #[cfg(windows)]
    let secret_reconnect_requested = Arc::new(AtomicBool::new(false));
    #[cfg(windows)]
    start_secret_connector(
        connection_id.clone(),
        browser.to_string(),
        secret_command_tx,
        secret_outbound_rx,
        secret_fatal.clone(),
        secret_reconnect_requested.clone(),
    );
    #[cfg(not(windows))]
    {
        drop(secret_command_tx);
        drop(secret_outbound_rx);
    }

    let _cleanup = SessionCleanup(paths.clone());
    let mut last_heartbeat = SystemTime::now();
    let mut secret_request_ids = HashSet::new();
    let mut legacy_request_ids = HashSet::new();
    loop {
        #[cfg(windows)]
        if let Some(error) = secret_fatal_exit_error(&secret_fatal) {
            return Err(error);
        }
        while let Ok(message) = incoming_rx.try_recv() {
            let message = message?;
            if message.kind == "extension.event" {
                let Some(event) = message.event else {
                    continue;
                };
                #[cfg(windows)]
                let detail = format!("event={event}");
                let send_result = secret_outbound_tx.try_send(serde_json::json!({
                    "version": PROTOCOL_VERSION,
                    "type": "secret.event",
                    "event": event,
                    "payload": message.payload,
                    "queuedAtUnixMs": unix_time_ms(),
                }));
                #[cfg(windows)]
                note_secret_outbound_failure(send_result, &detail, &secret_reconnect_requested);
                #[cfg(not(windows))]
                let _ = send_result;
                continue;
            }
            if message.kind != "extension.response" {
                continue;
            }
            let Some(id) = message.id.as_deref() else {
                continue;
            };
            if !is_safe_identifier(id) {
                continue;
            }
            if secret_request_ids.remove(id) {
                #[cfg(windows)]
                let detail = format!("responseId={}", &id[..id.len().min(8)]);
                let send_result = secret_outbound_tx.try_send(serde_json::json!({
                    "version": PROTOCOL_VERSION,
                    "type": "secret.response",
                    "id": id,
                    "ok": message.ok.unwrap_or(false),
                    "result": message.result,
                    "error": message.error,
                    "queuedAtUnixMs": unix_time_ms(),
                }));
                #[cfg(windows)]
                note_secret_outbound_failure(send_result, &detail, &secret_reconnect_requested);
                #[cfg(not(windows))]
                let _ = send_result;
                continue;
            }
            if !legacy_request_ids.remove(id) {
                continue;
            }
            let response = serde_json::json!({
                "version": PROTOCOL_VERSION,
                "id": id,
                "ok": message.ok.unwrap_or(false),
                "result": message.result,
                "error": message.error,
            });
            atomic_write_json(&paths.responses.join(format!("{id}.json")), &response)?;
        }

        while let Ok(command) = secret_command_rx.try_recv() {
            if command.get("type").and_then(Value::as_str) == Some("secret.lifecycle") {
                write_native_message(
                    &mut output,
                    &serde_json::json!({
                        "version": PROTOCOL_VERSION,
                        "type": "extension.event",
                        "event": command.get("event").and_then(Value::as_str),
                        "payload": command.get("payload").cloned().unwrap_or(Value::Null),
                    }),
                )?;
                continue;
            }
            let Some(id) = command.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_safe_identifier(id) {
                continue;
            }
            secret_request_ids.insert(id.to_string());
            write_native_message(&mut output, &command)?;
        }

        for command_path in pending_commands(&paths.commands)? {
            let bytes =
                fs::read(&command_path).map_err(|error| format!("读取浏览器指令失败: {error}"))?;
            let command: Value = serde_json::from_slice(&bytes)
                .map_err(|error| format!("浏览器指令格式无效: {error}"))?;
            if is_password_spool_command(&command) {
                fs::remove_file(&command_path)
                    .map_err(|error| format!("清理被拒绝的密码指令失败: {error}"))?;
                continue;
            }
            let Some(id) = command.get("id").and_then(Value::as_str) else {
                fs::remove_file(&command_path)
                    .map_err(|error| format!("清理无效浏览器指令失败: {error}"))?;
                continue;
            };
            if !is_safe_identifier(id) {
                fs::remove_file(&command_path)
                    .map_err(|error| format!("清理无效浏览器指令失败: {error}"))?;
                continue;
            }
            write_native_message(&mut output, &command)?;
            legacy_request_ids.insert(id.to_string());
            fs::remove_file(&command_path)
                .map_err(|error| format!("清理已发送浏览器指令失败: {error}"))?;
        }

        if last_heartbeat.elapsed().unwrap_or(HEARTBEAT_INTERVAL) >= HEARTBEAT_INTERVAL {
            write_session(
                &paths.session,
                &connection_id,
                browser,
                extension_version,
                extension_id,
                &hello.capabilities,
            )?;
            last_heartbeat = SystemTime::now();
        }
        std::thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

fn normalize_browser(value: &str) -> Result<&'static str, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "chrome" => Ok("chrome"),
        "edge" => Ok("edge"),
        "firefox" => Ok("firefox"),
        _ => Err(format!("不支持的浏览器类型: {value}")),
    }
}

fn bridge_root() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .ok_or_else(|| "无法确定当前用户的本地数据目录".to_string())
        .map(|root| root.join("PetalDesk").join("browser-bridge"))
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(all(windows, test))]
static TEST_DIAG_LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 向浏览器宿主诊断日志追加一行 JSON。
///
/// 该文件用于排查"扩展在、Host 在但密码通道已死"的假健康问题，只允许写
/// 事件名和短标识符：严禁记录 token、用户名、密码或任何凭据 payload。
/// 日志写不进去时静默忽略，诊断绝不能让桥接失败。
#[cfg(windows)]
fn log_host_diag(event: &str, detail: &str) {
    let Ok(path) = host_diag_path() else {
        return;
    };
    let _ = append_host_diag_line(&path, event, detail);
}

#[cfg(windows)]
fn host_diag_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(path) = TEST_DIAG_LOG_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(path);
    }
    bridge_root().map(|root| root.join("host-diagnostics.log"))
}

#[cfg(windows)]
fn append_host_diag_line(path: &Path, event: &str, detail: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建浏览器宿主诊断目录失败: {error}"))?;
    }
    let line = serde_json::json!({
        "ts": unix_time_ms(),
        "event": event,
        "detail": detail,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("打开浏览器宿主诊断日志失败: {error}"))?;
    writeln!(file, "{line}").map_err(|error| format!("写入浏览器宿主诊断日志失败: {error}"))
}

/// 进程启动时调用：日志超过 256KB 时截断，只保留尾部（按换行对齐，避免
/// 留下半行 JSON）。
#[cfg(windows)]
fn trim_host_diag_log(path: &Path) -> Result<(), String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取浏览器宿主诊断日志失败: {error}")),
    };
    if metadata.len() <= HOST_DIAG_MAX_BYTES {
        return Ok(());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("读取浏览器宿主诊断日志失败: {error}"))?;
    let tail = &bytes[bytes
        .len()
        .saturating_sub((HOST_DIAG_MAX_BYTES / 2) as usize)..];
    let start = tail
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    fs::write(path, &tail[start..])
        .map_err(|error| format!("截断浏览器宿主诊断日志失败: {error}"))
}

#[derive(Clone)]
struct BridgePaths {
    session: PathBuf,
    commands: PathBuf,
    responses: PathBuf,
}

impl BridgePaths {
    fn create(connection_id: &str) -> Result<Self, String> {
        let root = bridge_root()?;
        let sessions = root.join("sessions");
        let commands = root.join("commands").join(connection_id);
        let responses = root.join("responses").join(connection_id);
        fs::create_dir_all(&sessions)
            .and_then(|_| fs::create_dir_all(&commands))
            .and_then(|_| fs::create_dir_all(&responses))
            .map_err(|error| format!("创建浏览器桥接目录失败: {error}"))?;
        Ok(Self {
            session: sessions.join(format!("{connection_id}.json")),
            commands,
            responses,
        })
    }
}

struct SessionCleanup(BridgePaths);

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0.session);
        let _ = fs::remove_dir_all(&self.0.commands);
        let _ = fs::remove_dir_all(&self.0.responses);
    }
}

fn write_session(
    path: &Path,
    connection_id: &str,
    browser: &str,
    extension_version: &str,
    extension_id: &str,
    capabilities: &[String],
) -> Result<(), String> {
    atomic_write_json(
        path,
        &SessionMetadata {
            version: PROTOCOL_VERSION,
            connection_id,
            browser,
            extension_version,
            extension_id,
            capabilities,
            process_id: std::process::id(),
            last_seen_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        },
    )
}

fn pending_commands(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut commands = fs::read_dir(directory)
        .map_err(|error| format!("读取浏览器指令目录失败: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    commands.sort();
    Ok(commands)
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_password_spool_command(command: &Value) -> bool {
    command
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("password."))
        || command.get("type").and_then(Value::as_str) == Some("secret.command")
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "浏览器桥接文件没有父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建浏览器桥接目录失败: {error}"))?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes =
        serde_json::to_vec(value).map_err(|error| format!("序列化浏览器桥接消息失败: {error}"))?;
    fs::write(&temporary, bytes).map_err(|error| format!("写入浏览器桥接消息失败: {error}"))?;
    if let Err(error) = replace_bridge_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        Err(format!("提交浏览器桥接消息失败: {error}"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn replace_bridge_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_bridge_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn read_native_message<R: Read>(reader: &mut R) -> Result<Option<ExtensionMessage>, String> {
    let mut length = [0_u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("读取浏览器扩展消息长度失败: {error}")),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_NATIVE_MESSAGE_BYTES {
        return Err(format!("浏览器扩展消息长度无效: {length}"));
    }
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("读取浏览器扩展消息失败: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("浏览器扩展消息不是有效 JSON: {error}"))
}

fn write_native_message<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| format!("序列化浏览器扩展消息失败: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_NATIVE_MESSAGE_BYTES {
        return Err(format!("发送给浏览器扩展的消息长度无效: {}", bytes.len()));
    }
    let length =
        u32::try_from(bytes.len()).map_err(|_| "发送给浏览器扩展的消息过大".to_string())?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("发送浏览器扩展消息失败: {error}"))
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretReaderExit {
    PipeClosed,
    CommandChannelClosed,
    Stopped,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretWriterExit {
    PipeClosed,
    OutboundChannelClosed,
    Stopped,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretIoExit {
    Reader(SecretReaderExit),
    Writer(SecretWriterExit),
}

#[cfg(windows)]
impl SecretIoExit {
    fn stops_connector(self) -> bool {
        matches!(
            self,
            Self::Reader(SecretReaderExit::CommandChannelClosed)
                | Self::Writer(SecretWriterExit::OutboundChannelClosed)
        )
    }

    fn disconnect_reason(self) -> &'static str {
        match self {
            Self::Reader(SecretReaderExit::CommandChannelClosed)
            | Self::Writer(SecretWriterExit::OutboundChannelClosed) => "secret-channel-closed",
            Self::Reader(SecretReaderExit::Stopped) | Self::Writer(SecretWriterExit::Stopped) => {
                "secret-pipe-stopped"
            }
            Self::Reader(SecretReaderExit::PipeClosed)
            | Self::Writer(SecretWriterExit::PipeClosed) => "secret-pipe-closed",
        }
    }
}

#[cfg(windows)]
fn run_secret_reader<R: Read>(
    reader: &mut R,
    command_sender: &mpsc::Sender<Value>,
    stop: &AtomicBool,
) -> SecretReaderExit {
    loop {
        if stop.load(Ordering::Acquire) {
            return SecretReaderExit::Stopped;
        }
        match read_secret_frame(reader) {
            Ok(Some(command))
                if command.get("type").and_then(Value::as_str) == Some("secret.command") =>
            {
                if stop.load(Ordering::Acquire) {
                    return SecretReaderExit::Stopped;
                }
                if command_sender.send(command).is_err() {
                    return SecretReaderExit::CommandChannelClosed;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return SecretReaderExit::PipeClosed,
        }
    }
}

#[cfg(windows)]
fn run_secret_writer<W: Write>(
    writer: &mut W,
    outbound_receiver: &Mutex<mpsc::Receiver<Value>>,
    stop: &AtomicBool,
) -> SecretWriterExit {
    loop {
        if stop.load(Ordering::Acquire) {
            return SecretWriterExit::Stopped;
        }
        let received = outbound_receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv_timeout(SECRET_IO_POLL_INTERVAL);
        let message = match received {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return SecretWriterExit::OutboundChannelClosed;
            }
        };
        if stop.load(Ordering::Acquire) {
            return SecretWriterExit::Stopped;
        }
        let queued_at = message
            .get("queuedAtUnixMs")
            .and_then(Value::as_u64)
            .map(u128::from)
            .unwrap_or_else(unix_time_ms);
        if unix_time_ms().saturating_sub(queued_at) > 30_000 {
            continue;
        }
        if write_secret_frame(writer, &message).is_err() {
            return SecretWriterExit::PipeClosed;
        }
    }
}

#[cfg(windows)]
fn cancel_secret_thread_io<T>(thread: &JoinHandle<T>) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    if unsafe { CancelSynchronousIo(thread.as_raw_handle() as _) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
        Ok(())
    } else {
        Err(format!("failed to cancel browser secret pipe I/O: {error}"))
    }
}

#[cfg(windows)]
fn cancel_and_join_secret_thread<T>(
    thread: JoinHandle<T>,
    deadline: Instant,
) -> Option<std::thread::Result<T>> {
    loop {
        if thread.is_finished() {
            return Some(thread.join());
        }
        let _ = cancel_secret_thread_io(&thread);
        if Instant::now() >= deadline {
            return thread.is_finished().then(|| thread.join());
        }
        std::thread::sleep(SECRET_THREAD_POLL_INTERVAL);
    }
}

#[cfg(windows)]
fn join_finished_secret_worker(thread: &mut Option<JoinHandle<()>>) {
    if !thread.as_ref().is_some_and(JoinHandle::is_finished) {
        return;
    }
    if thread.take().is_some_and(|thread| thread.join().is_err()) {
        eprintln!("browser secret pipe worker panicked");
    }
}

#[cfg(windows)]
fn cancel_and_join_secret_workers(
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    deadline: Instant,
) -> bool {
    let mut reader = Some(reader);
    let mut writer = Some(writer);
    loop {
        join_finished_secret_worker(&mut reader);
        join_finished_secret_worker(&mut writer);
        if reader.is_none() && writer.is_none() {
            return true;
        }
        if let Some(thread) = reader.as_ref() {
            let _ = cancel_secret_thread_io(thread);
        }
        if let Some(thread) = writer.as_ref() {
            let _ = cancel_secret_thread_io(thread);
        }
        if Instant::now() >= deadline {
            join_finished_secret_worker(&mut reader);
            join_finished_secret_worker(&mut writer);
            return reader.is_none() && writer.is_none();
        }
        std::thread::sleep(SECRET_THREAD_POLL_INTERVAL);
    }
}

#[cfg(windows)]
enum SecretHandshakeWait<T> {
    Finished(std::thread::Result<T>),
    TimedOutStopped,
    TimedOutRunning,
}

#[cfg(windows)]
fn wait_secret_handshake_thread<T>(
    thread: JoinHandle<T>,
    timeout: Duration,
    stop_timeout: Duration,
) -> SecretHandshakeWait<T> {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(SECRET_THREAD_POLL_INTERVAL);
    }
    if thread.is_finished() {
        return SecretHandshakeWait::Finished(thread.join());
    }
    match cancel_and_join_secret_thread(thread, Instant::now() + stop_timeout) {
        Some(_) => SecretHandshakeWait::TimedOutStopped,
        None => SecretHandshakeWait::TimedOutRunning,
    }
}

#[cfg(windows)]
fn connect_secret_pipe(
    endpoint: SecretEndpoint,
    connection_id: String,
    browser: String,
) -> Result<std::fs::File, String> {
    let mut pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&endpoint.pipe_name)
        .map_err(|error| format!("failed to open browser secret pipe: {error}"))?;
    write_secret_frame(
        &mut pipe,
        &serde_json::json!({
            "version": PROTOCOL_VERSION,
            "type": "secret.hello",
            "token": endpoint.token,
            "connectionId": connection_id,
            "browser": browser,
            "processId": std::process::id(),
        }),
    )?;
    let ready = read_secret_frame(&mut pipe)?
        .ok_or_else(|| "browser secret pipe closed during handshake".to_string())?;
    if ready.get("type").and_then(Value::as_str) != Some("secret.ready") {
        return Err("browser secret handshake was rejected".to_string());
    }
    Ok(pipe)
}

/// 把 connector 上报的 fatal 转成主循环的退出错误。主循环每次迭代都会调用：
/// 一旦 connector 遇到不可恢复故障（线程卡死），Host 进程必须退出，让
/// Firefox 在扩展重连时重新拉起，而不是继续刷心跳伪装健康。
#[cfg(windows)]
fn secret_fatal_exit_error(fatal: &OnceLock<String>) -> Option<String> {
    fatal.get().map(|reason| {
        log_host_diag("fatal", reason);
        format!("密码浏览器安全通道发生不可恢复错误，宿主进程退出: {reason}")
    })
}

/// outbound 队列 try_send 失败（Full/Disconnected）时不再静默丢弃：记录诊断
/// （只写事件名或 request id 短码，绝不写 payload），并通知 connector 丢弃
/// 当前连接、明确重建。
#[cfg(windows)]
fn note_secret_outbound_failure(
    result: Result<(), mpsc::TrySendError<Value>>,
    detail: &str,
    reconnect_requested: &AtomicBool,
) {
    let queue = match result {
        Ok(()) => return,
        Err(mpsc::TrySendError::Full(_)) => "full",
        Err(mpsc::TrySendError::Disconnected(_)) => "disconnected",
    };
    log_host_diag(
        "outbound-send-failed",
        &format!("{detail} queue={queue}"),
    );
    reconnect_requested.store(true, Ordering::Release);
}

#[cfg(windows)]
fn start_secret_connector(
    connection_id: String,
    browser: String,
    command_sender: mpsc::Sender<Value>,
    outbound_receiver: mpsc::Receiver<Value>,
    fatal: Arc<OnceLock<String>>,
    reconnect_requested: Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("petaldesk-password-native-pipe".to_string())
        .spawn(move || {
            log_host_diag("connector-started", &connection_id);
            if command_sender
                .send(serde_json::json!({
                "type": "secret.lifecycle",
                "event": "secretDisconnected",
                "payload": { "reason": "secret-pipe-unavailable" },
                }))
                .is_err()
            {
                log_host_diag("connector-exit", "command-channel-closed");
                return;
            }
            let outbound_receiver = Arc::new(Mutex::new(outbound_receiver));
            // 桌面未运行时 endpoint 读取每秒失败一次且内容不变，去重后只记一次。
            let mut last_endpoint_error = String::new();
            // 握手失败每 500ms 重试一次，连续相同的失败只记一次。
            let mut last_handshake_failure = String::new();
            let mut note_handshake_failure = |detail: String| {
                if detail != last_handshake_failure {
                    log_host_diag("handshake-failed", &detail);
                    last_handshake_failure = detail;
                }
            };
            loop {
                let endpoint = match read_secret_endpoint() {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        if error != last_endpoint_error {
                            log_host_diag("endpoint-unavailable", &error);
                            last_endpoint_error = error;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };
                last_endpoint_error.clear();
                let handshake_connection_id = connection_id.clone();
                let handshake_browser = browser.clone();
                let handshake_thread = std::thread::Builder::new()
                    .name("petaldesk-password-native-handshake".to_string())
                    .spawn(move || {
                        connect_secret_pipe(endpoint, handshake_connection_id, handshake_browser)
                    });
                let handshake_thread = match handshake_thread {
                    Ok(thread) => thread,
                    Err(_) => {
                        note_handshake_failure("thread-unavailable".to_string());
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": { "reason": "secret-pipe-thread-unavailable" },
                        }));
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                let mut pipe = match wait_secret_handshake_thread(
                    handshake_thread,
                    SECRET_HANDSHAKE_TIMEOUT,
                    SECRET_IO_STOP_TIMEOUT,
                ) {
                    SecretHandshakeWait::Finished(Ok(Ok(pipe))) => pipe,
                    SecretHandshakeWait::Finished(Ok(Err(error))) => {
                        note_handshake_failure(error);
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    SecretHandshakeWait::Finished(Err(_)) => {
                        note_handshake_failure("thread-panicked".to_string());
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": { "reason": "secret-pipe-thread-unavailable" },
                        }));
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    SecretHandshakeWait::TimedOutStopped => {
                        note_handshake_failure("timeout".to_string());
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": { "reason": "secret-pipe-handshake-timeout" },
                        }));
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    SecretHandshakeWait::TimedOutRunning => {
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": { "reason": "secret-pipe-handshake-stuck" },
                        }));
                        let reason = "browser secret pipe handshake did not stop after timeout";
                        eprintln!("{reason}");
                        log_host_diag("connector-exit", reason);
                        let _ = fatal.set(reason.to_string());
                        return;
                    }
                };
                let mut reader = match pipe.try_clone() {
                    Ok(reader) => reader,
                    Err(error) => {
                        log_host_diag(
                            "connection-setup-failed",
                            &format!("pipe-clone: {error}"),
                        );
                        continue;
                    }
                };
                if command_sender
                    .send(serde_json::json!({
                        "type": "secret.lifecycle",
                        "event": "secretConnected",
                        "payload": {},
                    }))
                    .is_err()
                {
                    log_host_diag("connector-exit", "command-channel-closed");
                    return;
                }
                log_host_diag("handshake-succeeded", "");

                let stop = Arc::new(AtomicBool::new(false));
                let (io_ended_tx, io_ended_rx) = mpsc::channel();
                let reader_commands = command_sender.clone();
                let reader_stop = stop.clone();
                let reader_ended = io_ended_tx.clone();
                let reader_thread = std::thread::Builder::new()
                    .name("petaldesk-password-native-reader".to_string())
                    .spawn(move || {
                        let exit = run_secret_reader(&mut reader, &reader_commands, &reader_stop);
                        let _ = reader_ended.send(SecretIoExit::Reader(exit));
                    });
                let reader_thread = match reader_thread {
                    Ok(thread) => thread,
                    Err(_) => {
                        log_host_diag("worker-spawn-failed", "reader");
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": { "reason": "secret-pipe-thread-unavailable" },
                        }));
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };

                let writer_receiver = outbound_receiver.clone();
                let writer_stop = stop.clone();
                let writer_ended = io_ended_tx.clone();
                let writer_thread = std::thread::Builder::new()
                    .name("petaldesk-password-native-writer".to_string())
                    .spawn(move || {
                        let exit = run_secret_writer(&mut pipe, &writer_receiver, &writer_stop);
                        let _ = writer_ended.send(SecretIoExit::Writer(exit));
                    });
                let writer_thread = match writer_thread {
                    Ok(thread) => thread,
                    Err(_) => {
                        stop.store(true, Ordering::Release);
                        let deadline = Instant::now() + SECRET_IO_STOP_TIMEOUT;
                        let reader_stopped =
                            cancel_and_join_secret_thread(reader_thread, deadline).is_some();
                        let _ = command_sender.send(serde_json::json!({
                            "type": "secret.lifecycle",
                            "event": "secretDisconnected",
                            "payload": {
                                "reason": if reader_stopped {
                                    "secret-pipe-thread-unavailable"
                                } else {
                                    "secret-pipe-worker-stuck"
                                }
                            },
                        }));
                        if !reader_stopped {
                            let reason =
                                "browser secret pipe reader did not stop after writer spawn failure";
                            eprintln!("{reason}");
                            log_host_diag("connector-exit", reason);
                            let _ = fatal.set(reason.to_string());
                            return;
                        }
                        log_host_diag("worker-spawn-failed", "writer");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                drop(io_ended_tx);

                // 新连接开始消费 outbound 队列，旧连接期间积累的重建请求作废。
                reconnect_requested.store(false, Ordering::Release);
                let first_exit = loop {
                    match io_ended_rx.recv_timeout(SECRET_IO_POLL_INTERVAL) {
                        Ok(exit) => break exit,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            break SecretIoExit::Reader(SecretReaderExit::PipeClosed);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout)
                            if reconnect_requested.swap(false, Ordering::AcqRel) =>
                        {
                            // 主循环无法把响应/事件塞进 outbound 队列：明确丢弃
                            // 当前连接并重建，而不是让桌面端干等超时。
                            log_host_diag("reconnect-requested", "outbound queue send failed");
                            break SecretIoExit::Writer(SecretWriterExit::PipeClosed);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) if reader_thread.is_finished() => {
                            break SecretIoExit::Reader(SecretReaderExit::PipeClosed);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) if writer_thread.is_finished() => {
                            break SecretIoExit::Writer(SecretWriterExit::PipeClosed);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                };
                stop.store(true, Ordering::Release);
                let deadline = Instant::now() + SECRET_IO_STOP_TIMEOUT;
                let workers_stopped =
                    cancel_and_join_secret_workers(reader_thread, writer_thread, deadline);

                let mut stop_connector = first_exit.stops_connector();
                while let Ok(exit) = io_ended_rx.try_recv() {
                    stop_connector |= exit.stops_connector();
                }
                if !workers_stopped {
                    let _ = command_sender.send(serde_json::json!({
                        "type": "secret.lifecycle",
                        "event": "secretDisconnected",
                        "payload": { "reason": "secret-pipe-worker-stuck" },
                    }));
                    let reason = "browser secret pipe worker did not stop; reconnect was abandoned";
                    eprintln!("{reason}");
                    log_host_diag("connector-exit", reason);
                    let _ = fatal.set(reason.to_string());
                    return;
                }
                log_host_diag(
                    "worker-exit",
                    &format!(
                        "{}: {}",
                        match first_exit {
                            SecretIoExit::Reader(_) => "reader",
                            SecretIoExit::Writer(_) => "writer",
                        },
                        first_exit.disconnect_reason()
                    ),
                );
                let _ = command_sender.send(serde_json::json!({
                    "type": "secret.lifecycle",
                    "event": "secretDisconnected",
                    "payload": { "reason": first_exit.disconnect_reason() },
                }));
                if stop_connector {
                    log_host_diag("connector-exit", first_exit.disconnect_reason());
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
}

#[cfg(windows)]
fn read_secret_endpoint() -> Result<SecretEndpoint, String> {
    let path = bridge_root()?.join("secret-endpoint.json");
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("browser secret endpoint is unavailable: {error}"))?;
    if metadata.len() == 0 || metadata.len() > 64 * 1024 {
        return Err("browser secret endpoint length is invalid".to_string());
    }
    let endpoint: SecretEndpoint = serde_json::from_slice(
        &fs::read(path)
            .map_err(|error| format!("failed to read browser secret endpoint: {error}"))?,
    )
    .map_err(|error| format!("browser secret endpoint is invalid: {error}"))?;
    if !secret_endpoint_is_valid_at(
        &endpoint,
        is_process_alive(endpoint.process_id),
        unix_time_ms(),
    ) {
        return Err("browser secret endpoint was rejected".to_string());
    }
    Ok(endpoint)
}

#[cfg(windows)]
fn secret_endpoint_is_valid_at(
    endpoint: &SecretEndpoint,
    process_alive: bool,
    now_unix_ms: u128,
) -> bool {
    endpoint.version == PROTOCOL_VERSION
        && endpoint.pipe_name.len() <= 256
        && endpoint
            .pipe_name
            .starts_with(r"\\.\pipe\PetalDesk-password-")
        && (32..=128).contains(&endpoint.token.len())
        && endpoint.process_id != 0
        && endpoint.expires_at_unix_ms > now_unix_ms
        && endpoint.expires_at_unix_ms.saturating_sub(now_unix_ms)
            <= SECRET_ENDPOINT_MAX_FUTURE.as_millis()
        && process_alive
}

#[cfg(windows)]
fn is_process_alive(process_id: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return false;
    }
    let mut exit_code = 0_u32;
    let alive = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0
        && exit_code == STILL_ACTIVE as u32;
    unsafe { CloseHandle(process) };
    alive
}

#[cfg(windows)]
fn read_secret_frame<R: Read>(reader: &mut R) -> Result<Option<Value>, String> {
    let mut length = [0_u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("failed to read browser secret message: {error}")),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_NATIVE_MESSAGE_BYTES {
        return Err(format!(
            "browser secret message length is invalid: {length}"
        ));
    }
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read browser secret message: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("browser secret message is invalid: {error}"))
}

#[cfg(windows)]
fn write_secret_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("failed to encode browser secret message: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_NATIVE_MESSAGE_BYTES {
        return Err(format!(
            "browser secret message length is invalid: {}",
            bytes.len()
        ));
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write browser secret message: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn native_message_codec_round_trips_json() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../browser-extension/test/fixtures/native-protocol.json"
        ))
        .unwrap();
        let value = &fixture["ready"];
        let mut bytes = Vec::new();
        write_native_message(&mut bytes, &value).unwrap();
        let decoded = read_native_message(&mut Cursor::new(bytes))
            .unwrap()
            .unwrap();
        assert_eq!(decoded.protocol_version, Some(PROTOCOL_VERSION));
        assert_eq!(decoded.kind, "extension.ready");
        assert_eq!(decoded.browser.as_deref(), Some("chrome"));

        let mut response_bytes = Vec::new();
        write_native_message(&mut response_bytes, &fixture["response"]).unwrap();
        let response = read_native_message(&mut Cursor::new(response_bytes))
            .unwrap()
            .unwrap();
        assert_eq!(response.kind, "extension.response");
        assert_eq!(response.id.as_deref(), Some("request-1"));
        assert_eq!(response.ok, Some(true));
    }

    #[test]
    fn rejects_oversized_native_messages() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((MAX_NATIVE_MESSAGE_BYTES + 1) as u32).to_le_bytes());
        let error = read_native_message(&mut Cursor::new(bytes)).unwrap_err();
        assert!(error.contains("长度无效"));
    }

    #[test]
    fn only_accepts_supported_browser_names() {
        assert_eq!(normalize_browser("Firefox").unwrap(), "firefox");
        assert!(normalize_browser("opera").is_err());
    }

    #[test]
    fn response_ids_cannot_escape_the_spool_directory() {
        assert!(is_safe_identifier("request-42_test"));
        assert!(!is_safe_identifier("../request"));
        assert!(!is_safe_identifier("request.json"));
    }

    #[test]
    fn password_commands_are_never_accepted_from_the_file_spool() {
        assert!(is_password_spool_command(&serde_json::json!({
            "type": "command",
            "id": "password-request",
            "command": "password.provideCredentials",
            "payload": { "password": "must-stay-in-memory" }
        })));
        assert!(is_password_spool_command(&serde_json::json!({
            "type": "secret.command",
            "id": "secret-request",
            "command": "capture.start"
        })));
        assert!(!is_password_spool_command(&serde_json::json!({
            "type": "command",
            "id": "capture-request",
            "command": "capture.start"
        })));
    }

    #[cfg(windows)]
    #[test]
    fn secret_reader_reports_a_closed_pipe() {
        let (command_tx, _command_rx) = mpsc::channel();
        let stop = AtomicBool::new(false);
        assert_eq!(
            run_secret_reader(&mut Cursor::new(Vec::<u8>::new()), &command_tx, &stop),
            SecretReaderExit::PipeClosed
        );
    }

    #[cfg(windows)]
    #[test]
    fn secret_writer_reports_a_pipe_write_failure() {
        struct BrokenWriter;

        impl Write for BrokenWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "test pipe closed",
                ))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (outbound_tx, outbound_rx) = mpsc::channel();
        outbound_tx
            .send(serde_json::json!({
                "type": "secret.response",
                "queuedAtUnixMs": unix_time_ms(),
            }))
            .unwrap();
        let stop = AtomicBool::new(false);
        assert_eq!(
            run_secret_writer(&mut BrokenWriter, &Mutex::new(outbound_rx), &stop),
            SecretWriterExit::PipeClosed
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancelling_synchronous_io_releases_a_blocked_pipe_reader() {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        assert_ne!(
            unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0,) },
            0
        );
        let mut reader = unsafe { std::fs::File::from_raw_handle(read_handle as _) };
        let writer = unsafe { std::fs::File::from_raw_handle(write_handle as _) };
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let reader_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut byte = [0_u8; 1];
            let result = reader.read_exact(&mut byte);
            let _ = finished_tx.send(result.is_err());
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let joined =
            cancel_and_join_secret_thread(reader_thread, Instant::now() + SECRET_IO_STOP_TIMEOUT);
        let cancelled = finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(writer);
        assert!(cancelled, "blocked pipe read was not cancelled");
        assert!(joined.is_some(), "cancelled pipe reader did not stop");
    }

    #[cfg(windows)]
    #[test]
    fn worker_stop_retries_after_a_missed_cancel_window() {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        assert_ne!(
            unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0,) },
            0
        );
        let mut reader = unsafe { std::fs::File::from_raw_handle(read_handle as _) };
        let writer = unsafe { std::fs::File::from_raw_handle(write_handle as _) };
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (proceed_tx, proceed_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let reader_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            proceed_rx.recv().unwrap();
            let mut byte = [0_u8; 1];
            let result = reader.read_exact(&mut byte);
            finished_tx.send(result.is_err()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let release_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            proceed_tx.send(()).unwrap();
        });
        let other_worker = std::thread::spawn(|| {});
        let stopped = cancel_and_join_secret_workers(
            reader_thread,
            other_worker,
            Instant::now() + SECRET_IO_STOP_TIMEOUT,
        );
        drop(writer);
        release_thread.join().unwrap();
        assert!(
            stopped,
            "workers did not stop after the missed-cancel window"
        );
        assert!(
            finished_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "the worker did not observe cancellation after entering pipe I/O"
        );
    }

    #[cfg(windows)]
    #[test]
    fn handshake_timeout_cancels_a_blocked_pipe_operation() {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        assert_ne!(
            unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0,) },
            0
        );
        let mut reader = unsafe { std::fs::File::from_raw_handle(read_handle as _) };
        let writer = unsafe { std::fs::File::from_raw_handle(write_handle as _) };
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let handshake_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut byte = [0_u8; 1];
            let _ = reader.read_exact(&mut byte);
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let outcome = wait_secret_handshake_thread(
            handshake_thread,
            Duration::from_millis(25),
            SECRET_IO_STOP_TIMEOUT,
        );
        drop(writer);
        assert!(
            matches!(outcome, SecretHandshakeWait::TimedOutStopped),
            "handshake timeout did not cancel the blocked operation"
        );
    }

    #[cfg(windows)]
    #[test]
    fn endpoint_requires_a_live_owner_and_a_bounded_unexpired_ttl() {
        let now = 1_000_000_u128;
        let mut endpoint = SecretEndpoint {
            version: PROTOCOL_VERSION,
            pipe_name: r"\\.\pipe\PetalDesk-password-test".to_string(),
            token: "x".repeat(48),
            process_id: std::process::id(),
            expires_at_unix_ms: now + 60_000,
        };
        assert!(secret_endpoint_is_valid_at(&endpoint, true, now));
        assert!(!secret_endpoint_is_valid_at(&endpoint, false, now));
        endpoint.expires_at_unix_ms = now;
        assert!(!secret_endpoint_is_valid_at(&endpoint, true, now));
        endpoint.expires_at_unix_ms = now + SECRET_ENDPOINT_MAX_FUTURE.as_millis() + 1;
        assert!(!secret_endpoint_is_valid_at(&endpoint, true, now));
        assert!(is_process_alive(std::process::id()));
    }

    #[cfg(windows)]
    static DIAG_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(windows)]
    struct TestTempDir(PathBuf);

    #[cfg(windows)]
    impl TestTempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("petaldesk-host-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn diag_log(&self) -> PathBuf {
            self.0.join("host-diagnostics.log")
        }
    }

    #[cfg(windows)]
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    fn set_test_diag_log(path: PathBuf) {
        *TEST_DIAG_LOG_PATH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    }

    #[cfg(windows)]
    fn clear_test_diag_log() {
        *TEST_DIAG_LOG_PATH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    #[cfg(windows)]
    #[test]
    fn fatal_flag_turns_into_a_host_exit_error() {
        let _guard = DIAG_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = TestTempDir::new();
        set_test_diag_log(temp.diag_log());

        let fatal = OnceLock::new();
        assert!(secret_fatal_exit_error(&fatal).is_none());
        fatal
            .set("browser secret pipe worker did not stop".to_string())
            .unwrap();
        let error = secret_fatal_exit_error(&fatal).expect("fatal flag must produce an error");
        assert!(error.contains("browser secret pipe worker did not stop"));

        let log = fs::read_to_string(temp.diag_log()).unwrap();
        assert!(log.contains("\"event\":\"fatal\""));
        assert!(log.contains("browser secret pipe worker did not stop"));
        clear_test_diag_log();
    }

    #[cfg(windows)]
    #[test]
    fn host_diag_log_appends_json_lines_and_trims_to_the_tail() {
        let temp = TestTempDir::new();
        let path = temp.diag_log();

        append_host_diag_line(&path, "one", "first").unwrap();
        append_host_diag_line(&path, "two", "second").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["event"], "one");
        assert_eq!(first["detail"], "first");
        assert!(first["ts"].is_number());
        // 未超限时截断是无操作。
        trim_host_diag_log(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        // 不存在的文件同样视为无需截断。
        trim_host_diag_log(&temp.0.join("missing.log")).unwrap();

        let mut oversized = String::new();
        while oversized.len() <= HOST_DIAG_MAX_BYTES as usize {
            oversized.push_str(&format!(
                "{{\"ts\":1,\"event\":\"fill\",\"detail\":\"{}\"}}\n",
                "x".repeat(1024)
            ));
        }
        oversized.push_str("{\"ts\":2,\"event\":\"tail\",\"detail\":\"last\"}\n");
        fs::write(&path, oversized).unwrap();
        trim_host_diag_log(&path).unwrap();
        let trimmed = fs::read_to_string(&path).unwrap();
        assert!(trimmed.len() <= HOST_DIAG_MAX_BYTES as usize);
        assert!(trimmed.contains("\"event\":\"tail\""));
        // 截断按换行对齐，留下的每一行都是完整 JSON。
        for line in trimmed.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn full_outbound_queue_flags_a_secret_reconnect() {
        let _guard = DIAG_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = TestTempDir::new();
        set_test_diag_log(temp.diag_log());

        let (sender, receiver) = mpsc::sync_channel::<Value>(1);
        sender
            .send(serde_json::json!({ "type": "secret.response", "id": "occupied" }))
            .unwrap();
        let result = sender.try_send(serde_json::json!({
            "type": "secret.response",
            "id": "abcdef0123456789",
            "result": { "password": "do-not-log" },
        }));
        let reconnect = AtomicBool::new(false);
        note_secret_outbound_failure(result, "responseId=abcdef01", &reconnect);
        assert!(reconnect.load(Ordering::Acquire));

        let log = fs::read_to_string(temp.diag_log()).unwrap();
        assert!(log.contains("\"event\":\"outbound-send-failed\""));
        assert!(log.contains("responseId=abcdef01"));
        assert!(log.contains("queue=full"));
        assert!(!log.contains("do-not-log"));

        // 队列对端消失（connector 已退出）也要标记重建。
        drop(receiver);
        let result = sender.try_send(serde_json::json!({ "type": "secret.event" }));
        let reconnect = AtomicBool::new(false);
        note_secret_outbound_failure(result, "event=capture", &reconnect);
        assert!(reconnect.load(Ordering::Acquire));
        let log = fs::read_to_string(temp.diag_log()).unwrap();
        assert!(log.contains("queue=disconnected"));

        // 发送成功不改变标志、不写日志。
        let reconnect = AtomicBool::new(false);
        note_secret_outbound_failure(Ok(()), "event=capture", &reconnect);
        assert!(!reconnect.load(Ordering::Acquire));
        clear_test_diag_log();
    }
}

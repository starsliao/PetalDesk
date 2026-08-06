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
// Read 只被同步帧读取（非 Windows 路径与测试）使用；Windows 生产路径全部
// 走重叠 I/O 原语。
#[cfg(any(not(windows), test))]
use std::io::Read;
use std::io::Write;
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
const DIAG_CAPACITY: usize = 100;
/// 超时请求的存活探测时限。扩展端 native-bridge.js 会直接应答 ping（不经过
/// password-bridge），健康连接即使真实命令卡住也应在 1.5s 内回复。
const PING_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// 结构化诊断条目：只含事件名和短标识符，绝不包含 token 或凭据 payload。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagEntry {
    pub(crate) at_unix_ms: u128,
    pub(crate) layer: &'static str,
    pub(crate) event: &'static str,
    pub(crate) detail: String,
}

/// `send_and_wait` 的失败分类。只有 `TimedOut` 值得先做 ping 探测，其余两种
/// 已经说明连接本身完了。
enum SecretSendError {
    /// 扩展返回了错误应答；连接本身是健康的。
    Answered(String),
    /// 请求发不出去或应答通道断开；按 reason 退休该 generation。
    Retire { reason: &'static str, message: String },
    /// 超时未收到应答；退休前先用 ping 探测连接死活。
    TimedOut(String),
}

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
    // 每代连接一个手动重置事件：retire 时 signal，挂在重叠 I/O 等待
    // （WaitForMultipleObjects([op_event, stop_event])）里的 reader/writer
    // 随即醒来，CancelIoEx 自己挂起的操作并退出。CancelSynchronousIo 对重叠
    // 等待无效，线程句柄注册机制已随之移除。
    #[cfg(windows)]
    stop_event: SecretPipeEvent,
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
    diag: Mutex<VecDeque<DiagEntry>>,
    last_request_outcome: Mutex<Option<DiagEntry>>,
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
            stop_event: SecretPipeEvent::new_manual_reset()?,
        })
    }

    #[cfg(test)]
    fn detached() -> Self {
        Self {
            retired: AtomicBool::new(false),
            #[cfg(windows)]
            pipe: Mutex::new(None),
            #[cfg(windows)]
            stop_event: SecretPipeEvent::new_manual_reset()
                .expect("failed to create test secret pipe stop event"),
        }
    }

    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    #[cfg(windows)]
    fn stop_event(&self) -> &SecretPipeEvent {
        &self.stop_event
    }

    fn retire(&self) -> bool {
        if self.retired.swap(true, Ordering::AcqRel) {
            return false;
        }
        #[cfg(windows)]
        {
            // 手动重置事件保持 signaled：无论 worker 正挂在等待里还是尚未进入
            // 下一次操作，都会在标志检查或等待处醒来并自行取消挂起操作。
            self.stop_event.signal();
            disconnect_registered_pipe(&self.pipe);
        }
        true
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

/// 手动重置事件：每代连接一个 stop 事件（SecretConnectionControl 持有），
/// 每个 I/O 线程另有自己的操作事件。
#[cfg(windows)]
struct SecretPipeEvent(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl SecretPipeEvent {
    fn new_manual_reset() -> Result<Self, String> {
        use windows_sys::Win32::System::Threading::CreateEventW;
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            Err(format!(
                "CreateEventW failed: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(handle))
        }
    }

    fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }

    fn signal(&self) {
        use windows_sys::Win32::System::Threading::SetEvent;
        unsafe { SetEvent(self.0) };
    }

    fn reset(&self) {
        use windows_sys::Win32::System::Threading::ResetEvent;
        unsafe { ResetEvent(self.0) };
    }
}

#[cfg(windows)]
impl Drop for SecretPipeEvent {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe { CloseHandle(self.0) };
    }
}

// SetEvent/WaitForMultipleObjects 可以安全地从多个线程作用于同一事件句柄。
#[cfg(windows)]
unsafe impl Send for SecretPipeEvent {}
#[cfg(windows)]
unsafe impl Sync for SecretPipeEvent {}

/// 一次重叠管道 I/O 的结局。
#[cfg(windows)]
#[derive(Debug)]
enum SecretPipeIo {
    /// 操作完成，值为本次传输的字节数。
    Completed(usize),
    /// 对端关闭或断开（管道视角的 EOF）。
    Ended,
    /// 其他 I/O 错误。
    Failed(String),
    /// 停止事件触发，挂起操作已被 CancelIoEx 取消。
    Stopped,
}

#[cfg(windows)]
fn classify_secret_pipe_error(context: &str, error: &std::io::Error) -> SecretPipeIo {
    use windows_sys::Win32::Foundation::{
        ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_NOT_CONNECTED,
    };
    let Some(code) = error.raw_os_error() else {
        return SecretPipeIo::Failed(format!("{context}: {error}"));
    };
    if code == ERROR_OPERATION_ABORTED as i32 {
        SecretPipeIo::Stopped
    } else if code == ERROR_BROKEN_PIPE as i32
        || code == ERROR_PIPE_NOT_CONNECTED as i32
        || code == ERROR_NO_DATA as i32
    {
        SecretPipeIo::Ended
    } else {
        SecretPipeIo::Failed(format!("{context}: {error}"))
    }
}

/// 在 overlapped 句柄上发起一次 ReadFile 并等待结果。每次操作使用调用线程
/// 自己的 OVERLAPPED+事件，因此 reader/writer 可以在同一管道句柄上并发挂起
/// 各自的读/写——这正是"同步句柄有挂起读时写死锁"的修复点。
#[cfg(windows)]
fn secret_pipe_read(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
    buffer: &mut [u8],
) -> SecretPipeIo {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_IO_PENDING};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    op_event.reset();
    let mut overlapped = OVERLAPPED::default();
    overlapped.hEvent = op_event.handle();
    let read = unsafe {
        ReadFile(
            pipe,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            &mut overlapped,
        )
    };
    if read != 0 {
        return collect_secret_pipe_io(pipe, &overlapped, "failed to read browser secret frame");
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return classify_secret_pipe_error(
            "failed to read browser secret frame",
            &std::io::Error::from_raw_os_error(error as i32),
        );
    }
    wait_secret_pipe_io(pipe, &overlapped, stop_event, "failed to read browser secret frame")
}

/// `secret_pipe_read` 的写方向对应物。
#[cfg(windows)]
fn secret_pipe_write(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
    bytes: &[u8],
) -> SecretPipeIo {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_IO_PENDING};
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    op_event.reset();
    let mut overlapped = OVERLAPPED::default();
    overlapped.hEvent = op_event.handle();
    let written = unsafe {
        WriteFile(
            pipe,
            bytes.as_ptr(),
            bytes.len() as u32,
            std::ptr::null_mut(),
            &mut overlapped,
        )
    };
    if written != 0 {
        return collect_secret_pipe_io(pipe, &overlapped, "failed to write browser secret frame");
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return classify_secret_pipe_error(
            "failed to write browser secret frame",
            &std::io::Error::from_raw_os_error(error as i32),
        );
    }
    wait_secret_pipe_io(pipe, &overlapped, stop_event, "failed to write browser secret frame")
}

/// 同步完成（ReadFile/WriteFile 直接返回 TRUE）或等待完成后取实际字节数。
#[cfg(windows)]
fn collect_secret_pipe_io(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    overlapped: &windows_sys::Win32::System::IO::OVERLAPPED,
    context: &str,
) -> SecretPipeIo {
    use windows_sys::Win32::System::IO::GetOverlappedResult;
    let mut transferred = 0_u32;
    if unsafe { GetOverlappedResult(pipe, overlapped, &mut transferred, 0) } != 0 {
        SecretPipeIo::Completed(transferred as usize)
    } else {
        classify_secret_pipe_error(context, &std::io::Error::last_os_error())
    }
}

/// 等待一个已挂起的重叠操作：操作事件与停止事件先到先赢。停止路径取消挂起
/// 操作并以 bWait=TRUE 取一次结果，确保内核不再持有 OVERLAPPED 和缓冲区之后
/// 它们才离开作用域。
#[cfg(windows)]
fn wait_secret_pipe_io(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    overlapped: &windows_sys::Win32::System::IO::OVERLAPPED,
    stop_event: Option<&SecretPipeEvent>,
    context: &str,
) -> SecretPipeIo {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult};
    use windows_sys::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};

    let waited = if let Some(stop_event) = stop_event {
        let handles = [overlapped.hEvent, stop_event.handle()];
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) }
    } else {
        let handles = [overlapped.hEvent];
        unsafe { WaitForMultipleObjects(1, handles.as_ptr(), 0, INFINITE) }
    };
    if waited == WAIT_OBJECT_0 {
        return collect_secret_pipe_io(pipe, overlapped, context);
    }
    let _ = unsafe { CancelIoEx(pipe, overlapped) };
    let mut transferred = 0_u32;
    let _ = unsafe { GetOverlappedResult(pipe, overlapped, &mut transferred, 1) };
    if waited == WAIT_OBJECT_0 + 1 {
        SecretPipeIo::Stopped
    } else {
        SecretPipeIo::Failed(format!(
            "{context}: pipe wait failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// 重叠版 read_exact：字节管道允许部分读，循环累积直到填满缓冲区。
#[cfg(windows)]
fn secret_pipe_read_exact(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
    buffer: &mut [u8],
) -> SecretPipeIo {
    let mut filled = 0_usize;
    while filled < buffer.len() {
        match secret_pipe_read(pipe, op_event, stop_event, &mut buffer[filled..]) {
            SecretPipeIo::Completed(0) => return SecretPipeIo::Ended,
            SecretPipeIo::Completed(bytes) => filled += bytes,
            other => return other,
        }
    }
    SecretPipeIo::Completed(filled)
}

/// 重叠版 write_all：部分写时循环写满。
#[cfg(windows)]
fn secret_pipe_write_all(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
    bytes: &[u8],
) -> SecretPipeIo {
    let mut written = 0_usize;
    while written < bytes.len() {
        match secret_pipe_write(pipe, op_event, stop_event, &bytes[written..]) {
            SecretPipeIo::Completed(0) => return SecretPipeIo::Ended,
            SecretPipeIo::Completed(bytes) => written += bytes,
            other => return other,
        }
    }
    SecretPipeIo::Completed(written)
}

/// 帧级 I/O 的结局；Ended/Failed 保持 read_frame 的 EOF/错误区分，供读循环
/// 沿用 "read-loop-ended"/"read-loop-failed" 的退休原因。
#[cfg(windows)]
#[derive(Debug)]
enum SecretFrameIo {
    Frame(Value),
    Written,
    Ended,
    Failed(String),
    Stopped,
}

/// 帧格式与 read_frame 相同：4 字节 LE 长度前缀 + JSON。
#[cfg(windows)]
fn read_frame_overlapped(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
) -> SecretFrameIo {
    let mut length = [0_u8; 4];
    match secret_pipe_read_exact(pipe, op_event, stop_event, &mut length) {
        SecretPipeIo::Completed(_) => {}
        SecretPipeIo::Ended => return SecretFrameIo::Ended,
        SecretPipeIo::Failed(error) => return SecretFrameIo::Failed(error),
        SecretPipeIo::Stopped => return SecretFrameIo::Stopped,
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_SECRET_MESSAGE_BYTES {
        return SecretFrameIo::Failed(format!(
            "browser secret frame length is invalid: {length}"
        ));
    }
    let mut bytes = vec![0_u8; length];
    match secret_pipe_read_exact(pipe, op_event, stop_event, &mut bytes) {
        SecretPipeIo::Completed(_) => {}
        SecretPipeIo::Ended => return SecretFrameIo::Ended,
        SecretPipeIo::Failed(error) => return SecretFrameIo::Failed(error),
        SecretPipeIo::Stopped => return SecretFrameIo::Stopped,
    }
    match serde_json::from_slice(&bytes) {
        Ok(value) => SecretFrameIo::Frame(value),
        Err(error) => SecretFrameIo::Failed(format!("browser secret frame is invalid: {error}")),
    }
}

/// `write_frame` 的重叠版：同一帧格式，写完即对端可见，无需 flush。
#[cfg(windows)]
fn write_frame_overlapped(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: &SecretPipeEvent,
    stop_event: Option<&SecretPipeEvent>,
    value: &Value,
) -> SecretFrameIo {
    let bytes = match encode_frame(value) {
        Ok(bytes) => bytes,
        Err(error) => return SecretFrameIo::Failed(error),
    };
    match secret_pipe_write_all(pipe, op_event, stop_event, &bytes) {
        SecretPipeIo::Completed(_) => SecretFrameIo::Written,
        SecretPipeIo::Ended => SecretFrameIo::Ended,
        SecretPipeIo::Failed(error) => SecretFrameIo::Failed(error),
        SecretPipeIo::Stopped => SecretFrameIo::Stopped,
    }
}

/// 写一帧的最小抽象：生产路径是 `OverlappedFrameWriter`（重叠管道写 + stop
/// 事件唤醒），测试与非 Windows 平台用任意 `Write`。
trait SecretFrameWriter {
    /// 写一帧；Err 表示连接级失败，调用方据此退休该 generation。
    fn write_frame(&mut self, value: &Value) -> Result<(), String>;
}

#[cfg(any(not(windows), test))]
impl<W: Write> SecretFrameWriter for W {
    fn write_frame(&mut self, value: &Value) -> Result<(), String> {
        write_frame(self, value)
    }
}

#[cfg(windows)]
struct OverlappedFrameWriter<'a> {
    pipe: windows_sys::Win32::Foundation::HANDLE,
    op_event: SecretPipeEvent,
    stop_event: &'a SecretPipeEvent,
}

#[cfg(windows)]
impl SecretFrameWriter for OverlappedFrameWriter<'_> {
    fn write_frame(&mut self, value: &Value) -> Result<(), String> {
        match write_frame_overlapped(self.pipe, &self.op_event, Some(self.stop_event), value) {
            SecretFrameIo::Written => Ok(()),
            // retire 唤醒：按写失败退出（与 CancelSynchronousIo 时代一致，
            // close_connection_generation 对同一代的重复关闭是幂等的）。
            SecretFrameIo::Stopped => Err("browser secret connection was retired".to_string()),
            SecretFrameIo::Ended => Err("browser secret pipe closed".to_string()),
            SecretFrameIo::Failed(error) => Err(error),
            SecretFrameIo::Frame(_) => {
                Err("browser secret pipe write returned an unexpected frame".to_string())
            }
        }
    }
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
                diag: Mutex::new(VecDeque::new()),
                last_request_outcome: Mutex::new(None),
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
                diag: Mutex::new(VecDeque::new()),
                last_request_outcome: Mutex::new(None),
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
        let request_id = Uuid::new_v4().to_string();
        match self.send_and_wait(&connection, &request_id, command, payload, timeout) {
            Ok(value) => {
                self.note_request_outcome("completed", command, &request_id, "ok");
                Ok(value)
            }
            Err(SecretSendError::Answered(message)) => {
                self.note_request_outcome("failed", command, &request_id, &message);
                Err(message)
            }
            Err(SecretSendError::Retire { reason, message }) => {
                close_connection_generation(
                    &self.inner,
                    connection_id,
                    connection.browser,
                    connection.generation,
                    &connection.control,
                    reason,
                );
                self.note_request_outcome("connection-retired", command, &request_id, reason);
                Err(message)
            }
            Err(SecretSendError::TimedOut(message)) => {
                record_diag(
                    &self.inner,
                    "request",
                    "timeout",
                    request_detail(command, &request_id),
                );
                if self.probe_connection(&connection) {
                    // 连接仍然健康：只让本次调用方看到超时，不退休 generation，
                    // 避免单次慢请求触发"杀连接→重连→再超时"的抖动循环。
                    record_diag(
                        &self.inner,
                        "request",
                        "probe-ok",
                        request_detail(command, &request_id),
                    );
                    self.note_request_outcome(
                        "timeout",
                        command,
                        &request_id,
                        "ping ok, connection kept",
                    );
                    return Err(message);
                }
                record_diag(
                    &self.inner,
                    "request",
                    "probe-failed",
                    request_detail(command, &request_id),
                );
                close_connection_generation(
                    &self.inner,
                    connection_id,
                    connection.browser,
                    connection.generation,
                    &connection.control,
                    "request-timeout",
                );
                self.note_request_outcome(
                    "timeout",
                    command,
                    &request_id,
                    "ping failed, connection retired",
                );
                Err(message)
            }
        }
    }

    /// 发送一条命令并等待应答，不做任何退休处理：失败含义由调用方决定。
    /// ping 探测也走这里，因此探测自身绝不会触发新的探测。
    fn send_and_wait(
        &self,
        connection: &SecretConnection,
        request_id: &str,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, SecretSendError> {
        if connection.control.is_retired() {
            return Err(SecretSendError::Retire {
                reason: "already-retired",
                message: "password browser connection closed".to_string(),
            });
        }
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        lock_unpoisoned(&self.inner.pending).insert(request_id.to_string(), response_tx);
        let request = serde_json::json!({
            "version": SECRET_PROTOCOL_VERSION,
            "type": "secret.command",
            "id": request_id,
            "protocolVersion": 1,
            "command": command,
            "payload": payload,
        });
        if connection.control.is_retired() {
            lock_unpoisoned(&self.inner.pending).remove(request_id);
            return Err(SecretSendError::Retire {
                reason: "already-retired",
                message: "password browser connection closed".to_string(),
            });
        }
        if let Err(error) = connection.sender.try_send(request) {
            lock_unpoisoned(&self.inner.pending).remove(request_id);
            let (reason, message) = match error {
                mpsc::TrySendError::Full(_) => {
                    ("request-queue-full", "password browser connection is busy")
                }
                mpsc::TrySendError::Disconnected(_) => (
                    "request-queue-disconnected",
                    "password browser connection closed",
                ),
            };
            record_diag(
                &self.inner,
                "request",
                reason,
                request_detail(command, request_id),
            );
            return Err(SecretSendError::Retire {
                reason,
                message: message.to_string(),
            });
        }
        let result = match response_rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(SecretSendError::Answered(message)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(SecretSendError::TimedOut(format!(
                "password browser request timed out after {} ms",
                timeout.as_millis()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SecretSendError::Retire {
                reason: "response-channel-closed",
                message: "password browser connection closed".to_string(),
            }),
        };
        lock_unpoisoned(&self.inner.pending).remove(request_id);
        result
    }

    /// 用同一连接发内部 ping 探测死活。扩展端 native-bridge.js 直接应答
    /// ping（不经过 password-bridge），所以真实命令卡住时健康连接仍会快速回复。
    fn probe_connection(&self, connection: &SecretConnection) -> bool {
        self.send_and_wait(
            connection,
            &Uuid::new_v4().to_string(),
            "ping",
            Value::Object(Default::default()),
            PING_PROBE_TIMEOUT,
        )
        .is_ok()
    }

    fn note_request_outcome(
        &self,
        event: &'static str,
        command: &str,
        request_id: &str,
        outcome: &str,
    ) {
        *lock_unpoisoned(&self.inner.last_request_outcome) = Some(DiagEntry {
            at_unix_ms: unix_time_ms(),
            layer: "request",
            event,
            detail: format!("{} outcome={outcome}", request_detail(command, request_id)),
        });
    }

    /// 最近 N 条诊断（按时间升序）。供密码状态接口展示通道健康状况。
    pub(crate) fn diag_snapshot(&self, limit: usize) -> Vec<DiagEntry> {
        let diag = lock_unpoisoned(&self.inner.diag);
        diag.iter()
            .skip(diag.len().saturating_sub(limit))
            .cloned()
            .collect()
    }

    /// 最近一次请求的结局。供密码状态接口展示。
    pub(crate) fn last_request_outcome(&self) -> Option<DiagEntry> {
        lock_unpoisoned(&self.inner.last_request_outcome).clone()
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

pub(crate) fn bridge_root() -> Result<PathBuf, String> {
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
    // 精确的 "ping" 是连接存活探测命令，由扩展直接应答；其余命令必须以
    // "password." 开头。
    if command != "ping"
        && (!command.starts_with("password.")
            || command.len() > 64
            || !command
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err("password browser command name is invalid".to_string());
    }
    Ok(())
}

/// 诊断 detail 只含命令名和 requestId 前 8 位，绝不包含 payload。
fn request_detail(command: &str, request_id: &str) -> String {
    let short_id = request_id.get(..8).unwrap_or(request_id);
    format!("command={command} requestId={short_id}")
}

fn record_diag(inner: &SecretInner, layer: &'static str, event: &'static str, detail: impl Into<String>) {
    let mut diag = lock_unpoisoned(&inner.diag);
    while diag.len() >= DIAG_CAPACITY {
        diag.pop_front();
    }
    diag.push_back(DiagEntry {
        at_unix_ms: unix_time_ms(),
        layer,
        event,
        detail: detail.into(),
    });
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

/// 同步帧读取。Windows 生产路径的管道句柄已改为 FILE_FLAG_OVERLAPPED（同步
/// ReadFile 在其上是未定义行为），此函数保留给非 Windows 路径与测试使用。
#[cfg(any(not(windows), test))]
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

fn encode_frame(value: &Value) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("failed to encode browser secret frame: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_SECRET_MESSAGE_BYTES {
        return Err(format!(
            "browser secret frame length is invalid: {}",
            bytes.len()
        ));
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&bytes);
    Ok(frame)
}

/// 同步帧写入（含 flush），语义不变；保留给非 Windows 路径与测试使用。
/// Windows 生产写入路径是 `write_frame_overlapped`（管道写完即可见，无 flush）。
#[cfg(any(not(windows), test))]
fn write_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), String> {
    let frame = encode_frame(value)?;
    writer
        .write_all(&frame)
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write browser secret frame: {error}"))
}

/// 握手与连接建立阶段。所有错误文案均为静态字符串或 I/O 错误，不含 token，
/// 因此可以安全写入诊断缓冲。
fn establish_secret_connection(
    pipe: &mut std::fs::File,
    inner: &SecretInner,
) -> Result<(String, BrowserFamily, std::fs::File, Arc<SecretConnectionControl>), String> {
    // Windows 的管道句柄带 FILE_FLAG_OVERLAPPED，握手读写也必须走重叠结构；
    // 握手阶段尚无 stop 事件，等待语义与原先的同步阻塞一致（对端静默则一直等，
    // 对端断开则报错退出）。
    #[cfg(windows)]
    let handshake_raw = {
        use std::os::windows::io::AsRawHandle;
        pipe.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
    };
    #[cfg(windows)]
    let handshake_event = SecretPipeEvent::new_manual_reset()?;
    #[cfg(windows)]
    let hello = match read_frame_overlapped(handshake_raw, &handshake_event, None) {
        SecretFrameIo::Frame(hello) => hello,
        SecretFrameIo::Failed(error) => return Err(error),
        _ => return Err("browser secret pipe closed".to_string()),
    };
    #[cfg(not(windows))]
    let hello = read_frame(pipe)?.ok_or_else(|| "browser secret pipe closed".to_string())?;
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
    validate_named_pipe_client(pipe, declared_process_id)?;
    let ready = serde_json::json!({
        "version": SECRET_PROTOCOL_VERSION,
        "type": "secret.ready",
    });
    #[cfg(windows)]
    match write_frame_overlapped(handshake_raw, &handshake_event, None, &ready) {
        SecretFrameIo::Written => {}
        SecretFrameIo::Failed(error) => return Err(error),
        _ => return Err("browser secret pipe closed".to_string()),
    }
    #[cfg(not(windows))]
    write_frame(pipe, &ready)?;

    let reader = pipe
        .try_clone()
        .map_err(|error| format!("failed to clone browser secret pipe: {error}"))?;
    let control = Arc::new(SecretConnectionControl::new(pipe)?);
    Ok((connection_id, browser, reader, control))
}

fn handle_pipe_connection(mut pipe: std::fs::File, inner: Arc<SecretInner>) -> Result<(), String> {
    let (connection_id, browser, reader, control) =
        match establish_secret_connection(&mut pipe, &inner) {
            Ok(established) => established,
            Err(error) => {
                record_diag(&inner, "handshake", "failed", error.clone());
                return Err(error);
            }
        };
    record_diag(
        &inner,
        "handshake",
        "succeeded",
        format!("connectionId={connection_id} browser={}", browser.as_str()),
    );
    // reader 的操作事件（Windows）：每个线程自己的 OVERLAPPED+事件，与 writer
    // 在同一管道句柄上并发挂起互不阻塞。创建失败与建立阶段失败同等处理。
    #[cfg(windows)]
    let reader_event = match SecretPipeEvent::new_manual_reset() {
        Ok(event) => event,
        Err(error) => {
            record_diag(&inner, "handshake", "failed", error.clone());
            return Err(error);
        }
    };
    #[cfg(windows)]
    let reader_raw = {
        use std::os::windows::io::AsRawHandle;
        reader.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
    };
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
    record_diag(
        &inner,
        "connection",
        "established",
        format!(
            "connectionId={connection_id} browser={} generation={generation}",
            browser.as_str()
        ),
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
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let writer_raw = pipe.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let stop_source = writer_control.clone();
            match SecretPipeEvent::new_manual_reset() {
                Ok(op_event) => {
                    let frame_writer = OverlappedFrameWriter {
                        pipe: writer_raw,
                        op_event,
                        stop_event: stop_source.stop_event(),
                    };
                    run_connection_writer(
                        frame_writer,
                        outbound_rx,
                        writer_inner,
                        writer_connection_id,
                        browser,
                        generation,
                        writer_control,
                    );
                }
                Err(_) => {
                    close_connection_generation(
                        &writer_inner,
                        &writer_connection_id,
                        browser,
                        generation,
                        &writer_control,
                        "writer-register-failed",
                    );
                }
            }
        }
        #[cfg(not(windows))]
        run_connection_writer(
            pipe,
            outbound_rx,
            writer_inner,
            writer_connection_id,
            browser,
            generation,
            writer_control,
        );
    });

    // Keep the read error until after connection cleanup. A browser process can
    // disappear with a BrokenPipe/ConnectionReset error instead of a clean EOF;
    // returning through `?` here would leave stale connection state behind.
    let read_result = (|| {
        loop {
            #[cfg(windows)]
            let next = match read_frame_overlapped(
                reader_raw,
                &reader_event,
                Some(control.stop_event()),
            ) {
                SecretFrameIo::Frame(message) => Some(message),
                SecretFrameIo::Failed(error) => return Err(error),
                // Ended：对端断开；Stopped：retire 唤醒（该代的退休已由发起方
                // 记录，此处的关闭是幂等空操作）；Written：读路径不可达。
                _ => None,
            };
            #[cfg(not(windows))]
            let next = read_frame(&mut &reader)?;
            let Some(message) = next else { break };
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

    let read_reason = if read_result.is_ok() {
        "read-loop-ended"
    } else {
        "read-loop-failed"
    };
    close_connection_generation(
        &inner,
        &connection_id,
        browser,
        generation,
        &control,
        read_reason,
    );
    let _ = writer.join();
    read_result
}

fn run_connection_writer<W: SecretFrameWriter>(
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
        if writer.write_frame(&message).is_err() {
            close_connection_generation(
                &inner,
                &connection_id,
                browser,
                generation,
                &control,
                "writer-io-failed",
            );
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
    reason: &'static str,
) -> bool {
    control.retire();
    if !remove_connection_generation(&inner.connections, connection_id, generation) {
        return false;
    }
    record_diag(
        inner,
        "connection",
        "retired",
        format!(
            "connectionId={connection_id} browser={} reason={reason}",
            browser.as_str()
        ),
    );
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

/// Builds a bridge with one fake Firefox connection whose outbound requests
/// arrive on the returned channel. Password-service tests pair this with
/// `test_answer_request` to drive request/response flows without a real pipe.
#[cfg(test)]
pub(crate) fn test_bridge(connection_id: &str) -> (BrowserSecretBridge, mpsc::Receiver<Value>) {
    let inner = Arc::new(SecretInner {
        token: Mutex::new("test-token".to_string()),
        endpoint_path: PathBuf::new(),
        connections: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        events: Mutex::new(VecDeque::new()),
        event_ready: Condvar::new(),
        diag: Mutex::new(VecDeque::new()),
        last_request_outcome: Mutex::new(None),
    });
    let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED_EVENTS);
    insert_connection_generation(
        &inner.connections,
        connection_id.to_string(),
        SecretConnection {
            browser: BrowserFamily::Firefox,
            sender,
            connected_at: Instant::now(),
            generation: Uuid::new_v4(),
            control: Arc::new(SecretConnectionControl::detached()),
        },
    );
    (BrowserSecretBridge { inner }, receiver)
}

#[cfg(test)]
impl BrowserSecretBridge {
    /// Answers an in-flight request captured from the test channel.
    pub(crate) fn test_answer_request(&self, request: &Value, response: Value) {
        let Some(id) = request.get("id").and_then(Value::as_str) else {
            return;
        };
        if let Some(pending) = lock_unpoisoned(&self.inner.pending).remove(id) {
            let _ = pending.send(Ok(response));
        }
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
    record_diag(&inner, "pipe-server", "started", pipe_name);
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
        GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};

    let name = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let security = CurrentUserSecurity::new()?;
    // 服务器端句柄同样必须带 FILE_FLAG_OVERLAPPED：同步句柄上"有挂起阻塞读时
    // 另一线程的 WriteFile 永久阻塞"。代价是该句柄上的所有 I/O（含
    // ConnectNamedPipe 和握手）都必须走 OVERLAPPED。
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
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
    let connect = (|| {
        let connect_event = SecretPipeEvent::new_manual_reset()?;
        let mut overlapped = OVERLAPPED::default();
        overlapped.hEvent = connect_event.handle();
        if unsafe { ConnectNamedPipe(handle, &mut overlapped) } != 0 {
            return Ok(());
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_PIPE_CONNECTED {
            return Ok(());
        }
        if error != ERROR_IO_PENDING {
            return Err(format!(
                "ConnectNamedPipe failed: {}",
                std::io::Error::from_raw_os_error(error as i32)
            ));
        }
        // 等待客户端接入；与原先的同步等待语义一致（对端不连则一直等）。
        if unsafe { WaitForSingleObject(connect_event.handle(), INFINITE) } != 0 {
            return Err(format!(
                "ConnectNamedPipe wait failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut transferred = 0_u32;
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } == 0 {
            return Err(format!(
                "ConnectNamedPipe failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    })();
    if let Err(error) = connect {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(error);
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
            diag: Mutex::new(VecDeque::new()),
            last_request_outcome: Mutex::new(None),
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
    fn validate_command_allows_exact_ping_only() {
        assert!(validate_command("ping").is_ok());
        assert!(validate_command("pinger").is_err());
        assert!(validate_command("ping.pong").is_err());
        assert!(validate_command(" password.").is_err());
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
        let (sender, receiver) = mpsc::sync_channel(4);
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
        // 退休前会先做一次 ping 探测：队列里依次看到原请求和 ping，且 ping
        // 同样无人应答后连接才被退休。
        let mut commands = Vec::new();
        while let Ok(request) = receiver.recv_timeout(Duration::from_secs(1)) {
            commands.push(
                request
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            );
        }
        assert_eq!(
            commands,
            vec![
                Some("password.getStatus".to_string()),
                Some("ping".to_string())
            ]
        );
        let event = bridge.receive_event(Duration::ZERO).unwrap();
        assert_eq!(event.connection_id, "timed-out");
        assert_eq!(event.event, "connectionClosed");
        let outcome = bridge.last_request_outcome().unwrap();
        assert_eq!(outcome.event, "timeout");
        assert!(outcome.detail.contains("command=password.getStatus"));
        assert!(outcome.detail.contains("ping failed"));
        let diag = bridge.diag_snapshot(DIAG_CAPACITY);
        assert!(diag.iter().any(|entry| entry.event == "timeout"));
        assert!(diag.iter().any(|entry| entry.event == "probe-failed"));
        assert!(diag
            .iter()
            .any(|entry| entry.event == "retired"
                && entry.detail.contains("reason=request-timeout")));
    }

    #[test]
    fn timed_out_request_keeps_connection_when_ping_answers() {
        let inner = test_inner();
        let bridge = BrowserSecretBridge {
            inner: inner.clone(),
        };
        let (sender, receiver) = mpsc::sync_channel(4);
        let generation = Uuid::new_v4();
        let control = insert_test_connection(&inner, "probe-alive", sender, generation);
        // 模拟 Host 一侧：只应答 ping（与 native-bridge.js 的行为一致），
        // 真实命令一直不应答。
        let responder_inner = inner.clone();
        let responder = std::thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                if request.get("command").and_then(Value::as_str) != Some("ping") {
                    continue;
                }
                let Some(id) = request.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let pending = lock_unpoisoned(&responder_inner.pending).remove(id);
                if let Some(pending) = pending {
                    let _ = pending.send(Ok(serde_json::json!({ "pong": true })));
                }
            }
        });

        let error = bridge
            .request_connection(
                "probe-alive",
                "password.getStatus",
                Value::Object(Default::default()),
                Duration::from_millis(50),
            )
            .unwrap_err();

        assert_eq!(
            error,
            "password browser request timed out after 50 ms"
        );
        // ping 应答正常：连接保留，不退休 generation，也不产生关闭事件。
        assert!(!control.is_retired());
        assert!(lock_unpoisoned(&inner.connections).contains_key("probe-alive"));
        assert!(lock_unpoisoned(&inner.pending).is_empty());
        assert!(bridge.receive_event(Duration::ZERO).is_none());
        let outcome = bridge.last_request_outcome().unwrap();
        assert_eq!(outcome.event, "timeout");
        assert!(outcome.detail.contains("ping ok"));
        let diag = bridge.diag_snapshot(DIAG_CAPACITY);
        assert!(diag.iter().any(|entry| entry.event == "probe-ok"));
        assert!(!diag.iter().any(|entry| entry.event == "probe-failed"));

        assert!(close_connection_generation(
            &inner,
            "probe-alive",
            BrowserFamily::Firefox,
            generation,
            &control,
            "test-close",
        ));
        responder.join().unwrap();
    }

    #[test]
    fn diag_buffer_keeps_only_the_most_recent_entries() {
        let inner = test_inner();
        for index in 0..(DIAG_CAPACITY + 25) {
            record_diag(&inner, "test", "fill", format!("entry-{index}"));
        }
        let bridge = BrowserSecretBridge {
            inner: inner.clone(),
        };
        let snapshot = bridge.diag_snapshot(DIAG_CAPACITY * 2);
        assert_eq!(snapshot.len(), DIAG_CAPACITY);
        assert_eq!(snapshot[0].detail, "entry-25");
        assert_eq!(
            snapshot[DIAG_CAPACITY - 1].detail,
            format!("entry-{}", DIAG_CAPACITY + 24)
        );
        let limited = bridge.diag_snapshot(5);
        assert_eq!(limited.len(), 5);
        assert_eq!(limited[0].detail, format!("entry-{}", DIAG_CAPACITY + 20));
        assert_eq!(limited[4].detail, format!("entry-{}", DIAG_CAPACITY + 24));
        assert!(limited.iter().all(|entry| entry.layer == "test"
            && entry.event == "fill"
            && entry.at_unix_ms > 0));
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
            "test-close",
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
            "test-close",
        ));
        assert!(!close_connection_generation(
            &inner,
            "same-id",
            BrowserFamily::Firefox,
            new_generation,
            &replacement_control,
            "test-close",
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
            "test-close",
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
            "test-close",
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
        let reader = server.try_clone().unwrap();
        let reader_control = control.clone();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let reader_thread = std::thread::spawn(move || {
            use std::os::windows::io::AsRawHandle;
            let raw = reader.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let op_event = SecretPipeEvent::new_manual_reset().unwrap();
            let result = read_frame_overlapped(raw, &op_event, Some(reader_control.stop_event()));
            let _ = finished_tx.send(result);
        });
        // 让 reader 先挂起在重叠读上（客户端保持静默）。
        std::thread::sleep(Duration::from_millis(300));

        assert!(control.retire());
        let result = finished_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("retired named pipe reader remained blocked");
        assert!(
            matches!(result, SecretFrameIo::Stopped),
            "retired reader observed {result:?} instead of the stop event"
        );
        reader_thread.join().unwrap();
        drop(client);
    }

    /// 核心回归：服务器端 overlapped 句柄上"有挂起读时写必须完成"（同步句柄
    /// 在同一情形下的死锁正是本次修复对象）。
    #[cfg(windows)]
    #[test]
    fn server_end_overlapped_write_completes_while_read_is_pending() {
        use std::os::windows::io::AsRawHandle;
        let (server, mut client) = connected_test_named_pipe();
        let control = Arc::new(SecretConnectionControl::new(&server).unwrap());
        let reader_control = control.clone();
        let reader = server.try_clone().unwrap();
        let reader_thread = std::thread::spawn(move || {
            let raw = reader.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let op_event = SecretPipeEvent::new_manual_reset().unwrap();
            read_frame_overlapped(raw, &op_event, Some(reader_control.stop_event()))
        });
        // 让读先挂起；客户端保持静默（不写任何东西）。
        std::thread::sleep(Duration::from_millis(300));

        let raw = server.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let op_event = SecretPipeEvent::new_manual_reset().unwrap();
        let frame = serde_json::json!({
            "type": "secret.command",
            "id": "overlapped-server-write",
            "command": "ping",
        });
        let started = Instant::now();
        let outcome = write_frame_overlapped(raw, &op_event, None, &frame);
        assert!(
            matches!(outcome, SecretFrameIo::Written),
            "server-end write failed behind a pending read: {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "server-end write blocked behind the pending read"
        );
        // 客户端同步读到完整帧。
        let received = read_frame(&mut client).unwrap().unwrap();
        assert_eq!(received, frame);

        assert!(control.retire());
        let reader_outcome = reader_thread.join().unwrap();
        assert!(matches!(reader_outcome, SecretFrameIo::Stopped));
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
        let writer_inner = inner.clone();
        let writer_control = control.clone();
        let writer = std::thread::spawn(move || {
            use std::os::windows::io::AsRawHandle;
            let raw = server.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let stop_source = writer_control.clone();
            let op_event = SecretPipeEvent::new_manual_reset().unwrap();
            let frame_writer = OverlappedFrameWriter {
                pipe: raw,
                op_event,
                stop_event: stop_source.stop_event(),
            };
            run_connection_writer(
                frame_writer,
                receiver,
                writer_inner,
                "blocked-writer".to_string(),
                BrowserFamily::Firefox,
                generation,
                writer_control,
            );
        });
        // 512KB 远超 64KB 管道缓冲且客户端不读：写必然挂起在重叠 I/O 上。
        std::thread::sleep(Duration::from_millis(300));

        let _ = close_connection_generation(
            &inner,
            "blocked-writer",
            BrowserFamily::Firefox,
            generation,
            &control,
            "test-close",
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

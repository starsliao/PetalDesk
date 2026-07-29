use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const PROTOCOL_VERSION: u32 = 1;
const MAX_NATIVE_MESSAGE_BYTES: usize = 1024 * 1024;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

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

    let _cleanup = SessionCleanup(paths.clone());
    let mut last_heartbeat = SystemTime::now();
    loop {
        while let Ok(message) = incoming_rx.try_recv() {
            let message = message?;
            if message.kind != "extension.response" {
                continue;
            }
            let Some(id) = message.id.as_deref() else {
                continue;
            };
            if !is_safe_identifier(id) {
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

        for command_path in pending_commands(&paths.commands)? {
            let bytes =
                fs::read(&command_path).map_err(|error| format!("读取浏览器指令失败: {error}"))?;
            let command: Value = serde_json::from_slice(&bytes)
                .map_err(|error| format!("浏览器指令格式无效: {error}"))?;
            write_native_message(&mut output, &command)?;
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
}

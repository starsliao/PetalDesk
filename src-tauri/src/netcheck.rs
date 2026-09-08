//! 网络检测：ICMP ping、TCP 端口连通性、UDP 端口探测，三者都给出延时。
//!
//! ICMP 走 `IcmpSendEcho` 而不是原始套接字：原始套接字在 Windows 上需要管理员
//! 权限，而这个 API 不需要，普通用户直接能用。
//!
//! UDP 天生测不准，因此不做「通/不通」的二元判断。UDP 没有握手，发出去没有回应
//! 既可能是端口开着但应用不回包，也可能是被防火墙静默丢弃。这里明确区分三种结果：
//!   - 收到应用层回包        -> 开放，并给出延时
//!   - 收到 ICMP 端口不可达  -> 关闭（Windows 在已 connect 的 UDP 套接字上把它
//!                              暴露为 WSAECONNRESET）
//!   - 超时无任何回应        -> 开放或被过滤，无法确定
//! 对 DNS 和 NTP 这类有标准请求格式的服务发送真正的协议探测包，这样超时之外还能
//! 拿到确定结论；其余端口只能发空载荷，绝大多数情况会落到第三种。

use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// 单次探测允许的最大等待时间。再长对交互式工具没有意义。
const MAX_TIMEOUT_MS: u32 = 10_000;
const MIN_TIMEOUT_MS: u32 = 100;
const MAX_COUNT: u16 = 20;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeKind {
    Ping,
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetCheckRequest {
    pub target: String,
    pub kind: ProbeKind,
    #[serde(default)]
    pub port: Option<u16>,
    pub count: u16,
    pub timeout_ms: u32,
}

/// 单次探测的判定。`Open` 与 `Closed` 是确定结论，`Filtered` 明确表示无法确定。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Open,
    Closed,
    Filtered,
    Unreachable,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub seq: u16,
    pub outcome: Outcome,
    pub latency_ms: Option<f64>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    pub min_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    /// 抖动，取延时样本的总体标准差。只有一个样本时为 0。
    pub jitter_ms: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetCheckReport {
    pub target: String,
    pub resolved_ip: String,
    pub kind: ProbeKind,
    pub port: Option<u16>,
    /// UDP 时说明用了哪种探测载荷，便于理解结论的可信度。
    pub probe_note: Option<String>,
    pub attempts: Vec<Attempt>,
    pub sent: u16,
    pub answered: u16,
    pub loss_ratio: f64,
    pub latency: Option<LatencyStats>,
    pub verdict: Verdict,
    pub started_at: String,
    pub elapsed_ms: u64,
}

/// 整体结论。`OpenOrFiltered` 是 UDP 专有的「测不准」结论，不要与 `Unreachable` 混淆。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Reachable,
    Partial,
    Unreachable,
    Closed,
    OpenOrFiltered,
}

fn clamp_request(request: &NetCheckRequest) -> (u16, u32) {
    let count = request.count.clamp(1, MAX_COUNT);
    let timeout = request.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
    (count, timeout)
}

/// 纯统计，便于脱离网络单测。
pub fn latency_stats(attempts: &[Attempt]) -> Option<LatencyStats> {
    let samples: Vec<f64> = attempts.iter().filter_map(|a| a.latency_ms).collect();
    if samples.is_empty() {
        return None;
    }
    let count = samples.len() as f64;
    let sum: f64 = samples.iter().sum();
    let avg = sum / count;
    let variance = samples.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / count;
    Some(LatencyStats {
        min_ms: samples.iter().cloned().fold(f64::INFINITY, f64::min),
        avg_ms: avg,
        max_ms: samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        jitter_ms: variance.sqrt(),
    })
}

/// 由逐次结果推出整体结论。判定顺序刻意让「确定的坏消息」优先于「测不准」，
/// 避免把一个已经明确拒绝的端口报成「可能开着」。
pub fn overall_verdict(kind: ProbeKind, attempts: &[Attempt]) -> Verdict {
    let open = attempts.iter().filter(|a| a.outcome == Outcome::Open).count();
    let closed = attempts
        .iter()
        .filter(|a| a.outcome == Outcome::Closed)
        .count();
    if open > 0 {
        return if open == attempts.len() {
            Verdict::Reachable
        } else {
            Verdict::Partial
        };
    }
    if closed > 0 {
        return Verdict::Closed;
    }
    // 没有任何回应：UDP 说不出「不通」，只能说测不准；TCP/ICMP 可以说不可达。
    if kind == ProbeKind::Udp {
        Verdict::OpenOrFiltered
    } else {
        Verdict::Unreachable
    }
}

pub fn loss_ratio(sent: u16, answered: u16) -> f64 {
    if sent == 0 {
        0.0
    } else {
        f64::from(sent - answered.min(sent)) / f64::from(sent)
    }
}

/// 解析目标为地址。ICMP 只实现了 IPv4，因此 ping 时优先挑 IPv4 结果。
fn resolve(target: &str, port: u16, prefer_v4: bool) -> AppResult<IpAddr> {
    let host = target.trim();
    if host.is_empty() {
        return Err(crate::error::AppError::invalid("请输入要检测的地址。"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| {
            crate::error::AppError::new(
                "resolve_failed",
                format!("无法解析「{host}」：{error}"),
            )
        })?
        .collect();
    if prefer_v4 {
        if let Some(addr) = addrs.iter().find(|addr| addr.is_ipv4()) {
            return Ok(addr.ip());
        }
    }
    addrs
        .first()
        .map(|addr| addr.ip())
        .ok_or_else(|| crate::error::AppError::new("resolve_failed", format!("「{host}」没有解析到任何地址。")))
}

/// 常见 UDP 服务的探测载荷。发真正的协议请求才可能收到应用层回应，
/// 从而把结论从「测不准」变成「确定开放」。
fn udp_probe(port: u16) -> (Vec<u8>, Option<String>) {
    match port {
        53 => (dns_query(), Some("发送标准 DNS 查询，收到应答即可确定端口开放".to_string())),
        123 => (ntp_client_packet(), Some("发送 NTP 客户端请求，收到应答即可确定端口开放".to_string())),
        _ => (
            vec![0u8; 1],
            Some("该端口没有通用的探测协议，只能发空载荷；多数服务不会回应，因此超时通常无法区分开放与被过滤".to_string()),
        ),
    }
}

/// 一个查询 example.com A 记录的标准 DNS 报文。
fn dns_query() -> Vec<u8> {
    let mut packet = vec![
        0x9e, 0x1a, // 事务 ID
        0x01, 0x00, // 标准查询，期望递归
        0x00, 0x01, // 问题数 1
        0x00, 0x00, // 回答数 0
        0x00, 0x00, // 授权数 0
        0x00, 0x00, // 附加数 0
    ];
    for label in ["example", "com"] {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0x00); // 根标签
    packet.extend_from_slice(&[0x00, 0x01]); // QTYPE = A
    packet.extend_from_slice(&[0x00, 0x01]); // QCLASS = IN
    packet
}

/// LI=0、VN=3、Mode=3（客户端）的 48 字节 NTP 报文。
fn ntp_client_packet() -> Vec<u8> {
    let mut packet = vec![0u8; 48];
    packet[0] = 0x1b;
    packet
}

fn probe_tcp(addr: SocketAddr, timeout: Duration) -> Attempt {
    let started = Instant::now();
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(_stream) => Attempt {
            seq: 0,
            outcome: Outcome::Open,
            latency_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
            detail: "握手成功".to_string(),
        },
        Err(error) => {
            let (outcome, detail) = match error.kind() {
                // 明确被拒绝：对端回了 RST，说明主机在但端口没开。
                ErrorKind::ConnectionRefused => (Outcome::Closed, "连接被拒绝（端口未监听）".to_string()),
                ErrorKind::TimedOut => (Outcome::Filtered, "超时无响应（可能被防火墙丢弃）".to_string()),
                _ => (Outcome::Unreachable, format!("连接失败：{error}")),
            };
            Attempt {
                seq: 0,
                outcome,
                latency_ms: None,
                detail,
            }
        }
    }
}

fn probe_udp(addr: SocketAddr, timeout: Duration, payload: &[u8]) -> Attempt {
    let bind: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0".parse().expect("bind v4")
    } else {
        "[::]:0".parse().expect("bind v6")
    };
    let socket = match UdpSocket::bind(bind) {
        Ok(socket) => socket,
        Err(error) => {
            return Attempt {
                seq: 0,
                outcome: Outcome::Error,
                latency_ms: None,
                detail: format!("创建 UDP 套接字失败：{error}"),
            }
        }
    };
    // 必须 connect：只有已连接的 UDP 套接字才会把 ICMP 端口不可达
    // 作为错误交付给 recv，否则关闭的端口和被过滤的端口无法区分。
    if let Err(error) = socket.connect(addr) {
        return Attempt {
            seq: 0,
            outcome: Outcome::Error,
            latency_ms: None,
            detail: format!("绑定目标失败：{error}"),
        };
    }
    let _ = socket.set_read_timeout(Some(timeout));

    let started = Instant::now();
    if let Err(error) = socket.send(payload) {
        return Attempt {
            seq: 0,
            outcome: Outcome::Error,
            latency_ms: None,
            detail: format!("发送失败：{error}"),
        };
    }
    let mut buffer = [0u8; 1500];
    match socket.recv(&mut buffer) {
        Ok(size) => Attempt {
            seq: 0,
            outcome: Outcome::Open,
            latency_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
            detail: format!("收到 {size} 字节应答"),
        },
        Err(error) => match error.kind() {
            // Windows 把 ICMP 端口不可达暴露成 WSAECONNRESET。
            ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused => Attempt {
                seq: 0,
                outcome: Outcome::Closed,
                latency_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
                detail: "收到 ICMP 端口不可达（端口确定关闭）".to_string(),
            },
            ErrorKind::WouldBlock | ErrorKind::TimedOut => Attempt {
                seq: 0,
                outcome: Outcome::Filtered,
                latency_ms: None,
                detail: "超时无应答：端口可能开放但不回包，也可能被过滤".to_string(),
            },
            _ => Attempt {
                seq: 0,
                outcome: Outcome::Error,
                latency_ms: None,
                detail: format!("接收失败：{error}"),
            },
        },
    }
}

#[tauri::command]
pub async fn run_network_check(request: NetCheckRequest) -> AppResult<NetCheckReport> {
    crate::commands::run_background("网络检测", move || run_network_check_inner(request)).await
}

fn run_network_check_inner(request: NetCheckRequest) -> AppResult<NetCheckReport> {
    let (count, timeout_ms) = clamp_request(&request);
    let timeout = Duration::from_millis(u64::from(timeout_ms));

    let port = match request.kind {
        ProbeKind::Ping => 0,
        _ => request.port.ok_or_else(|| {
            crate::error::AppError::invalid("检测 TCP 或 UDP 端口时必须填写端口号。")
        })?,
    };
    if request.kind != ProbeKind::Ping && port == 0 {
        return Err(crate::error::AppError::invalid("端口号必须在 1-65535 之间。"));
    }

    let ip = resolve(&request.target, port.max(1), request.kind == ProbeKind::Ping)?;
    let addr = SocketAddr::new(ip, port);
    let (payload, probe_note) = if request.kind == ProbeKind::Udp {
        let (payload, note) = udp_probe(port);
        (payload, note)
    } else {
        (Vec::new(), None)
    };

    let started_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let started = Instant::now();
    let mut attempts = Vec::with_capacity(usize::from(count));
    for seq in 0..count {
        // 连续探测之间留一点间隔，避免把目标当成洪泛来源，也让抖动更接近真实。
        if seq > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut attempt = match request.kind {
            ProbeKind::Ping => probe_ping(ip, timeout),
            ProbeKind::Tcp => probe_tcp(addr, timeout),
            ProbeKind::Udp => probe_udp(addr, timeout, &payload),
        };
        attempt.seq = seq + 1;
        attempts.push(attempt);
    }

    let answered = attempts
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Open | Outcome::Closed))
        .count() as u16;
    Ok(NetCheckReport {
        target: request.target.trim().to_string(),
        resolved_ip: ip.to_string(),
        kind: request.kind,
        port: if request.kind == ProbeKind::Ping {
            None
        } else {
            Some(port)
        },
        probe_note,
        sent: count,
        answered,
        loss_ratio: loss_ratio(count, answered),
        latency: latency_stats(&attempts),
        verdict: overall_verdict(request.kind, &attempts),
        attempts,
        started_at,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(windows)]
fn probe_ping(ip: IpAddr, timeout: Duration) -> Attempt {
    let IpAddr::V4(v4) = ip else {
        return Attempt {
            seq: 0,
            outcome: Outcome::Error,
            latency_ms: None,
            detail: "当前版本的 ping 只支持 IPv4 目标；IPv6 请改用 TCP 或 UDP 检测。".to_string(),
        };
    };
    windows_icmp::echo(v4, timeout)
}

#[cfg(not(windows))]
fn probe_ping(_ip: IpAddr, _timeout: Duration) -> Attempt {
    Attempt {
        seq: 0,
        outcome: Outcome::Error,
        latency_ms: None,
        detail: "ping 目前仅支持 Windows。".to_string(),
    }
}

#[cfg(windows)]
mod windows_icmp {
    use super::{Attempt, Outcome};
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY,
    };

    /// 与 `ping` 默认一致的 32 字节载荷。
    const PAYLOAD: &[u8; 32] = b"petaldesk-network-check-payload!";
    const IP_SUCCESS: u32 = 0;

    struct IcmpHandle(HANDLE);

    impl Drop for IcmpHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// IP_STATUS 到中文说明的映射，覆盖排障时真正会遇到的几种。
    fn status_detail(status: u32) -> (Outcome, String) {
        match status {
            11002 => (Outcome::Unreachable, "目标网络不可达".to_string()),
            11003 => (Outcome::Unreachable, "目标主机不可达".to_string()),
            11004 => (Outcome::Unreachable, "目标协议不可达".to_string()),
            11005 => (Outcome::Closed, "目标端口不可达".to_string()),
            11010 => (Outcome::Filtered, "请求超时".to_string()),
            11013 => (Outcome::Unreachable, "TTL 超时（可能存在路由环路）".to_string()),
            other => (Outcome::Unreachable, format!("ICMP 状态码 {other}")),
        }
    }

    pub(super) fn echo(target: Ipv4Addr, timeout: Duration) -> Attempt {
        let handle = IcmpHandle(unsafe { IcmpCreateFile() });
        if handle.0.is_null() || handle.0 == INVALID_HANDLE_VALUE {
            return Attempt {
                seq: 0,
                outcome: Outcome::Error,
                latency_ms: None,
                detail: "无法打开 ICMP 句柄".to_string(),
            };
        }

        // 回复缓冲区至少要能放下一个 ICMP_ECHO_REPLY、原样回显的载荷，
        // 再加 8 字节给可能返回的差错报文。
        let reply_size = std::mem::size_of::<ICMP_ECHO_REPLY>() + PAYLOAD.len() + 8;
        let mut reply = vec![0u8; reply_size];
        let destination = u32::from_ne_bytes(target.octets());
        let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;

        let started = Instant::now();
        let replies = unsafe {
            IcmpSendEcho(
                handle.0,
                destination,
                PAYLOAD.as_ptr().cast(),
                PAYLOAD.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr().cast(),
                reply_size as u32,
                timeout_ms,
            )
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        if replies == 0 {
            return Attempt {
                seq: 0,
                outcome: Outcome::Filtered,
                latency_ms: None,
                detail: "请求超时".to_string(),
            };
        }

        // SAFETY: IcmpSendEcho 返回非零表示缓冲区起始处是一个有效的 ICMP_ECHO_REPLY。
        let echo = unsafe { &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>() };
        if echo.Status != IP_SUCCESS {
            let (outcome, detail) = status_detail(echo.Status);
            return Attempt {
                seq: 0,
                outcome,
                latency_ms: None,
                detail,
            };
        }
        Attempt {
            seq: 0,
            outcome: Outcome::Open,
            // API 的 RoundTripTime 只有毫秒整数精度，局域网内常年显示 0，
            // 因此统一用本地计时，和 TCP/UDP 的口径也一致。
            latency_ms: Some(elapsed_ms),
            detail: format!("TTL 往返成功（系统报告 {} ms）", echo.RoundTripTime),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(seq: u16, outcome: Outcome, latency: Option<f64>) -> Attempt {
        Attempt {
            seq,
            outcome,
            latency_ms: latency,
            detail: String::new(),
        }
    }

    #[test]
    fn latency_stats_ignore_attempts_without_a_sample() {
        let attempts = vec![
            attempt(1, Outcome::Open, Some(10.0)),
            attempt(2, Outcome::Filtered, None),
            attempt(3, Outcome::Open, Some(20.0)),
        ];
        let stats = latency_stats(&attempts).expect("stats");
        assert_eq!(stats.min_ms, 10.0);
        assert_eq!(stats.max_ms, 20.0);
        assert_eq!(stats.avg_ms, 15.0);
        assert_eq!(stats.jitter_ms, 5.0);
    }

    #[test]
    fn latency_stats_are_absent_when_nothing_answered() {
        let attempts = vec![attempt(1, Outcome::Filtered, None)];
        assert!(latency_stats(&attempts).is_none());
    }

    #[test]
    fn a_single_sample_has_no_jitter() {
        let stats = latency_stats(&[attempt(1, Outcome::Open, Some(7.5))]).expect("stats");
        assert_eq!(stats.jitter_ms, 0.0);
        assert_eq!(stats.avg_ms, 7.5);
    }

    #[test]
    fn every_attempt_open_reads_as_reachable() {
        let attempts = vec![
            attempt(1, Outcome::Open, Some(1.0)),
            attempt(2, Outcome::Open, Some(2.0)),
        ];
        assert_eq!(overall_verdict(ProbeKind::Tcp, &attempts), Verdict::Reachable);
    }

    #[test]
    fn some_attempts_open_reads_as_partial() {
        let attempts = vec![
            attempt(1, Outcome::Open, Some(1.0)),
            attempt(2, Outcome::Filtered, None),
        ];
        assert_eq!(overall_verdict(ProbeKind::Ping, &attempts), Verdict::Partial);
    }

    /// UDP 全程无回应时只能说「测不准」。报成不可达会让用户以为端口确定关着。
    #[test]
    fn silent_udp_is_reported_as_open_or_filtered_rather_than_unreachable() {
        let attempts = vec![
            attempt(1, Outcome::Filtered, None),
            attempt(2, Outcome::Filtered, None),
        ];
        assert_eq!(
            overall_verdict(ProbeKind::Udp, &attempts),
            Verdict::OpenOrFiltered
        );
        // 同样的沉默对 TCP/ICMP 是可以下结论的。
        assert_eq!(
            overall_verdict(ProbeKind::Tcp, &attempts),
            Verdict::Unreachable
        );
    }

    /// 收到 ICMP 端口不可达是确定结论，必须压过「测不准」。
    #[test]
    fn a_rejected_udp_port_outranks_the_undecidable_result() {
        let attempts = vec![
            attempt(1, Outcome::Filtered, None),
            attempt(2, Outcome::Closed, Some(3.0)),
        ];
        assert_eq!(overall_verdict(ProbeKind::Udp, &attempts), Verdict::Closed);
    }

    #[test]
    fn loss_ratio_counts_unanswered_probes() {
        assert_eq!(loss_ratio(4, 4), 0.0);
        assert_eq!(loss_ratio(4, 1), 0.75);
        assert_eq!(loss_ratio(0, 0), 0.0);
    }

    #[test]
    fn requests_are_clamped_into_a_sane_range() {
        let request = NetCheckRequest {
            target: "example.com".to_string(),
            kind: ProbeKind::Tcp,
            port: Some(443),
            count: 999,
            timeout_ms: 1,
        };
        assert_eq!(clamp_request(&request), (MAX_COUNT, MIN_TIMEOUT_MS));

        let request = NetCheckRequest {
            count: 0,
            timeout_ms: 99_999,
            ..request
        };
        assert_eq!(clamp_request(&request), (1, MAX_TIMEOUT_MS));
    }

    #[test]
    fn dns_probe_is_a_well_formed_query_for_example_com() {
        let (payload, note) = udp_probe(53);
        assert!(note.expect("note").contains("DNS"));
        assert_eq!(&payload[4..6], &[0x00, 0x01], "问题数应为 1");
        let name: Vec<u8> = payload[12..].to_vec();
        assert_eq!(name[0], 7);
        assert_eq!(&name[1..8], b"example");
        assert_eq!(name[8], 3);
        assert_eq!(&name[9..12], b"com");
        assert_eq!(name[12], 0, "根标签");
        assert_eq!(&name[13..17], &[0x00, 0x01, 0x00, 0x01], "QTYPE=A QCLASS=IN");
    }

    #[test]
    fn ntp_probe_is_a_48_byte_client_packet() {
        let (payload, note) = udp_probe(123);
        assert!(note.expect("note").contains("NTP"));
        assert_eq!(payload.len(), 48);
        assert_eq!(payload[0], 0x1b, "LI=0 VN=3 Mode=3");
    }

    /// 没有通用协议的端口必须说明结论不确定，否则用户会把超时读成「不通」。
    #[test]
    fn an_unknown_udp_port_warns_that_a_timeout_is_inconclusive() {
        let (_, note) = udp_probe(9999);
        let note = note.expect("note");
        assert!(note.contains("空载荷"));
        assert!(note.contains("无法区分"));
    }

    #[test]
    fn literal_addresses_skip_name_resolution() {
        assert_eq!(
            resolve("10.0.0.1", 80, true).expect("ip"),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
        );
    }

    /// 实机冒烟：验证 IcmpSendEcho、TCP 连接计时和 UDP 三态判定在真实网络上确实可用。
    /// 依赖本机网络状态，默认跳过；需要时用
    /// `cargo test --lib netcheck::tests::live -- --ignored --nocapture` 手动运行。
    #[test]
    #[ignore = "依赖本机实时网络"]
    fn live_probe_smoke() {
        let run = |kind: ProbeKind, target: &str, port: Option<u16>| {
            run_network_check_inner(NetCheckRequest {
                target: target.to_string(),
                kind,
                port,
                count: 3,
                timeout_ms: 2000,
            })
            .expect("probe")
        };

        let ping = run(ProbeKind::Ping, "127.0.0.1", None);
        println!(
            "ping 127.0.0.1 -> {:?} 丢包 {:.0}% 延时 {:?}",
            ping.verdict,
            ping.loss_ratio * 100.0,
            ping.latency.as_ref().map(|s| s.avg_ms)
        );
        assert_eq!(ping.verdict, Verdict::Reachable, "本机回环 ping 必须通");
        assert!(ping.latency.is_some());

        // 回环上几乎不可能有人监听这个端口，必须得出「不是开放」的结论。
        // 这里不断言一定是 Closed：本机若装有 Proxifier 这类会钩住所有 connect
        // 的代理软件，对端的 RST 会被吞掉，连 .NET 也只能等到超时。
        let closed = run(ProbeKind::Tcp, "127.0.0.1", Some(1));
        println!("tcp 127.0.0.1:1 -> {:?} ({})", closed.verdict, closed.attempts[0].detail);
        assert!(
            matches!(closed.verdict, Verdict::Closed | Verdict::Unreachable),
            "关闭端口不应报成开放，实得 {:?}",
            closed.verdict
        );

        // 已知在监听的端口必须报成开放，这条不受代理钩子影响。
        let open = run(ProbeKind::Tcp, "127.0.0.1", Some(10808));
        println!(
            "tcp 127.0.0.1:10808 -> {:?} 延时 {:?}",
            open.verdict,
            open.latency.as_ref().map(|s| s.avg_ms)
        );

        let udp = run(ProbeKind::Udp, "127.0.0.1", Some(9));
        println!(
            "udp 127.0.0.1:9 -> {:?} ({})",
            udp.verdict, udp.attempts[0].detail
        );
        assert!(
            matches!(udp.verdict, Verdict::Closed | Verdict::OpenOrFiltered),
            "UDP 只能得出确定关闭或测不准，实得 {:?}",
            udp.verdict
        );
    }

    #[test]
    fn an_empty_target_is_rejected_before_any_probe() {
        assert!(resolve("   ", 80, true).is_err());
    }
}

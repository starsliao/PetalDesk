//! 网络体检:枚举本机 TCP 连接,按状态、进程、目标端点聚合,并研判本地代理环路。
//!
//! 关键点:TIME_WAIT 连接在 Windows 上的属主 PID 是 0,按进程聚合永远归因不到。
//! 因此除了「按进程」还必须做「按目标端点」聚合,并把回环目标的端口反查成监听方
//! 进程 —— 这样两万条无主 TIME_WAIT 才能落到具体程序头上。
//!
//! 采集使用 `GetExtendedTcpTable` 而不是 netstat 或 `Get-NetTCPConnection`:后两者
//! 在两万条连接的量级上分别要数秒和直接超时返回空表,而这恰好是最需要体检的时候。

use crate::error::AppResult;
use serde::Serialize;
use std::collections::HashMap;

/// 聚合表格只展示前若干行,其余归入合计。
const TOP_ROWS: usize = 12;
/// 判定环路时,目标端点至少要收到这么多连接,避免把偶发的本地调用当成环路。
const LOOP_MIN_HITS: usize = 100;
/// 且该端点的属主进程自身出站要有这么高比例回到本机,才算「在原地空转」。
const LOOP_MIN_LOOPBACK_RATIO: f64 = 0.7;
const EPHEMERAL_WARNING_RATIO: f64 = 0.7;
const EPHEMERAL_CRITICAL_RATIO: f64 = 0.9;
/// netsh 读取失败时回退到 Windows Vista 之后的默认动态端口范围。
const DEFAULT_EPHEMERAL_START: u32 = 49152;
const DEFAULT_EPHEMERAL_COUNT: u32 = 16384;

/// 一条 TCP 记录。聚合逻辑只依赖这个结构,因此可以脱离 Windows API 单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    pub local_port: u16,
    pub remote: String,
    pub remote_port: u16,
    pub remote_is_loopback: bool,
    pub state: &'static str,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EphemeralRange {
    pub start: u32,
    pub count: u32,
}

impl Default for EphemeralRange {
    fn default() -> Self {
        Self {
            start: DEFAULT_EPHEMERAL_START,
            count: DEFAULT_EPHEMERAL_COUNT,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StateBucket {
    pub state: String,
    pub count: usize,
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralUsage {
    pub start: u32,
    pub end: u32,
    pub capacity: usize,
    pub used: usize,
    pub ratio: f64,
    /// `normal` / `warning` / `critical`,前端据此选配色和结论文案。
    pub level: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessBucket {
    pub pid: u32,
    pub name: String,
    pub count: usize,
    pub ratio: f64,
    pub states: Vec<StateBucket>,
    pub loopback_out: usize,
    pub external_out: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TargetBucket {
    pub endpoint: String,
    pub count: usize,
    pub ratio: f64,
    pub loopback: bool,
    /// 回环目标反查到的监听方进程;这是把无主 TIME_WAIT 归因到程序的关键。
    pub owner_pid: Option<u32>,
    pub owner_name: Option<String>,
    pub states: Vec<StateBucket>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoopCaller {
    pub pid: u32,
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoopFinding {
    pub endpoint: String,
    pub hits: usize,
    pub owner_pid: u32,
    pub owner_name: String,
    pub owner_loopback_out: usize,
    pub owner_external_out: usize,
    pub owner_loopback_ratio: f64,
    pub callers: Vec<LoopCaller>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkReport {
    pub sampled_at: String,
    pub sample_ms: u64,
    pub total: usize,
    /// 参与目标聚合的连接数,即排除 LISTENING 之后的条目。
    pub outbound_total: usize,
    pub states: Vec<StateBucket>,
    pub ephemeral: EphemeralUsage,
    pub processes: Vec<ProcessBucket>,
    pub targets: Vec<TargetBucket>,
    pub loops: Vec<LoopFinding>,
}

pub const LISTENING: &str = "LISTENING";

fn ratio_of(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

/// 把一组 (状态 -> 条数) 折成按条数降序的展示列表,条数相同时按名称稳定排序。
fn state_buckets(counts: HashMap<&'static str, usize>, total: usize) -> Vec<StateBucket> {
    let mut buckets: Vec<StateBucket> = counts
        .into_iter()
        .map(|(state, count)| StateBucket {
            state: state.to_string(),
            count,
            ratio: ratio_of(count, total),
        })
        .collect();
    buckets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.state.cmp(&right.state))
    });
    buckets
}

pub fn process_display_name(pid: u32, names: &HashMap<u32, String>) -> String {
    match pid {
        0 => "(无属主 · TIME_WAIT)".to_string(),
        4 => "System".to_string(),
        _ => names
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| format!("(已退出 {pid})")),
    }
}

fn ephemeral_usage(rows: &[ConnectionRow], range: EphemeralRange) -> EphemeralUsage {
    let start = range.start;
    let end = range.start.saturating_add(range.count).saturating_sub(1);
    let mut ports: Vec<u16> = rows
        .iter()
        .map(|row| row.local_port)
        .filter(|port| {
            let port = u32::from(*port);
            port >= start && port <= end
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    let capacity = range.count as usize;
    let used = ports.len();
    let ratio = ratio_of(used, capacity);
    let level = if ratio >= EPHEMERAL_CRITICAL_RATIO {
        "critical"
    } else if ratio >= EPHEMERAL_WARNING_RATIO {
        "warning"
    } else {
        "normal"
    };
    EphemeralUsage {
        start,
        end,
        capacity,
        used,
        ratio,
        level: level.to_string(),
    }
}

/// 纯函数聚合。与平台无关,便于直接单测。
pub fn build_report(
    rows: &[ConnectionRow],
    names: &HashMap<u32, String>,
    range: EphemeralRange,
    sampled_at: String,
    sample_ms: u64,
) -> NetworkReport {
    let total = rows.len();

    // 1. 状态总览
    let mut overall_states: HashMap<&'static str, usize> = HashMap::new();
    for row in rows {
        *overall_states.entry(row.state).or_insert(0) += 1;
    }
    let states = state_buckets(overall_states, total);

    // 2. 临时端口占用比
    let ephemeral = ephemeral_usage(rows, range);

    // 监听端口 -> 属主 PID。同一端口有多条监听记录时保留第一条即可,
    // 我们只需要知道「这个本地端口归谁」。
    let mut listener_owner: HashMap<u16, u32> = HashMap::new();
    for row in rows.iter().filter(|row| row.state == LISTENING) {
        listener_owner.entry(row.local_port).or_insert(row.pid);
    }

    let outbound: Vec<&ConnectionRow> = rows.iter().filter(|row| row.state != LISTENING).collect();
    let outbound_total = outbound.len();

    // 3. 按进程聚合
    let mut per_process: HashMap<u32, (usize, HashMap<&'static str, usize>, usize, usize)> =
        HashMap::new();
    for row in rows {
        let entry = per_process
            .entry(row.pid)
            .or_insert_with(|| (0, HashMap::new(), 0, 0));
        entry.0 += 1;
        *entry.1.entry(row.state).or_insert(0) += 1;
        if row.state != LISTENING {
            if row.remote_is_loopback {
                entry.2 += 1;
            } else {
                entry.3 += 1;
            }
        }
    }
    let mut processes: Vec<ProcessBucket> = per_process
        .into_iter()
        .map(
            |(pid, (count, state_counts, loopback_out, external_out))| ProcessBucket {
                pid,
                name: process_display_name(pid, names),
                count,
                ratio: ratio_of(count, total),
                states: state_buckets(state_counts, count),
                loopback_out,
                external_out,
            },
        )
        .collect();
    processes.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.pid.cmp(&right.pid))
    });
    processes.truncate(TOP_ROWS);

    // 4. 按目标端点聚合 —— TIME_WAIT 的真正归因方式
    let mut per_target: HashMap<&str, (usize, HashMap<&'static str, usize>, bool, u16)> =
        HashMap::new();
    for row in &outbound {
        let entry = per_target
            .entry(row.remote.as_str())
            .or_insert_with(|| (0, HashMap::new(), row.remote_is_loopback, row.remote_port));
        entry.0 += 1;
        *entry.1.entry(row.state).or_insert(0) += 1;
    }
    let mut targets: Vec<TargetBucket> = per_target
        .into_iter()
        .map(|(endpoint, (count, state_counts, loopback, remote_port))| {
            let owner_pid = if loopback {
                listener_owner.get(&remote_port).copied()
            } else {
                None
            };
            TargetBucket {
                endpoint: endpoint.to_string(),
                count,
                ratio: ratio_of(count, outbound_total),
                loopback,
                owner_pid,
                owner_name: owner_pid.map(|pid| process_display_name(pid, names)),
                states: state_buckets(state_counts, count),
            }
        })
        .collect();
    targets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.endpoint.cmp(&right.endpoint))
    });
    targets.truncate(TOP_ROWS);

    // 5. 环路研判
    let loops = detect_loops(&outbound, &targets, names);

    NetworkReport {
        sampled_at,
        sample_ms,
        total,
        outbound_total,
        states,
        ephemeral,
        processes,
        targets,
        loops,
    }
}

/// 环路判据:某个回环端点被大量打入,而它的属主进程自身出站几乎全部又回到本机 ——
/// 说明流量在「A 转给 B、B 又被转回 A」之间空转,而不是真的发出去了。
fn detect_loops(
    outbound: &[&ConnectionRow],
    targets: &[TargetBucket],
    names: &HashMap<u32, String>,
) -> Vec<LoopFinding> {
    let mut findings = Vec::new();
    for target in targets {
        if !target.loopback || target.count < LOOP_MIN_HITS {
            continue;
        }
        // PID 0/4 是无属主与系统进程,不适用「进程自身出站」这条判据。
        let Some(owner_pid) = target.owner_pid.filter(|pid| *pid > 4) else {
            continue;
        };

        let mut loopback_out = 0usize;
        let mut external_out = 0usize;
        for row in outbound.iter().filter(|row| row.pid == owner_pid) {
            if row.remote_is_loopback {
                loopback_out += 1;
            } else {
                external_out += 1;
            }
        }
        let owner_total = loopback_out + external_out;
        if owner_total == 0 {
            continue;
        }
        let owner_loopback_ratio = ratio_of(loopback_out, owner_total);
        if owner_loopback_ratio < LOOP_MIN_LOOPBACK_RATIO {
            continue;
        }

        // 谁在往这个端点打:直接指出上游是哪个程序。
        let mut caller_counts: HashMap<u32, usize> = HashMap::new();
        for row in outbound
            .iter()
            .filter(|row| row.remote == target.endpoint && row.pid > 0)
        {
            *caller_counts.entry(row.pid).or_insert(0) += 1;
        }
        let mut callers: Vec<LoopCaller> = caller_counts
            .into_iter()
            .map(|(pid, count)| LoopCaller {
                pid,
                name: process_display_name(pid, names),
                count,
            })
            .collect();
        callers.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.pid.cmp(&right.pid))
        });
        callers.truncate(5);

        findings.push(LoopFinding {
            endpoint: target.endpoint.clone(),
            hits: target.count,
            owner_pid,
            owner_name: process_display_name(owner_pid, names),
            owner_loopback_out: loopback_out,
            owner_external_out: external_out,
            owner_loopback_ratio,
            callers,
        });
    }
    findings
}

#[tauri::command]
pub async fn collect_network_report() -> AppResult<NetworkReport> {
    crate::commands::run_background("采集网络连接", collect_network_report_inner).await
}

#[cfg(windows)]
fn collect_network_report_inner() -> AppResult<NetworkReport> {
    let started = std::time::Instant::now();
    let rows = platform::collect_rows()?;
    let names = platform::process_names();
    let range = platform::ephemeral_range();
    let sample_ms = started.elapsed().as_millis() as u64;
    let sampled_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    Ok(build_report(&rows, &names, range, sampled_at, sample_ms))
}

#[cfg(not(windows))]
fn collect_network_report_inner() -> AppResult<NetworkReport> {
    Err(crate::error::AppError::new(
        "unsupported_platform",
        "网络体检目前仅支持 Windows。",
    ))
}

#[cfg(windows)]
mod platform {
    use super::{ConnectionRow, EphemeralRange};
    use crate::error::{AppError, AppResult};
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, INVALID_HANDLE_VALUE, NO_ERROR,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    /// MIB_TCP_STATE 数值到 netstat 习惯命名的映射。
    fn state_name(state: u32) -> &'static str {
        match state {
            1 => "CLOSED",
            2 => super::LISTENING,
            3 => "SYN_SENT",
            4 => "SYN_RECEIVED",
            5 => "ESTABLISHED",
            6 => "FIN_WAIT_1",
            7 => "FIN_WAIT_2",
            8 => "CLOSE_WAIT",
            9 => "CLOSING",
            10 => "LAST_ACK",
            11 => "TIME_WAIT",
            12 => "DELETE_TCB",
            _ => "UNKNOWN",
        }
    }

    /// 表里的端口是网络字节序,且只有低 16 位有效。
    fn port_of(raw: u32) -> u16 {
        u16::from_be((raw & 0xFFFF) as u16)
    }

    /// 按 8 字节对齐分配缓冲区:MIB_TCP6TABLE_OWNER_PID 里有 u32 与数组混排,
    /// 用 Vec<u8> 只能保证 1 字节对齐,解引用是未定义行为。
    fn table_buffer(bytes: u32) -> Vec<u64> {
        vec![0u64; (bytes as usize).div_ceil(8).max(1)]
    }

    fn fetch_table(family: u32) -> AppResult<Vec<u64>> {
        let mut size: u32 = 0;
        let mut buffer = table_buffer(0);
        // 表大小会在两次调用之间变化,重试几轮直到拿到一致的快照。
        for _ in 0..6 {
            let result = unsafe {
                GetExtendedTcpTable(
                    buffer.as_mut_ptr().cast(),
                    &mut size,
                    0,
                    family,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            };
            if result == NO_ERROR {
                return Ok(buffer);
            }
            if result != ERROR_INSUFFICIENT_BUFFER {
                return Err(AppError::new(
                    "network_query_failed",
                    format!("读取 TCP 连接表失败(错误码 {result})"),
                ));
            }
            buffer = table_buffer(size);
        }
        Err(AppError::new(
            "network_query_failed",
            "TCP 连接表持续变化,未能取得一致快照,请重试。",
        ))
    }

    pub(super) fn collect_rows() -> AppResult<Vec<ConnectionRow>> {
        let mut rows = Vec::new();

        let ipv4 = fetch_table(u32::from(AF_INET))?;
        // SAFETY: 缓冲区由 GetExtendedTcpTable 按 MIB_TCPTABLE_OWNER_PID 布局填充,
        // 且已按 8 字节对齐分配。
        unsafe {
            let table = &*ipv4.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
            let entries = std::slice::from_raw_parts(
                table.table.as_ptr().cast::<MIB_TCPROW_OWNER_PID>(),
                table.dwNumEntries as usize,
            );
            for entry in entries {
                let remote_addr = Ipv4Addr::from(entry.dwRemoteAddr.to_ne_bytes());
                let remote_port = port_of(entry.dwRemotePort);
                rows.push(ConnectionRow {
                    local_port: port_of(entry.dwLocalPort),
                    remote: format!("{remote_addr}:{remote_port}"),
                    remote_port,
                    remote_is_loopback: remote_addr.is_loopback(),
                    state: state_name(entry.dwState),
                    pid: entry.dwOwningPid,
                });
            }
        }

        let ipv6 = fetch_table(u32::from(AF_INET6))?;
        // SAFETY: 同上,按 MIB_TCP6TABLE_OWNER_PID 布局解释。
        unsafe {
            let table = &*ipv6.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
            let entries = std::slice::from_raw_parts(
                table.table.as_ptr().cast::<MIB_TCP6ROW_OWNER_PID>(),
                table.dwNumEntries as usize,
            );
            for entry in entries {
                let remote_addr = Ipv6Addr::from(entry.ucRemoteAddr);
                let remote_port = port_of(entry.dwRemotePort);
                rows.push(ConnectionRow {
                    local_port: port_of(entry.dwLocalPort),
                    remote: format!("[{remote_addr}]:{remote_port}"),
                    remote_port,
                    remote_is_loopback: remote_addr.is_loopback(),
                    state: state_name(entry.dwState),
                    pid: entry.dwOwningPid,
                });
            }
        }

        Ok(rows)
    }

    struct Snapshot(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for Snapshot {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub(super) fn process_names() -> HashMap<u32, String> {
        let mut names = HashMap::new();
        let snapshot = Snapshot(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) });
        if snapshot.0.is_null() || snapshot.0 == INVALID_HANDLE_VALUE {
            return names;
        }
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
            return names;
        }
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            // 与任务管理器一致地去掉 .exe 后缀,表格更紧凑。
            let name = name.strip_suffix(".exe").unwrap_or(&name).to_string();
            names.insert(entry.th32ProcessID, name);
            if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
                break;
            }
        }
        names
    }

    /// 动态端口范围没有公开的 Win32 查询接口,只能问 netsh。输出是本地化文本,
    /// 但两个数字的位置固定,按整数提取即可;失败则回退到系统默认范围。
    pub(super) fn ephemeral_range() -> EphemeralRange {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let output = std::process::Command::new("netsh")
            .args(["int", "ipv4", "show", "dynamicport", "tcp"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let Ok(output) = output else {
            return EphemeralRange::default();
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let numbers: Vec<u32> = text
            .split(|character: char| !character.is_ascii_digit())
            .filter(|token| !token.is_empty())
            .filter_map(|token| token.parse::<u32>().ok())
            .collect();
        match numbers.as_slice() {
            [start, count, ..] if *start > 1024 && *count > 0 && start + count <= 65_536 => {
                EphemeralRange {
                    start: *start,
                    count: *count,
                }
            }
            _ => EphemeralRange::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, local_port: u16, remote: &str, state: &'static str) -> ConnectionRow {
        let remote_port = remote
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(0);
        ConnectionRow {
            local_port,
            remote: remote.to_string(),
            remote_port,
            remote_is_loopback: remote.starts_with("127.") || remote.starts_with("[::1]"),
            state,
            pid,
        }
    }

    fn names() -> HashMap<u32, String> {
        HashMap::from([
            (100, "xray".to_string()),
            (200, "Proxifier".to_string()),
            (300, "firefox".to_string()),
        ])
    }

    fn report(rows: &[ConnectionRow]) -> NetworkReport {
        build_report(
            rows,
            &names(),
            EphemeralRange {
                start: 49152,
                count: 100,
            },
            "2026-09-08 12:00:00".to_string(),
            7,
        )
    }

    /// 造一个 xray/Proxifier 互打的环路现场:大量 TIME_WAIT 指向 xray 的 SOCKS 口,
    /// 而 xray 自身出站几乎全部回到本机。
    fn loop_scene() -> Vec<ConnectionRow> {
        let mut rows = vec![row(100, 10808, "0.0.0.0:0", LISTENING)];
        for index in 0..150 {
            // TIME_WAIT 已经脱离属主,PID 记为 0 —— 正是按进程归因不到的那部分。
            rows.push(row(0, 50000 + index, "127.0.0.1:10808", "TIME_WAIT"));
        }
        for index in 0..40 {
            rows.push(row(200, 51000 + index, "127.0.0.1:10808", "ESTABLISHED"));
        }
        for index in 0..90 {
            // xray 自己的出站又被劫持回本机,只有少量真的发了出去。
            rows.push(row(100, 52000 + index, "127.0.0.1:60001", "ESTABLISHED"));
        }
        rows.push(row(100, 52999, "8.8.8.8:443", "ESTABLISHED"));
        rows
    }

    #[test]
    fn summarizes_states_with_ratios() {
        let rows = vec![
            row(0, 50001, "127.0.0.1:10808", "TIME_WAIT"),
            row(0, 50002, "127.0.0.1:10808", "TIME_WAIT"),
            row(300, 50003, "1.1.1.1:443", "ESTABLISHED"),
            row(300, 443, "0.0.0.0:0", LISTENING),
        ];
        let report = report(&rows);
        assert_eq!(report.total, 4);
        assert_eq!(report.outbound_total, 3);
        assert_eq!(report.states[0].state, "TIME_WAIT");
        assert_eq!(report.states[0].count, 2);
        assert!((report.states[0].ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn counts_distinct_ephemeral_ports_only_inside_the_configured_range() {
        let rows = vec![
            row(300, 49152, "1.1.1.1:443", "ESTABLISHED"),
            // 同一个本地端口重复出现只算一次占用。
            row(300, 49152, "1.1.1.1:80", "TIME_WAIT"),
            row(300, 49153, "1.1.1.1:443", "ESTABLISHED"),
            // 低于起始端口,不属于动态范围。
            row(300, 443, "1.1.1.1:443", "ESTABLISHED"),
            // 超出 49152+100-1 = 49251 的上界。
            row(300, 49252, "1.1.1.1:443", "ESTABLISHED"),
        ];
        let usage = report(&rows).ephemeral;
        assert_eq!(usage.start, 49152);
        assert_eq!(usage.end, 49251);
        assert_eq!(usage.capacity, 100);
        assert_eq!(usage.used, 2);
        assert_eq!(usage.level, "normal");
    }

    #[test]
    fn flags_ephemeral_exhaustion_levels() {
        let warning: Vec<ConnectionRow> = (0..70)
            .map(|index| row(300, 49152 + index, "1.1.1.1:443", "ESTABLISHED"))
            .collect();
        assert_eq!(report(&warning).ephemeral.level, "warning");

        let critical: Vec<ConnectionRow> = (0..90)
            .map(|index| row(300, 49152 + index, "1.1.1.1:443", "ESTABLISHED"))
            .collect();
        assert_eq!(report(&critical).ephemeral.level, "critical");
    }

    #[test]
    fn names_the_unowned_time_wait_bucket_instead_of_leaving_pid_zero_bare() {
        let report = report(&loop_scene());
        let unowned = report
            .processes
            .iter()
            .find(|bucket| bucket.pid == 0)
            .expect("pid 0 bucket");
        assert_eq!(unowned.name, "(无属主 · TIME_WAIT)");
        assert_eq!(unowned.count, 150);
    }

    /// 这条是整个工具的核心:按进程只能看到「PID 0 有 150 条」,
    /// 按目标端点才能看出这 150 条其实全是打给 xray 的。
    #[test]
    fn attributes_unowned_time_wait_to_the_listening_process_via_target_aggregation() {
        let report = report(&loop_scene());
        let top = &report.targets[0];
        assert_eq!(top.endpoint, "127.0.0.1:10808");
        assert_eq!(top.count, 190);
        assert!(top.loopback);
        assert_eq!(top.owner_pid, Some(100));
        assert_eq!(top.owner_name.as_deref(), Some("xray"));
        assert_eq!(top.states[0].state, "TIME_WAIT");
        assert_eq!(top.states[0].count, 150);
    }

    #[test]
    fn detects_the_proxy_loop_and_names_its_callers() {
        let report = report(&loop_scene());
        assert_eq!(report.loops.len(), 1);
        let finding = &report.loops[0];
        assert_eq!(finding.endpoint, "127.0.0.1:10808");
        assert_eq!(finding.hits, 190);
        assert_eq!(finding.owner_name, "xray");
        assert_eq!(finding.owner_loopback_out, 90);
        assert_eq!(finding.owner_external_out, 1);
        assert!(finding.owner_loopback_ratio > 0.98);
        assert_eq!(finding.callers[0].name, "Proxifier");
        assert_eq!(finding.callers[0].count, 40);
    }

    #[test]
    fn does_not_flag_a_busy_local_service_that_actually_reaches_the_network() {
        let mut rows = vec![row(100, 8080, "0.0.0.0:0", LISTENING)];
        for index in 0..150 {
            rows.push(row(300, 50000 + index, "127.0.0.1:8080", "ESTABLISHED"));
        }
        // 属主自身的出站绝大多数是真实外网,不构成空转。
        for index in 0..100 {
            rows.push(row(100, 51000 + index, "93.184.216.34:443", "ESTABLISHED"));
        }
        assert!(report(&rows).loops.is_empty());
    }

    #[test]
    fn ignores_loopback_targets_below_the_hit_threshold() {
        let mut rows = vec![row(100, 9000, "0.0.0.0:0", LISTENING)];
        for index in 0..20 {
            rows.push(row(200, 50000 + index, "127.0.0.1:9000", "ESTABLISHED"));
        }
        for index in 0..20 {
            rows.push(row(100, 51000 + index, "127.0.0.1:9999", "ESTABLISHED"));
        }
        assert!(report(&rows).loops.is_empty());
    }

    #[test]
    fn leaves_external_targets_unattributed() {
        let rows = vec![
            row(300, 50001, "93.184.216.34:443", "ESTABLISHED"),
            row(300, 50002, "93.184.216.34:443", "TIME_WAIT"),
        ];
        let report = report(&rows);
        let top = &report.targets[0];
        assert!(!top.loopback);
        assert_eq!(top.owner_pid, None);
        assert_eq!(top.owner_name, None);
    }

    /// 实机冒烟:验证 GetExtendedTcpTable、进程名解析和动态端口范围在真实系统上确实可用。
    /// 结果依赖当前机器状态,因此默认跳过,需要时用
    /// `cargo test --lib network::tests::live -- --ignored --nocapture` 手动运行。
    #[cfg(windows)]
    #[test]
    #[ignore = "依赖本机实时网络状态"]
    fn live_collection_smoke() {
        let report = collect_network_report_inner().expect("采集失败");
        println!(
            "采样 {} · {} ms · {} 条记录(其中出站 {} 条)",
            report.sampled_at, report.sample_ms, report.total, report.outbound_total
        );
        println!(
            "临时端口 {}-{}:已用 {} / {} = {:.1}% [{}]",
            report.ephemeral.start,
            report.ephemeral.end,
            report.ephemeral.used,
            report.ephemeral.capacity,
            report.ephemeral.ratio * 100.0,
            report.ephemeral.level
        );
        println!("-- 按进程 --");
        for bucket in report.processes.iter().take(5) {
            println!(
                "  {:<6} {:<24} {:>7} {:>6.1}%",
                bucket.pid,
                bucket.name,
                bucket.count,
                bucket.ratio * 100.0
            );
        }
        println!("-- 按目标端点 --");
        for target in report.targets.iter().take(5) {
            println!(
                "  {:<24} {:>7} {:>6.1}%  归属 {}",
                target.endpoint,
                target.count,
                target.ratio * 100.0,
                target.owner_name.as_deref().unwrap_or("-")
            );
        }
        println!("-- 环路 --");
        for finding in &report.loops {
            println!(
                "  {} 被打入 {} 条,归属 {},自身 {:.1}% 出站在空转",
                finding.endpoint,
                finding.hits,
                finding.owner_name,
                finding.owner_loopback_ratio * 100.0
            );
            for caller in &finding.callers {
                println!("     <- {:<20} {} 条", caller.name, caller.count);
            }
        }

        assert!(report.total > 0, "本机应当至少有一条 TCP 记录");
        assert!(report.ephemeral.capacity > 0);
        // 进程名必须解析出来,否则表格里全是 PID,工具就失去意义了。
        assert!(
            report
                .processes
                .iter()
                .any(|bucket| bucket.pid > 4 && !bucket.name.starts_with('(')),
            "应当至少解析出一个真实进程名"
        );
    }
}

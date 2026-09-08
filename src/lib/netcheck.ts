import { invoke } from "@tauri-apps/api/core";

export type ProbeKind = "ping" | "tcp" | "udp";
export type Outcome = "open" | "closed" | "filtered" | "unreachable" | "error";
export type Verdict = "reachable" | "partial" | "unreachable" | "closed" | "open_or_filtered";

export interface Attempt {
  seq: number;
  outcome: Outcome;
  latencyMs: number | null;
  detail: string;
}

export interface LatencyStats {
  minMs: number;
  avgMs: number;
  maxMs: number;
  jitterMs: number;
}

export interface NetCheckReport {
  target: string;
  resolvedIp: string;
  kind: ProbeKind;
  port: number | null;
  probeNote: string | null;
  attempts: Attempt[];
  sent: number;
  answered: number;
  lossRatio: number;
  latency: LatencyStats | null;
  verdict: Verdict;
  startedAt: string;
  elapsedMs: number;
}

export interface NetCheckRequest {
  target: string;
  kind: ProbeKind;
  port?: number | null;
  count: number;
  timeoutMs: number;
}

export interface NetCheckApi {
  isDesktop(): boolean;
  run(request: NetCheckRequest): Promise<NetCheckReport>;
}

interface AppError {
  code?: string;
  message: string;
}

function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) {
    return String((error as AppError).message);
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as AppError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "网络检测失败，请稍后重试。";
}

export const netCheckApi: NetCheckApi = {
  isDesktop: isDesktopRuntime,

  async run(request: NetCheckRequest): Promise<NetCheckReport> {
    if (!isDesktopRuntime()) {
      throw new Error("网络检测需要在飞花桌面端运行。");
    }
    try {
      return await invoke<NetCheckReport>("run_network_check", { request });
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },
};

export const PROBE_KINDS: ReadonlyArray<{ value: ProbeKind; label: string; needsPort: boolean }> = [
  { value: "ping", label: "Ping", needsPort: false },
  { value: "tcp", label: "TCP 端口", needsPort: true },
  { value: "udp", label: "UDP 端口", needsPort: true },
];

export function needsPort(kind: ProbeKind): boolean {
  return kind !== "ping";
}

export function formatMs(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return value >= 100 ? `${value.toFixed(0)} ms` : `${value.toFixed(2)} ms`;
}

export function formatPercent(ratio: number): string {
  return `${(ratio * 100).toFixed(0)}%`;
}

const OUTCOME_LABELS: Record<Outcome, string> = {
  open: "有响应",
  closed: "端口关闭",
  filtered: "无响应",
  unreachable: "不可达",
  error: "出错",
};

export function outcomeLabel(outcome: Outcome): string {
  return OUTCOME_LABELS[outcome] ?? outcome;
}

/// 结论分三档配色：确定好、确定坏、测不准。`open_or_filtered` 必须是中性的
/// warning 而不是 bad —— 它表示无法确定，不是「不通」。
export function verdictTone(verdict: Verdict): "good" | "bad" | "warning" {
  if (verdict === "reachable") return "good";
  if (verdict === "partial" || verdict === "open_or_filtered") return "warning";
  return "bad";
}

export function verdictLabel(verdict: Verdict): string {
  switch (verdict) {
    case "reachable":
      return "连通";
    case "partial":
      return "时通时断";
    case "closed":
      return "端口关闭";
    case "open_or_filtered":
      return "开放或被过滤（无法确定）";
    default:
      return "不可达";
  }
}

/// 一句话结论。UDP 的措辞刻意不说「不通」——没有回应本来就无法区分
/// 「端口开着但不回包」和「被防火墙丢弃」。
export function headline(report: NetCheckReport): string {
  const where =
    report.port === null
      ? report.resolvedIp
      : `${report.resolvedIp}:${report.port}`;
  switch (report.verdict) {
    case "reachable":
      return `${where} 连通，${report.sent} 次探测全部有响应，平均 ${formatMs(
        report.latency?.avgMs ?? null,
      )}。`;
    case "partial":
      return `${where} 时通时断：${report.sent} 次中 ${report.answered} 次有响应，丢包 ${formatPercent(
        report.lossRatio,
      )}。`;
    case "closed":
      return `${where} 端口关闭：对端明确拒绝了连接。`;
    case "open_or_filtered":
      return `${where} 无法确定：${report.sent} 次探测都没有收到应答。UDP 没有握手，这既可能是端口开着但服务不回包，也可能是被防火墙丢弃。`;
    default:
      return `${where} 不可达：${report.sent} 次探测都没有得到响应。`;
  }
}

/// 端口号校验。返回 null 表示合法。
export function portError(kind: ProbeKind, port: string): string | null {
  if (!needsPort(kind)) return null;
  const trimmed = port.trim();
  if (trimmed.length === 0) return "请填写端口号。";
  if (!/^\d+$/.test(trimmed)) return "端口号只能是数字。";
  const value = Number(trimmed);
  if (value < 1 || value > 65535) return "端口号必须在 1-65535 之间。";
  return null;
}

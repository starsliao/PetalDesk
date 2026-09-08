import { invoke } from "@tauri-apps/api/core";

/// 与 `src-tauri/src/network.rs` 的 NetworkReport 一一对应。
export interface StateBucket {
  state: string;
  count: number;
  ratio: number;
}

export interface EphemeralUsage {
  start: number;
  end: number;
  capacity: number;
  used: number;
  ratio: number;
  level: EphemeralLevel;
}

export type EphemeralLevel = "normal" | "warning" | "critical";

export interface ProcessBucket {
  pid: number;
  name: string;
  count: number;
  ratio: number;
  states: StateBucket[];
  loopbackOut: number;
  externalOut: number;
}

export interface TargetBucket {
  endpoint: string;
  count: number;
  ratio: number;
  loopback: boolean;
  ownerPid: number | null;
  ownerName: string | null;
  states: StateBucket[];
}

export interface LoopCaller {
  pid: number;
  name: string;
  count: number;
}

export interface LoopFinding {
  endpoint: string;
  hits: number;
  ownerPid: number;
  ownerName: string;
  ownerLoopbackOut: number;
  ownerExternalOut: number;
  ownerLoopbackRatio: number;
  callers: LoopCaller[];
}

export interface NetworkReport {
  sampledAt: string;
  sampleMs: number;
  total: number;
  outboundTotal: number;
  states: StateBucket[];
  ephemeral: EphemeralUsage;
  processes: ProcessBucket[];
  targets: TargetBucket[];
  loops: LoopFinding[];
}

export interface NetworkApi {
  isDesktop(): boolean;
  collect(): Promise<NetworkReport>;
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
  return "网络体检失败，请稍后重试。";
}

export const networkApi: NetworkApi = {
  isDesktop: isDesktopRuntime,

  async collect(): Promise<NetworkReport> {
    if (!isDesktopRuntime()) {
      throw new Error("网络体检需要在飞花桌面端运行。");
    }
    try {
      return await invoke<NetworkReport>("collect_network_report");
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },
};

export function formatCount(value: number): string {
  return value.toLocaleString("zh-CN");
}

export function formatRatio(ratio: number): string {
  return `${(ratio * 100).toFixed(1)}%`;
}

/// 条形图宽度按百分比给出。极小但非零的占比至少保留 1%，否则视觉上等同于 0。
export function barWidth(ratio: number): string {
  if (ratio <= 0) return "0%";
  return `${Math.max(1, Math.min(100, ratio * 100)).toFixed(2)}%`;
}

const EPHEMERAL_VERDICTS: Record<EphemeralLevel, string> = {
  normal: "正常",
  warning: "接近耗尽，新建连接开始有风险",
  critical: "危险，随时可能任何程序都连不出去",
};

export function ephemeralVerdict(usage: EphemeralUsage): string {
  return EPHEMERAL_VERDICTS[usage.level] ?? EPHEMERAL_VERDICTS.normal;
}

/// 属主为 0 的连接是已脱离进程的 TIME_WAIT，按进程永远归因不到。
/// 用它来提示用户「这一坨要去目标聚合里看」。
export function unownedShare(report: NetworkReport): number {
  const unowned = report.processes.find((bucket) => bucket.pid === 0);
  return unowned ? unowned.ratio : 0;
}

/// 一句话结论。优先播报环路，其次端口耗尽，最后才是「未见异常」。
export function headline(report: NetworkReport): string {
  const [finding] = report.loops;
  if (finding) {
    return `${finding.ownerName} 与上游程序形成本地代理环路：${finding.endpoint} 被打入 ${formatCount(
      finding.hits,
    )} 条连接，而 ${finding.ownerName} 自身 ${formatRatio(
      finding.ownerLoopbackRatio,
    )} 的出站又回到本机，等于在原地空转。`;
  }
  if (report.ephemeral.level !== "normal") {
    return `临时端口已用掉 ${formatCount(report.ephemeral.used)} / ${formatCount(
      report.ephemeral.capacity,
    )}（${formatRatio(report.ephemeral.ratio)}），${ephemeralVerdict(report.ephemeral)}。`;
  }
  return `共 ${formatCount(report.total)} 条 TCP 连接，临时端口占用 ${formatRatio(
    report.ephemeral.ratio,
  )}，未见异常。`;
}

/// 目标聚合行的归因文案：回环目标能反查到监听进程时直接点名，
/// 这正是把无主 TIME_WAIT 落到具体程序头上的那一步。
export function targetOwnerLabel(target: TargetBucket): string {
  if (target.ownerName) return `${target.ownerName} (${target.ownerPid})`;
  if (target.loopback) return "本机（监听已关闭）";
  return "外部地址";
}

export function stateSummary(states: StateBucket[], limit = 3): string {
  return states
    .slice(0, limit)
    .map((bucket) => `${bucket.state} ${formatCount(bucket.count)}`)
    .join("  ");
}

import { describe, expect, it } from "vitest";
import {
  formatMs,
  formatPercent,
  headline,
  needsPort,
  outcomeLabel,
  portError,
  verdictLabel,
  verdictTone,
  type NetCheckReport,
} from "./netcheck";

function report(overrides: Partial<NetCheckReport> = {}): NetCheckReport {
  return {
    target: "github.com",
    resolvedIp: "20.205.243.166",
    kind: "tcp",
    port: 443,
    probeNote: null,
    attempts: [],
    sent: 4,
    answered: 4,
    lossRatio: 0,
    latency: { minMs: 10, avgMs: 12.5, maxMs: 15, jitterMs: 2 },
    verdict: "reachable",
    startedAt: "2026-09-08 12:00:00",
    elapsedMs: 800,
    ...overrides,
  };
}

describe("needsPort", () => {
  it("only asks for a port on the port probes", () => {
    expect(needsPort("ping")).toBe(false);
    expect(needsPort("tcp")).toBe(true);
    expect(needsPort("udp")).toBe(true);
  });
});

describe("portError", () => {
  it("accepts a valid port", () => {
    expect(portError("tcp", "443")).toBeNull();
    expect(portError("udp", " 53 ")).toBeNull();
  });

  it("never demands a port for ping", () => {
    expect(portError("ping", "")).toBeNull();
  });

  it("rejects empty, non-numeric and out-of-range values", () => {
    expect(portError("tcp", "")).toContain("请填写");
    expect(portError("tcp", "80a")).toContain("数字");
    expect(portError("tcp", "0")).toContain("1-65535");
    expect(portError("tcp", "65536")).toContain("1-65535");
  });
});

describe("formatMs", () => {
  it("keeps sub-millisecond detail but drops it once latency is large", () => {
    expect(formatMs(0.4903)).toBe("0.49 ms");
    expect(formatMs(128.4)).toBe("128 ms");
  });

  it("renders a dash when there is no sample", () => {
    expect(formatMs(null)).toBe("—");
    expect(formatMs(undefined)).toBe("—");
  });
});

describe("formatPercent", () => {
  it("rounds to whole percent", () => {
    expect(formatPercent(0)).toBe("0%");
    expect(formatPercent(0.75)).toBe("75%");
  });
});

describe("outcomeLabel", () => {
  it("names every outcome", () => {
    expect(outcomeLabel("open")).toBe("有响应");
    expect(outcomeLabel("closed")).toBe("端口关闭");
    expect(outcomeLabel("filtered")).toBe("无响应");
    expect(outcomeLabel("unreachable")).toBe("不可达");
    expect(outcomeLabel("error")).toBe("出错");
  });
});

describe("verdictTone", () => {
  it("treats a fully reachable target as good", () => {
    expect(verdictTone("reachable")).toBe("good");
  });

  /// 「开放或被过滤」表示无法确定，配色必须是中性的警告色。
  /// 用 bad 会让人读成「不通」，而那恰恰是这个结论没有断言的事情。
  it("marks the undecidable UDP result as a warning rather than a failure", () => {
    expect(verdictTone("open_or_filtered")).toBe("warning");
    expect(verdictTone("partial")).toBe("warning");
  });

  it("marks definite failures as bad", () => {
    expect(verdictTone("closed")).toBe("bad");
    expect(verdictTone("unreachable")).toBe("bad");
  });
});

describe("verdictLabel", () => {
  it("spells out that the UDP result is undecided", () => {
    expect(verdictLabel("open_or_filtered")).toContain("无法确定");
  });
});

describe("headline", () => {
  it("reports latency when everything answered", () => {
    const text = headline(report());
    expect(text).toContain("20.205.243.166:443");
    expect(text).toContain("连通");
    expect(text).toContain("12.50 ms");
  });

  it("omits the port for ping", () => {
    const text = headline(report({ kind: "ping", port: null }));
    expect(text).toContain("20.205.243.166");
    expect(text).not.toContain(":443");
  });

  it("reports loss when only some probes answered", () => {
    const text = headline(report({ verdict: "partial", answered: 1, lossRatio: 0.75 }));
    expect(text).toContain("4 次中 1 次");
    expect(text).toContain("75%");
  });

  it("says the port is definitively closed when the peer refused", () => {
    expect(headline(report({ verdict: "closed" }))).toContain("明确拒绝");
  });

  /// 最关键的一条：静默的 UDP 绝不能被描述成「不通」。
  it("explains that a silent UDP probe proves nothing either way", () => {
    const text = headline(
      report({ kind: "udp", port: 500, verdict: "open_or_filtered", answered: 0, lossRatio: 1 }),
    );
    expect(text).toContain("无法确定");
    expect(text).toContain("端口开着但服务不回包");
    expect(text).toContain("被防火墙丢弃");
    expect(text).not.toContain("不通");
  });

  it("states plainly that a TCP target is unreachable", () => {
    expect(headline(report({ verdict: "unreachable", answered: 0 }))).toContain("不可达");
  });
});

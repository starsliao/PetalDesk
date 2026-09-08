import { describe, expect, it } from "vitest";
import {
  barWidth,
  ephemeralVerdict,
  formatRatio,
  headline,
  stateSummary,
  targetOwnerLabel,
  unownedShare,
  type EphemeralUsage,
  type NetworkReport,
  type TargetBucket,
} from "./network";

function usage(overrides: Partial<EphemeralUsage> = {}): EphemeralUsage {
  return {
    start: 49152,
    end: 65535,
    capacity: 16384,
    used: 320,
    ratio: 0.02,
    level: "normal",
    ...overrides,
  };
}

function report(overrides: Partial<NetworkReport> = {}): NetworkReport {
  return {
    sampledAt: "2026-09-08 12:00:00",
    sampleMs: 42,
    total: 1200,
    outboundTotal: 1150,
    states: [{ state: "ESTABLISHED", count: 1150, ratio: 0.958 }],
    ephemeral: usage(),
    processes: [],
    targets: [],
    loops: [],
    ...overrides,
  };
}

function target(overrides: Partial<TargetBucket> = {}): TargetBucket {
  return {
    endpoint: "127.0.0.1:10808",
    count: 11228,
    ratio: 0.479,
    loopback: true,
    ownerPid: 43096,
    ownerName: "xray",
    states: [
      { state: "TIME_WAIT", count: 10745, ratio: 0.957 },
      { state: "ESTABLISHED", count: 292, ratio: 0.026 },
      { state: "FIN_WAIT_2", count: 189, ratio: 0.017 },
      { state: "CLOSE_WAIT", count: 2, ratio: 0.0002 },
    ],
    ...overrides,
  };
}

describe("formatRatio", () => {
  it("renders one decimal place", () => {
    expect(formatRatio(0.9891)).toBe("98.9%");
    expect(formatRatio(0)).toBe("0.0%");
  });
});

describe("barWidth", () => {
  it("keeps a tiny non-zero share visible", () => {
    expect(barWidth(0.0001)).toBe("1.00%");
  });

  it("renders nothing for a zero share", () => {
    expect(barWidth(0)).toBe("0%");
  });

  it("clamps above full", () => {
    expect(barWidth(1.4)).toBe("100.00%");
  });
});

describe("ephemeralVerdict", () => {
  it("escalates with the level", () => {
    expect(ephemeralVerdict(usage({ level: "normal" }))).toBe("正常");
    expect(ephemeralVerdict(usage({ level: "warning" }))).toContain("接近耗尽");
    expect(ephemeralVerdict(usage({ level: "critical" }))).toContain("危险");
  });
});

describe("targetOwnerLabel", () => {
  it("names the listening process behind a loopback target", () => {
    expect(targetOwnerLabel(target())).toBe("xray (43096)");
  });

  it("says the listener is gone when a loopback port no longer resolves", () => {
    expect(targetOwnerLabel(target({ ownerPid: null, ownerName: null }))).toBe("本机（监听已关闭）");
  });

  it("does not invent an owner for external addresses", () => {
    expect(
      targetOwnerLabel(
        target({ endpoint: "93.184.216.34:443", loopback: false, ownerPid: null, ownerName: null }),
      ),
    ).toBe("外部地址");
  });
});

describe("stateSummary", () => {
  it("keeps the three busiest states by default", () => {
    expect(stateSummary(target().states)).toBe("TIME_WAIT 10,745  ESTABLISHED 292  FIN_WAIT_2 189");
  });

  it("honours a wider limit", () => {
    expect(stateSummary(target().states, 4)).toContain("CLOSE_WAIT 2");
  });
});

describe("unownedShare", () => {
  it("reports the PID 0 slice that process aggregation cannot attribute", () => {
    const value = unownedShare(
      report({
        processes: [
          {
            pid: 0,
            name: "(无属主 · TIME_WAIT)",
            count: 21277,
            ratio: 0.906,
            states: [],
            loopbackOut: 21277,
            externalOut: 0,
          },
        ],
      }),
    );
    expect(value).toBeCloseTo(0.906);
  });

  it("returns zero when nothing is unowned", () => {
    expect(unownedShare(report())).toBe(0);
  });
});

describe("headline", () => {
  it("leads with the proxy loop when one is found", () => {
    const text = headline(
      report({
        ephemeral: usage({ used: 16206, ratio: 0.989, level: "critical" }),
        loops: [
          {
            endpoint: "127.0.0.1:10808",
            hits: 11228,
            ownerPid: 43096,
            ownerName: "xray",
            ownerLoopbackOut: 1126,
            ownerExternalOut: 12,
            ownerLoopbackRatio: 0.9895,
            callers: [{ pid: 30976, name: "Proxifier", count: 470 }],
          },
        ],
      }),
    );
    expect(text).toContain("xray");
    expect(text).toContain("127.0.0.1:10808");
    expect(text).toContain("11,228");
    expect(text).toContain("99.0%");
  });

  it("falls back to port exhaustion when there is no loop", () => {
    const text = headline(
      report({ ephemeral: usage({ used: 16206, ratio: 0.989, level: "critical" }) }),
    );
    expect(text).toContain("16,206");
    expect(text).toContain("危险");
  });

  it("says nothing is wrong when both checks are clean", () => {
    expect(headline(report())).toContain("未见异常");
  });
});

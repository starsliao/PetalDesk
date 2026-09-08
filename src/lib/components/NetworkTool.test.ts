import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NetworkApi, NetworkReport } from "../network";
import NetworkTool from "./NetworkTool.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/// 复刻一次真实现场:两万条无主 TIME_WAIT、临时端口接近耗尽、xray 与 Proxifier 互打环路。
function loopReport(overrides: Partial<NetworkReport> = {}): NetworkReport {
  return {
    sampledAt: "2026-09-08 08:40:00",
    sampleMs: 63,
    total: 23480,
    outboundTotal: 23420,
    states: [
      { state: "TIME_WAIT", count: 21277, ratio: 0.906 },
      { state: "ESTABLISHED", count: 1186, ratio: 0.051 },
      { state: "LISTENING", count: 60, ratio: 0.003 },
    ],
    ephemeral: {
      start: 49152,
      end: 65535,
      capacity: 16384,
      used: 16206,
      ratio: 0.9891,
      level: "critical",
    },
    processes: [
      {
        pid: 0,
        name: "(无属主 · TIME_WAIT)",
        count: 21277,
        ratio: 0.906,
        states: [{ state: "TIME_WAIT", count: 21277, ratio: 1 }],
        loopbackOut: 21277,
        externalOut: 0,
      },
      {
        pid: 43096,
        name: "xray",
        count: 1140,
        ratio: 0.049,
        states: [{ state: "ESTABLISHED", count: 562, ratio: 0.493 }],
        loopbackOut: 1126,
        externalOut: 12,
      },
    ],
    targets: [
      {
        endpoint: "127.0.0.1:10808",
        count: 11228,
        ratio: 0.479,
        loopback: true,
        ownerPid: 43096,
        ownerName: "xray",
        states: [{ state: "TIME_WAIT", count: 10745, ratio: 0.957 }],
      },
      {
        endpoint: "106.55.225.200:7100",
        count: 7,
        ratio: 0.0003,
        loopback: false,
        ownerPid: null,
        ownerName: null,
        states: [{ state: "ESTABLISHED", count: 7, ratio: 1 }],
      },
    ],
    loops: [
      {
        endpoint: "127.0.0.1:10808",
        hits: 11228,
        ownerPid: 43096,
        ownerName: "xray",
        ownerLoopbackOut: 1126,
        ownerExternalOut: 12,
        ownerLoopbackRatio: 0.9895,
        callers: [
          { pid: 30976, name: "Proxifier", count: 470 },
          { pid: 18212, name: "Code", count: 5 },
        ],
      },
    ],
    ...overrides,
  };
}

function healthyReport(): NetworkReport {
  return loopReport({
    total: 180,
    outboundTotal: 150,
    states: [{ state: "ESTABLISHED", count: 150, ratio: 0.833 }],
    ephemeral: {
      start: 49152,
      end: 65535,
      capacity: 16384,
      used: 150,
      ratio: 0.0092,
      level: "normal",
    },
    processes: [
      {
        pid: 300,
        name: "firefox",
        count: 90,
        ratio: 0.5,
        states: [{ state: "ESTABLISHED", count: 90, ratio: 1 }],
        loopbackOut: 2,
        externalOut: 88,
      },
    ],
    targets: [
      {
        endpoint: "93.184.216.34:443",
        count: 40,
        ratio: 0.266,
        loopback: false,
        ownerPid: null,
        ownerName: null,
        states: [{ state: "ESTABLISHED", count: 40, ratio: 1 }],
      },
    ],
    loops: [],
  });
}

function mockApi(result: NetworkReport | Error = loopReport()): NetworkApi & {
  collect: ReturnType<typeof vi.fn>;
} {
  return {
    isDesktop: () => false,
    collect:
      result instanceof Error
        ? vi.fn().mockRejectedValue(result)
        : vi.fn().mockResolvedValue(result),
  };
}

describe("NetworkTool", () => {
  it("does not query until asked", () => {
    const api = mockApi();
    const rendered = render(NetworkTool, { api });
    expect(api.collect).not.toHaveBeenCalled();
    expect(rendered.getByText("还没有采集数据")).toBeInTheDocument();
  });

  it("collects and renders the report when the query button is clicked", async () => {
    const api = mockApi();
    const rendered = render(NetworkTool, { api });

    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalledTimes(1));

    expect(rendered.getByText(/共 23,480 条 TCP 记录/)).toBeInTheDocument();
    expect(rendered.getByText("TIME_WAIT")).toBeInTheDocument();
    expect(rendered.getByText("98.9%")).toBeInTheDocument();
  });

  /// 工具存在的理由:让「PID 0 的两万条 TIME_WAIT 其实都是打给 xray 的」这条结论
  /// 直接出现在界面上,而不是靠人去比对两张表。
  it("attributes the unowned TIME_WAIT bulk to the listening process", async () => {
    const api = mockApi();
    const rendered = render(NetworkTool, { api });
    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalled());

    // 按进程这一栏只能显示无主桶,并明确提示要去看目标聚合。
    const processes = within(rendered.getByRole("region", { name: "按进程聚合" }));
    expect(processes.getByText("(无属主 · TIME_WAIT)")).toBeInTheDocument();
    expect(processes.getByText(/按进程归因不到/)).toBeInTheDocument();

    // 目标聚合把回环端点反查成了 xray。
    const targets = within(rendered.getByRole("region", { name: "按目标端点聚合" }));
    expect(targets.getByText("127.0.0.1:10808")).toBeInTheDocument();
    expect(targets.getByText("xray (43096)")).toBeInTheDocument();
  });

  it("reports the proxy loop with the upstream callers named", async () => {
    const api = mockApi();
    const rendered = render(NetworkTool, { api });
    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalled());

    const loops = within(rendered.getByRole("region", { name: "本地代理环路研判" }));
    expect(loops.getByText(/在原地空转/)).toBeInTheDocument();
    expect(loops.getByText("Proxifier")).toBeInTheDocument();
    expect(loops.getByText("470 条")).toBeInTheDocument();
  });

  it("shows a clean verdict when nothing is wrong", async () => {
    const api = mockApi(healthyReport());
    const rendered = render(NetworkTool, { api });
    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalled());

    expect(rendered.getByText("未发现明显的本地代理环路。")).toBeInTheDocument();
    expect(rendered.getByText(/未见异常/)).toBeInTheDocument();
  });

  it("surfaces the failure reason instead of an empty panel", async () => {
    const api = mockApi(new Error("网络体检目前仅支持 Windows。"));
    const rendered = render(NetworkTool, { api });
    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));

    await waitFor(() =>
      expect(rendered.getByRole("alert")).toHaveTextContent("网络体检目前仅支持 Windows。"),
    );
    expect(rendered.getByText("还没有采集数据")).toBeInTheDocument();
  });

  it("re-queries from the titlebar without duplicating in-flight requests", async () => {
    const api = mockApi();
    const rendered = render(NetworkTool, { api });
    await fireEvent.click(rendered.getByRole("button", { name: "开始查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalledTimes(1));

    await fireEvent.click(rendered.getByRole("button", { name: "重新查询" }));
    await waitFor(() => expect(api.collect).toHaveBeenCalledTimes(2));
  });
});

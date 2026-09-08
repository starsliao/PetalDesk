import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NetCheckApi, NetCheckReport, NetCheckRequest } from "../netcheck";
import NetCheckTool from "./NetCheckTool.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function report(overrides: Partial<NetCheckReport> = {}): NetCheckReport {
  return {
    target: "github.com",
    resolvedIp: "20.205.243.166",
    kind: "tcp",
    port: 443,
    probeNote: null,
    attempts: [
      { seq: 1, outcome: "open", latencyMs: 11.2, detail: "握手成功" },
      { seq: 2, outcome: "open", latencyMs: 13.8, detail: "握手成功" },
    ],
    sent: 2,
    answered: 2,
    lossRatio: 0,
    latency: { minMs: 11.2, avgMs: 12.5, maxMs: 13.8, jitterMs: 1.3 },
    verdict: "reachable",
    startedAt: "2026-09-08 12:00:00",
    elapsedMs: 420,
    ...overrides,
  };
}

function mockApi(result: NetCheckReport | Error = report()): NetCheckApi & {
  run: ReturnType<typeof vi.fn>;
} {
  return {
    isDesktop: () => false,
    run:
      result instanceof Error
        ? vi.fn().mockRejectedValue(result)
        : vi.fn().mockResolvedValue(result),
  };
}

/// 只依赖 getByLabelText 这一个查询函数，避免为 RenderResult 写出会随
/// testing-library 版本漂移的类型标注。
type ByLabel = (text: string) => HTMLElement;

async function fill(getByLabelText: ByLabel, target: string, port?: string) {
  await fireEvent.input(getByLabelText("地址"), { target: { value: target } });
  if (port !== undefined) {
    await fireEvent.input(getByLabelText("端口"), { target: { value: port } });
  }
}

describe("NetCheckTool", () => {
  it("does not probe until asked", () => {
    const api = mockApi();
    const rendered = render(NetCheckTool, { api });
    expect(api.run).not.toHaveBeenCalled();
    expect(rendered.getByText("输入地址后开始检测")).toBeInTheDocument();
  });

  it("hides the port field for ping and shows it for the port probes", async () => {
    const api = mockApi();
    const rendered = render(NetCheckTool, { api });

    expect(rendered.queryByLabelText("端口")).not.toBeInTheDocument();
    await fireEvent.change(rendered.getByLabelText("方式"), { target: { value: "tcp" } });
    expect(rendered.getByLabelText("端口")).toBeInTheDocument();
  });

  it("sends the typed target, kind and port to the backend", async () => {
    const api = mockApi();
    const rendered = render(NetCheckTool, { api });
    await fireEvent.change(rendered.getByLabelText("方式"), { target: { value: "tcp" } });
    await fill(rendered.getByLabelText, "github.com", "443");

    await fireEvent.click(rendered.getByRole("button", { name: "开始检测" }));
    await waitFor(() => expect(api.run).toHaveBeenCalledTimes(1));

    const request = api.run.mock.calls[0][0] as NetCheckRequest;
    expect(request.target).toBe("github.com");
    expect(request.kind).toBe("tcp");
    expect(request.port).toBe(443);
  });

  it("renders the verdict, latency stats and every attempt", async () => {
    const api = mockApi();
    const rendered = render(NetCheckTool, { api });
    await fireEvent.change(rendered.getByLabelText("方式"), { target: { value: "tcp" } });
    await fill(rendered.getByLabelText, "github.com", "443");
    await fireEvent.click(rendered.getByRole("button", { name: "开始检测" }));
    await waitFor(() => expect(api.run).toHaveBeenCalled());

    const verdict = within(rendered.getByRole("region", { name: "检测结论" }));
    expect(verdict.getByText("连通")).toBeInTheDocument();

    const stats = within(rendered.getByRole("region", { name: "延时统计" }));
    expect(stats.getByText("12.50 ms")).toBeInTheDocument();
    expect(stats.getByText("0%")).toBeInTheDocument();

    const attempts = within(rendered.getByRole("region", { name: "逐次结果" }));
    expect(attempts.getAllByText("有响应")).toHaveLength(2);
  });

  /// 工具的核心承诺：静默的 UDP 结果必须呈现为「无法确定」，而不是失败。
  it("presents a silent UDP probe as undecided rather than as a failure", async () => {
    const api = mockApi(
      report({
        kind: "udp",
        port: 500,
        verdict: "open_or_filtered",
        answered: 0,
        lossRatio: 1,
        probeNote: "该端口没有通用的探测协议，只能发空载荷",
        latency: null,
        attempts: [{ seq: 1, outcome: "filtered", latencyMs: null, detail: "超时无应答" }],
      }),
    );
    const rendered = render(NetCheckTool, { api });
    await fireEvent.change(rendered.getByLabelText("方式"), { target: { value: "udp" } });
    await fill(rendered.getByLabelText, "10.0.0.1", "500");
    await fireEvent.click(rendered.getByRole("button", { name: "开始检测" }));
    await waitFor(() => expect(api.run).toHaveBeenCalled());

    const verdict = rendered.getByRole("region", { name: "检测结论" });
    expect(within(verdict).getByText("开放或被过滤（无法确定）")).toBeInTheDocument();
    expect(within(verdict).getByText(/端口开着但服务不回包/)).toBeInTheDocument();
    // 中性警告色，不是失败色 —— 这个结论并没有断言「不通」。
    expect(verdict).toHaveClass("warning");
    expect(rendered.getByText(/只能发空载荷/)).toBeInTheDocument();
  });

  it("refuses to probe a port probe without a port", async () => {
    const api = mockApi();
    const rendered = render(NetCheckTool, { api });
    await fireEvent.change(rendered.getByLabelText("方式"), { target: { value: "tcp" } });
    await fill(rendered.getByLabelText, "github.com");

    expect(rendered.getByRole("button", { name: "开始检测" })).toBeDisabled();
    expect(api.run).not.toHaveBeenCalled();
  });

  it("surfaces the failure reason instead of a blank result", async () => {
    const api = mockApi(new Error("无法解析「nope.invalid」"));
    const rendered = render(NetCheckTool, { api });
    await fill(rendered.getByLabelText, "nope.invalid");
    await fireEvent.click(rendered.getByRole("button", { name: "开始检测" }));

    await waitFor(() =>
      expect(rendered.getByRole("alert")).toHaveTextContent("无法解析「nope.invalid」"),
    );
  });
});

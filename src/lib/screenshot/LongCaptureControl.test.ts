import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import LongCaptureControl from "./LongCaptureControl.svelte";
import type { LongCaptureStatus, ScreenshotApi } from "./types";

function status(state: LongCaptureStatus["state"], patch: Partial<LongCaptureStatus> = {}): LongCaptureStatus {
  return {
    jobId: "long-1",
    sessionId: "session-1",
    state,
    engine: "wheel",
    frameCount: 3,
    width: 500,
    height: 2400,
    canUndo: true,
    ...patch,
  };
}

function controlApi() {
  return {
    getLongCaptureStatus: vi.fn().mockResolvedValue(status("paused")),
    pauseLongCapture: vi.fn().mockResolvedValue(status("paused")),
    resumeLongCapture: vi.fn().mockResolvedValue(status("capturing")),
    retryLongCapture: vi.fn().mockResolvedValue(status("capturing")),
    undoLongCapture: vi.fn().mockResolvedValue(status("paused", { frameCount: 2 })),
    finishLongCapture: vi.fn().mockResolvedValue(status("ready")),
    cancelLongCapture: vi.fn().mockResolvedValue(status("canceled")),
    cancelLongCaptureSession: vi.fn().mockResolvedValue(status("canceled")),
  } as unknown as ScreenshotApi & Record<string, ReturnType<typeof vi.fn>>;
}

afterEach(cleanup);

describe("LongCaptureControl", () => {
  it("drives a capture job without loading the full screenshot editor", async () => {
    const api = controlApi();
    const onstatus = vi.fn();
    const onready = vi.fn();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("paused"),
      monitor: false,
      onstatus,
      onready,
    });

    expect(rendered.getByText("3 帧 · 2,400 px")).toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "重试当前段" })).toBeEnabled();
    expect(rendered.getByRole("button", { name: "回退上一段" })).toBeEnabled();
    expect(rendered.getByRole("button", { name: "完成长截图" })).toBeEnabled();
    await fireEvent.click(rendered.getByRole("button", { name: "继续长截图" }));
    await waitFor(() => expect(api.resumeLongCapture).toHaveBeenCalledWith("long-1"));
    expect(rendered.getByRole("button", { name: "暂停长截图" })).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledWith("long-1"));
    await fireEvent.click(rendered.getByRole("button", { name: "回退上一段" }));
    await waitFor(() => expect(api.undoLongCapture).toHaveBeenCalledWith("long-1"));
    expect(rendered.getByText("2 帧 · 2,400 px")).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "完成长截图" }));
    await waitFor(() => expect(onready).toHaveBeenCalledWith(expect.objectContaining({ state: "ready" })));
    expect(onstatus).toHaveBeenLastCalledWith(expect.objectContaining({ state: "ready" }));
    expect(api.getLongCaptureStatus).not.toHaveBeenCalled();
  });

  it("shows capture and manual-scroll status instead of an unexplained toolbar", () => {
    const api = controlApi();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing", { message: "等待手动滚动" }),
      monitor: false,
    });

    expect(rendered.getByText("等待手动滚动")).toBeInTheDocument();
  });

  it("uses Escape to cancel the independent control window job", async () => {
    const api = controlApi();
    const oncancel = vi.fn();
    render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing"),
      monitor: false,
      oncancel,
    });

    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    expect(oncancel).toHaveBeenCalledOnce();
  });

  it("delegates global shortcuts when embedded in another editor", async () => {
    const api = controlApi();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("paused"),
      monitor: false,
      keyboardShortcuts: false,
    });

    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent.keyDown(window, { key: " ", code: "Space" });
    await fireEvent.keyDown(window, { key: "Enter" });
    expect(api.cancelLongCapture).not.toHaveBeenCalled();
    expect(api.resumeLongCapture).not.toHaveBeenCalled();
    expect(api.finishLongCapture).not.toHaveBeenCalled();
    expect(rendered.getByRole("button", { name: "继续长截图" })).toHaveAttribute("title", "继续");
    expect(rendered.getByRole("button", { name: "完成长截图" })).toHaveAttribute("title", "完成");

    await fireEvent.click(rendered.getByRole("button", { name: "继续长截图" }));
    await waitFor(() => expect(api.resumeLongCapture).toHaveBeenCalledOnce());
  });

  it("lets cancellation supersede a control request that has not returned", async () => {
    const api = controlApi();
    let resolvePause: ((next: LongCaptureStatus) => void) | undefined;
    vi.mocked(api.pauseLongCapture).mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolvePause = resolve;
    }));
    const onstatus = vi.fn();
    const oncancel = vi.fn();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing"),
      monitor: false,
      onstatus,
      oncancel,
    });

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledWith("long-1"));
    expect(rendered.getByRole("button", { name: "取消长截图" })).toBeEnabled();

    await fireEvent.click(rendered.getByRole("button", { name: "取消长截图" }));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    await waitFor(() => expect(oncancel).toHaveBeenCalledOnce());
    expect(onstatus).toHaveBeenLastCalledWith(expect.objectContaining({ state: "canceled" }));

    resolvePause?.(status("paused"));
    await Promise.resolve();
    await Promise.resolve();
    expect(onstatus).toHaveBeenLastCalledWith(expect.objectContaining({ state: "canceled" }));
  });

  it("recovers controls after a native command times out", async () => {
    const api = controlApi();
    vi.mocked(api.pauseLongCapture).mockImplementation(() => new Promise<LongCaptureStatus>(() => undefined));
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing"),
      monitor: false,
      controlTimeoutMs: 250,
    });

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(rendered.getByText("控制长截图超时。")).toBeInTheDocument());
    expect(rendered.getByRole("button", { name: "暂停长截图" })).toBeEnabled();
    expect(rendered.getByRole("button", { name: "取消长截图" })).toBeEnabled();

    await fireEvent.click(rendered.getByRole("button", { name: "取消长截图" }));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
  });

  it("re-enables cancellation when the native cancel command times out", async () => {
    const api = controlApi();
    vi.mocked(api.cancelLongCapture).mockImplementation(() => new Promise<LongCaptureStatus>(() => undefined));
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing"),
      monitor: false,
      controlTimeoutMs: 250,
    });

    const cancelButton = rendered.getByRole("button", { name: "取消长截图" });
    await fireEvent.click(cancelButton);
    expect(cancelButton).toBeDisabled();
    await waitFor(() => expect(rendered.getByText("取消长截图超时。")).toBeInTheDocument());
    expect(cancelButton).toBeEnabled();

    await fireEvent.click(cancelButton);
    expect(api.cancelLongCapture).toHaveBeenCalledTimes(2);
  });

  it("ignores a stale poll failure after a newer control action", async () => {
    const api = controlApi();
    let rejectPoll: ((reason?: unknown) => void) | undefined;
    vi.mocked(api.getLongCaptureStatus).mockImplementation(() => new Promise<LongCaptureStatus | null>((_resolve, reject) => {
      rejectPoll = reject;
    }));
    const onerror = vi.fn();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("capturing"),
      onerror,
    });

    await waitFor(() => expect(api.getLongCaptureStatus).toHaveBeenCalledWith("long-1"), { timeout: 2_000 });
    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledWith("long-1"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "继续长截图" })).toBeInTheDocument());

    rejectPoll?.(new Error("旧轮询失败"));
    await Promise.resolve();
    await Promise.resolve();
    expect(onerror).not.toHaveBeenCalled();
    expect(rendered.queryByText("旧轮询失败")).not.toBeInTheDocument();
  });

  it("offers only cancellation after an independent capture job fails", async () => {
    const api = controlApi();
    const rendered = render(LongCaptureControl, {
      jobId: "long-1",
      api,
      initialStatus: status("failed", { message: "浏览器页面已关闭" }),
      monitor: false,
    });

    expect(rendered.getByText("长截图失败")).toBeInTheDocument();
    expect(rendered.getByText("浏览器页面已关闭")).toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "重试当前段" })).not.toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "回退上一段" })).not.toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "完成长截图" })).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Enter" });
    expect(api.finishLongCapture).not.toHaveBeenCalled();
    await fireEvent.click(rendered.getByRole("button", { name: "取消长截图" }));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    expect(api.retryLongCapture).not.toHaveBeenCalled();
    expect(api.undoLongCapture).not.toHaveBeenCalled();
  });
});

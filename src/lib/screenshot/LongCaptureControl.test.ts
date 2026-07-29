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

    await fireEvent.click(rendered.getByRole("button", { name: "继续长截图" }));
    await waitFor(() => expect(api.resumeLongCapture).toHaveBeenCalledOnce());
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

import { beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));

import { pinnedScreenshotApi, screenshotApi } from "./api";
import { DEFAULT_TOOL_SETTINGS } from "./types";

beforeEach(() => backend.invoke.mockReset());

describe("screenshot desktop API", () => {
  it("normalizes incomplete persisted tool settings", async () => {
    backend.invoke.mockResolvedValueOnce({
      schemaVersion: 1,
      shortcut: "F1",
      colorFormat: "hex",
      toolParameters: { strokeWidth: 9 },
    });
    await expect(screenshotApi.getSettings()).resolves.toMatchObject({
      shortcut: "F1",
      colorFormat: "hex",
      toolParameters: { ...DEFAULT_TOOL_SETTINGS, strokeWidth: 9 },
    });
  });

  it("returns an idle session instead of treating the preheated window as failed", async () => {
    backend.invoke.mockResolvedValueOnce(null);
    await expect(screenshotApi.getSession()).resolves.toBeNull();
  });

  it("presents a prepared capture session through its dedicated command", async () => {
    backend.invoke.mockResolvedValueOnce(undefined);
    await screenshotApi.present("session-1");
    expect(backend.invoke).toHaveBeenCalledWith("present_screenshot_capture", {
      sessionId: "session-1",
    });
  });

  it("submits exported PNG as a raw body with the one-time ticket header", async () => {
    backend.invoke
      .mockResolvedValueOnce({ canceled: false, ticket: "ticket-42" })
      .mockResolvedValueOnce({ action: "copy" });
    const png = Uint8Array.from([137, 80, 78, 71]);
    await expect(screenshotApi.exportPng({ sessionId: "session-1", action: "copy" }, png)).resolves.toEqual({ action: "copy" });
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "prepare_screenshot_export", {
      request: { sessionId: "session-1", action: "copy" },
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "commit_screenshot_export", png, {
      headers: { "x-petaldesk-export-token": "ticket-42" },
    });
  });

  it("does not submit PNG when the Save As dialog is canceled", async () => {
    backend.invoke.mockResolvedValueOnce({ canceled: true });
    await expect(screenshotApi.exportPng(
      { sessionId: "session-1", action: "save" },
      Uint8Array.from([1]),
    )).resolves.toMatchObject({ action: "save", canceled: true });
    expect(backend.invoke).toHaveBeenCalledTimes(1);
  });

  it("uses the long-capture request envelope and dedicated control commands", async () => {
    const status = {
      jobId: "long-1",
      sessionId: "session-1",
      state: "capturing",
      engine: "browserEnhanced",
      frameCount: 2,
      width: 600,
      height: 900,
      canUndo: true,
    };
    backend.invoke
      .mockResolvedValueOnce({ available: true, engines: ["browserEnhanced", "wheel"] })
      .mockResolvedValueOnce(status)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ ...status, state: "paused" });

    await expect(screenshotApi.getLongCaptureCapability()).resolves.toMatchObject({ available: true });
    await expect(screenshotApi.startLongCapture({
      sessionId: "session-1",
      selection: { x: 10, y: 20, width: 600, height: 400 },
      scrollAnchor: { x: 300, y: 220 },
      scope: "selection",
      mode: "current",
    })).resolves.toEqual(status);
    await expect(screenshotApi.pauseLongCapture("long-1")).resolves.toMatchObject({ state: "paused" });

    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_long_capture_capability", undefined);
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "start_long_capture", {
      request: {
        sessionId: "session-1",
        selection: { x: 10, y: 20, width: 600, height: 400 },
        scrollAnchor: { x: 300, y: 220 },
        scope: "selection",
        mode: "current",
      },
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(3, "pause_long_capture", { jobId: "long-1" });
    expect(backend.invoke).toHaveBeenNthCalledWith(4, "get_long_capture_status", { jobId: "long-1" });
  });

  it("normalizes legacy native capability and reviewing status values", async () => {
    backend.invoke
      .mockResolvedValueOnce({ supported: true, platform: "windows", engines: ["wheel"] })
      .mockResolvedValueOnce({
        jobId: "long-1",
        sessionId: "session-1",
        state: "reviewing",
        engine: "wheel",
        frameCount: 4,
        width: 500,
        height: 3200,
        message: "已完成",
        canUndo: true,
      });

    await expect(screenshotApi.getLongCaptureCapability()).resolves.toMatchObject({ available: true, supported: true });
    await expect(screenshotApi.getLongCaptureStatus("long-1")).resolves.toMatchObject({ state: "ready" });
  });

  it("uses native segment command names for retry and undo", async () => {
    const status = {
      jobId: "long-1", sessionId: "session-1", state: "paused", engine: "wheel",
      frameCount: 2, width: 500, height: 1200, message: "已暂停", canUndo: true,
    };
    backend.invoke.mockResolvedValueOnce(status).mockResolvedValueOnce(status);
    await screenshotApi.retryLongCapture("long-1");
    await screenshotApi.undoLongCapture("long-1");
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "retry_long_capture_segment", { jobId: "long-1" });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "undo_long_capture_segment", { jobId: "long-1" });
  });

  it("cancels a pending long capture by screenshot session", async () => {
    backend.invoke.mockResolvedValueOnce(null);

    await expect(screenshotApi.cancelLongCaptureSession("session-1")).resolves.toBeNull();
    expect(backend.invoke).toHaveBeenCalledWith("cancel_long_capture_session", {
      sessionId: "session-1",
    });
  });

  it("reads long-capture tiles as raw PNG data and delegates export", async () => {
    backend.invoke
      .mockResolvedValueOnce([137, 80, 78, 71])
      .mockResolvedValueOnce({ action: "save", savedPath: "C:\\long.png" });

    await expect(screenshotApi.getLongCaptureTile("long-1", 2048, 1024)).resolves.toEqual(
      Uint8Array.from([137, 80, 78, 71]),
    );
    await expect(screenshotApi.exportLongCapture("long-1", "save")).resolves.toMatchObject({ action: "save" });
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_long_capture_tile", {
      jobId: "long-1",
      y: 2048,
      height: 1024,
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "export_long_capture", { jobId: "long-1", action: "save" });
  });

  it("uses ticketed raw IPC for bounded annotated long-image strips", async () => {
    backend.invoke
      .mockResolvedValueOnce({ canceled: false, ticket: "long-ticket", stripHeight: 1024 })
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce({ action: "copy" })
      .mockResolvedValueOnce(undefined);
    const png = Uint8Array.from([137, 80, 78, 71]);

    await expect(screenshotApi.prepareLongCaptureAnnotationExport("long-1", "copy")).resolves.toEqual({
      canceled: false,
      ticket: "long-ticket",
      stripHeight: 1024,
    });
    await screenshotApi.uploadLongCaptureAnnotationStrip("long-ticket", 2048, png);
    await expect(screenshotApi.finishLongCaptureAnnotationExport("long-ticket")).resolves.toEqual({ action: "copy" });
    await screenshotApi.cancelLongCaptureAnnotationExport("long-ticket");

    expect(backend.invoke).toHaveBeenNthCalledWith(1, "prepare_long_capture_annotation_export", {
      jobId: "long-1",
      action: "copy",
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "upload_long_capture_annotation_strip", png, {
      headers: {
        "x-petaldesk-long-export-token": "long-ticket",
        "x-petaldesk-long-export-y": "2048",
      },
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(3, "finish_long_capture_annotation_export", { ticket: "long-ticket" });
    expect(backend.invoke).toHaveBeenNthCalledWith(4, "cancel_long_capture_annotation_export", { ticket: "long-ticket" });
  });

  it("uses dedicated pinned-image commands", async () => {
    backend.invoke
      .mockResolvedValueOnce(Uint8Array.from([1, 2, 3]))
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce({ canceled: false, savedPath: "C:\\shot.png" })
      .mockResolvedValueOnce(true);
    await expect(pinnedScreenshotApi.getPng("pin-1")).resolves.toEqual(Uint8Array.from([1, 2, 3]));
    await pinnedScreenshotApi.copy("pin-1");
    await pinnedScreenshotApi.save("pin-1");
    await pinnedScreenshotApi.close("pin-1");
    expect(backend.invoke.mock.calls.map(([command]) => command)).toEqual([
      "get_pinned_screenshot",
      "copy_pinned_screenshot",
      "save_pinned_screenshot",
      "close_pinned_screenshot",
    ]);
  });
});

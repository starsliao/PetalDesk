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

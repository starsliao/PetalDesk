import { beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));

import { normalizeUpdateState, updaterApi } from "./api";

beforeEach(() => backend.invoke.mockReset());

describe("updater desktop API", () => {
  it("normalizes backend state and camel-case phases", () => {
    expect(normalizeUpdateState({
      phase: "up_to_date",
      current_version: "0.5.2",
      downloaded_bytes: 0,
    })).toMatchObject({
      phase: "upToDate",
      currentVersion: "0.5.2",
      downloadedBytes: 0,
      totalBytes: null,
    });
  });

  it("uses explicit commands for settings and manual checks", async () => {
    backend.invoke
      .mockResolvedValueOnce({ autoUpdate: true })
      .mockResolvedValueOnce({ autoUpdate: false })
      .mockResolvedValueOnce({ phase: "upToDate", currentVersion: "0.5.2" });

    await expect(updaterApi.getSettings()).resolves.toEqual({ autoUpdate: true });
    await expect(updaterApi.setSettings({ autoUpdate: false })).resolves.toEqual({ autoUpdate: false });
    await expect(updaterApi.check()).resolves.toMatchObject({ phase: "upToDate" });

    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_update_settings", undefined);
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "set_update_settings", {
      settings: { autoUpdate: false },
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(3, "check_for_updates", undefined);
  });

  it("includes the calling window identity in installation acknowledgements", async () => {
    backend.invoke.mockResolvedValueOnce(undefined);

    await updaterApi.acknowledgeInstall("request-1", "note-42", false, "保存失败");

    expect(backend.invoke).toHaveBeenCalledWith("acknowledge_update_install", {
      requestId: "request-1",
      windowLabel: "note-42",
      ok: false,
      error: "保存失败",
    });
  });

  it("registers and unregisters a window around installation preparation", async () => {
    backend.invoke.mockResolvedValue(undefined);

    await updaterApi.registerInstallWindow();
    await updaterApi.unregisterInstallWindow();

    expect(backend.invoke).toHaveBeenNthCalledWith(1, "register_update_install_window", undefined);
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "unregister_update_install_window", undefined);
  });
});

import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UpdateApi, UpdateState } from "$lib/updater";
import AboutDialog from "./AboutDialog.svelte";

afterEach(cleanup);

const idleState: UpdateState = {
  phase: "idle",
  currentVersion: "0.8.0",
  availableVersion: null,
  releaseNotes: null,
  publishedAt: null,
  downloadedBytes: 0,
  totalBytes: null,
  error: null,
};

function mockApi(overrides: Partial<UpdateApi> = {}): UpdateApi {
  return {
    isSupported: () => true,
    getSettings: vi.fn().mockResolvedValue({ autoUpdate: true }),
    setSettings: vi.fn(async (settings) => settings),
    getState: vi.fn().mockResolvedValue(idleState),
    check: vi.fn().mockResolvedValue({ ...idleState, phase: "upToDate" }),
    download: vi.fn().mockResolvedValue({ ...idleState, phase: "ready" }),
    installAndRestart: vi.fn().mockResolvedValue(undefined),
    postpone: vi.fn().mockResolvedValue(idleState),
    registerInstallWindow: vi.fn().mockResolvedValue(undefined),
    unregisterInstallWindow: vi.fn().mockResolvedValue(undefined),
    acknowledgeInstall: vi.fn().mockResolvedValue(undefined),
    listen: vi.fn().mockResolvedValue(() => undefined),
    ...overrides,
  };
}

describe("AboutDialog", () => {
  it("shows the package timestamp next to the current version", () => {
    const buildTimestamp = 1_704_067_200;
    const rendered = render(AboutDialog, {
      currentVersion: "0.8.0",
      buildTimestamp,
      supported: false,
    });

    const packagedAt = rendered.getByText(/^打包时间 /);
    expect(packagedAt).toHaveAttribute("datetime", new Date(buildTimestamp * 1000).toISOString());
    expect(rendered.getByText("v0.8.0")).toBeInTheDocument();
    expect(rendered.queryByText("Windows")).not.toBeInTheDocument();
  });

  it("enables automatic updates by default and supports a manual check", async () => {
    const api = mockApi();
    const rendered = render(AboutDialog, { currentVersion: "0.8.0", supported: true, api });

    const automatic = await rendered.findByRole("checkbox", { name: /自动检查并下载更新/ });
    expect(automatic).toBeChecked();

    const check = rendered.getByRole("button", { name: "检查更新" });
    await waitFor(() => expect(check).toBeEnabled());
    await fireEvent.click(check);
    await waitFor(() => expect(api.check).toHaveBeenCalledOnce());
    expect(await rendered.findByText("已经是最新版本")).toBeInTheDocument();
  });

  it("persists a disabled automatic-update preference", async () => {
    const api = mockApi();
    const rendered = render(AboutDialog, { currentVersion: "0.8.0", supported: true, api });
    const automatic = await rendered.findByRole("checkbox", { name: /自动检查并下载更新/ });

    await waitFor(() => expect(automatic).toBeEnabled());
    await fireEvent.click(automatic);

    await waitFor(() => {
      expect(api.setSettings).toHaveBeenCalledWith({ autoUpdate: false });
    });
  });

  it("shows a failed manual check as an error instead of leaving a loading state", async () => {
    const api = mockApi({ check: vi.fn().mockRejectedValue(new Error("网络不可用")) });
    const rendered = render(AboutDialog, { currentVersion: "0.8.0", supported: true, api });
    const check = rendered.getByRole("button", { name: "检查更新" });

    await waitFor(() => expect(check).toBeEnabled());
    await fireEvent.click(check);

    expect(await rendered.findByText("暂时无法完成更新")).toBeInTheDocument();
    expect(rendered.queryByText("正在检查新版本")).not.toBeInTheDocument();
  });
});

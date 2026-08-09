import { afterEach, describe, expect, it, vi } from "vitest";

let activeUnmount: (() => void) | null = null;

afterEach(() => {
  activeUnmount?.();
  activeUnmount = null;
  vi.restoreAllMocks();
  window.history.replaceState({}, "", "/");
});

describe("launch-at-login settings", () => {
  it("restores the previous checkbox when Windows rejects the change", async () => {
    vi.resetModules();
    const [{ fireEvent, render, waitFor }, { notesApi }, { screenshotApi }, { default: Page }] =
      await Promise.all([
        import("@testing-library/svelte"),
        import("$lib/bridge"),
        import("$lib/screenshot"),
        import("./+page.svelte"),
      ]);
    const appInfo = vi.spyOn(notesApi, "appInfo").mockResolvedValue({
      workspacePath: "E:/notes",
      version: "0.8.3",
      defaultEditorMode: "typora",
      trayShortcutSettings: {
        doubleClick: "firstNote",
        altDoubleClick: "gantt",
        ctrlDoubleClick: "mfa",
        shiftDoubleClick: "mainWindow",
      },
      protectSensitiveWindows: false,
    });
    vi.spyOn(notesApi, "migrateLegacyTimerData").mockResolvedValue(false);
    vi.spyOn(notesApi, "listNotes").mockResolvedValue([]);
    vi.spyOn(notesApi, "listTrash").mockResolvedValue([]);
    vi.spyOn(notesApi, "getLaunchAtLoginStatus").mockResolvedValue({
      supported: true,
      enabled: false,
    });
    const setLaunchAtLogin = vi
      .spyOn(notesApi, "setLaunchAtLoginEnabled")
      .mockRejectedValue({ code: "launch_at_login_error", message: "注册表拒绝访问" });
    vi.spyOn(screenshotApi, "getSettings").mockResolvedValue({
      schemaVersion: 1,
      shortcut: "F1",
      lastSaveDirectory: null,
      colorFormat: "hex",
      toolParameters: {
        shape: "rectangle",
        line: "arrow",
        lineStyle: "solid",
        strokeColor: "#ff3b30",
        fillColor: null,
        strokeWidth: 4,
        pencilWidth: 4,
        markerWidth: 20,
        markerTip: "round",
        effect: "mosaic",
        effectMode: "brush",
        effectSize: 28,
        effectIntensity: 12,
        eraserMode: "brush",
        eraserSize: 28,
        textColor: "#ff3b30",
        fontFamily: "Microsoft YaHei UI",
        fontSize: 22,
        textBold: false,
        textItalic: false,
        textUnderline: false,
      },
    });

    const rendered = render(Page);
    activeUnmount = () => rendered.unmount();
    await waitFor(() => expect(rendered.getByRole("button", { name: "打开设置" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "打开设置" }));
    const checkbox = await rendered.findByRole("checkbox", { name: "开机启动" });
    expect(checkbox).not.toBeChecked();

    await fireEvent.click(checkbox);
    expect(setLaunchAtLogin).toHaveBeenCalledWith(true);
    await waitFor(() => expect(checkbox).not.toBeChecked());
    expect(rendered.getByRole("alert")).toHaveTextContent("注册表拒绝访问");
    expect(appInfo).toHaveBeenCalled();
  }, 60_000);
});

import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_TRAY_SHORTCUT_SETTINGS } from "$lib/bridge";
import ScreenshotSettingsDialog from "./ScreenshotSettingsDialog.svelte";

afterEach(cleanup);

describe("ScreenshotSettingsDialog", () => {
  it("records a modified shortcut and saves it", async () => {
    const onsave = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, { open: true, shortcut: "F1", onsave });

    await fireEvent.click(rendered.getByRole("button", { name: "录入截图快捷键" }));
    await fireEvent.keyDown(window, { key: "s", ctrlKey: true, shiftKey: true });
    expect(rendered.getByText("Ctrl+Shift+S")).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "保存" }));
    expect(onsave).toHaveBeenCalledWith(
      "Ctrl+Shift+S",
      DEFAULT_TRAY_SHORTCUT_SETTINGS,
    );
  });

  it("ignores an unmodified letter and restores F1", async () => {
    const rendered = render(ScreenshotSettingsDialog, { open: true, shortcut: "Alt+S" });
    const recorder = rendered.getByRole("button", { name: "录入截图快捷键" });
    await fireEvent.click(recorder);
    await fireEvent.keyDown(window, { key: "q" });
    expect(recorder).toHaveTextContent("请按快捷键");

    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent.click(rendered.getByRole("button", { name: "恢复默认 F1" }));
    expect(rendered.getByText("F1")).toBeInTheDocument();
  });

  it("groups general, data storage, and screenshot settings", () => {
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      dataStoragePath: "C:\\Users\\tester\\Documents\\PetalDesk",
    });

    expect(rendered.getByRole("heading", { name: "常规" })).toBeInTheDocument();
    expect(rendered.getByRole("heading", { name: "飞花 - PetalDesk 数据存储" })).toBeInTheDocument();
    expect(rendered.getByRole("heading", { name: "托盘双击动作" })).toBeInTheDocument();
    expect(rendered.getByRole("heading", { name: "截图" })).toBeInTheDocument();
    expect(rendered.getByRole("heading", { name: "关于" })).toBeInTheDocument();
    expect(rendered.getByText("C:\\Users\\tester\\Documents\\PetalDesk")).toBeInTheDocument();
  });

  it("opens the independent about and update dialog without saving other settings", async () => {
    const onsave = vi.fn();
    const onaboutopen = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      onsave,
      onaboutopen,
    });

    await fireEvent.click(rendered.getByRole("button", { name: "打开关于与更新" }));

    expect(onaboutopen).toHaveBeenCalledOnce();
    expect(onsave).not.toHaveBeenCalled();
  });

  it("toggles sensitive window protection through the privacy section", async () => {
    const onprotectsensitivechange = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      protectSensitiveWindows: false,
      onprotectsensitivechange,
    });

    expect(rendered.getByRole("heading", { name: "隐私与安全" })).toBeInTheDocument();
    const group = rendered.getByRole("group", { name: "保护敏感窗口" });
    const off = Array.from(group.querySelectorAll("button")).find(
      (button) => button.textContent === "关闭",
    );
    const on = Array.from(group.querySelectorAll("button")).find(
      (button) => button.textContent === "开启",
    );
    expect(off?.classList.contains("active")).toBe(true);
    expect(on?.classList.contains("active")).toBe(false);

    await fireEvent.click(on!);
    expect(onprotectsensitivechange).toHaveBeenCalledWith(true);

    rendered.rerender({ open: true, protectSensitiveWindows: true, onprotectsensitivechange });
    await fireEvent.click(rendered.getByRole("group", { name: "保护敏感窗口" }).children[0]);
    expect(onprotectsensitivechange).toHaveBeenCalledWith(false);
  });

  it("configures every tray double-click modifier and saves it with the screenshot shortcut", async () => {
    const onsave = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      shortcut: "F1",
      trayShortcutSettings: { ...DEFAULT_TRAY_SHORTCUT_SETTINGS },
      onsave,
    });

    const doubleClick = rendered.getByRole("combobox", { name: "双击打开" });
    const altDoubleClick = rendered.getByRole("combobox", { name: "Alt 加双击打开" });
    const ctrlDoubleClick = rendered.getByRole("combobox", { name: "Ctrl 加双击打开" });
    const shiftDoubleClick = rendered.getByRole("combobox", { name: "Shift 加双击打开" });

    expect(doubleClick).toHaveValue("firstNote");
    expect(altDoubleClick).toHaveValue("gantt");
    expect(ctrlDoubleClick).toHaveValue("mfa");
    expect(shiftDoubleClick).toHaveValue("mainWindow");
    expect(Array.from(doubleClick.querySelectorAll("option"), (option) => option.value)).toEqual([
      "firstNote",
      "recentNote",
      "mainWindow",
      "timer",
      "reminder",
      "gantt",
      "mfa",
      "screenshot",
    ]);

    await fireEvent.change(doubleClick, { target: { value: "timer" } });
    await fireEvent.change(altDoubleClick, { target: { value: "reminder" } });
    await fireEvent.change(ctrlDoubleClick, { target: { value: "screenshot" } });
    await fireEvent.change(shiftDoubleClick, { target: { value: "firstNote" } });
    await fireEvent.click(rendered.getByRole("button", { name: "保存" }));

    expect(onsave).toHaveBeenCalledWith("F1", {
      doubleClick: "timer",
      altDoubleClick: "reminder",
      ctrlDoubleClick: "screenshot",
      shiftDoubleClick: "firstNote",
    });
  });

  it("restores the default tray double-click actions", async () => {
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      trayShortcutSettings: {
        doubleClick: "screenshot",
        altDoubleClick: "timer",
        ctrlDoubleClick: "reminder",
        shiftDoubleClick: "gantt",
      },
    });

    await fireEvent.click(rendered.getByRole("button", { name: "恢复默认动作" }));
    expect(rendered.getByRole("combobox", { name: "双击打开" })).toHaveValue("firstNote");
    expect(rendered.getByRole("combobox", { name: "Alt 加双击打开" })).toHaveValue("gantt");
    expect(rendered.getByRole("combobox", { name: "Ctrl 加双击打开" })).toHaveValue("mfa");
    expect(rendered.getByRole("combobox", { name: "Shift 加双击打开" })).toHaveValue("mainWindow");
  });

  it("changes the default editor style from the segmented control", async () => {
    const oneditormodechange = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      editorMode: "typora",
      oneditormodechange,
    });

    expect(rendered.getByRole("button", { name: "Markdown" })).toHaveAttribute("aria-pressed", "true");
    await fireEvent.click(rendered.getByRole("button", { name: "纯文本" }));
    expect(oneditormodechange).toHaveBeenCalledWith("plain");
  });

  it("requests a data storage path change", async () => {
    const ondatastoragechange = vi.fn();
    const rendered = render(ScreenshotSettingsDialog, {
      open: true,
      dataStorageLabel: "默认飞花 - PetalDesk 数据存储",
      ondatastoragechange,
    });

    expect(rendered.getByText("默认飞花 - PetalDesk 数据存储")).toBeInTheDocument();
    await fireEvent.click(rendered.getByRole("button", { name: "更改" }));
    expect(ondatastoragechange).toHaveBeenCalledOnce();
  });
});

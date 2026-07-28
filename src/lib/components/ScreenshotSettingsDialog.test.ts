import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
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
    expect(onsave).toHaveBeenCalledWith("Ctrl+Shift+S");
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
    expect(rendered.getByRole("heading", { name: "截图" })).toBeInTheDocument();
    expect(rendered.getByText("C:\\Users\\tester\\Documents\\PetalDesk")).toBeInTheDocument();
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

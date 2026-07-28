import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import NoteTitlebar from "./NoteTitlebar.svelte";

afterEach(cleanup);

describe("NoteTitlebar title editing", () => {
  it("commits an edited title with Enter", async () => {
    const ontitlechange = vi.fn();
    const { getByLabelText, getByRole, queryByRole } = render(NoteTitlebar, {
      title: "旧标题",
      color: "yellow",
      ontitlechange,
    });

    await fireEvent.click(getByRole("button", { name: "编辑标题：旧标题" }));
    const input = getByLabelText("便签标题");
    await fireEvent.input(input, { target: { value: "  新标题  " } });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(ontitlechange).toHaveBeenCalledOnce();
    expect(ontitlechange).toHaveBeenCalledWith("新标题");
    expect(queryByRole("textbox", { name: "便签标题" })).not.toBeInTheDocument();
    expect(getByRole("button", { name: "编辑标题：新标题" })).toBeInTheDocument();
  });

  it("commits on blur and normalizes an empty title", async () => {
    const ontitlechange = vi.fn();
    const { getByLabelText, getByRole } = render(NoteTitlebar, {
      title: "旧标题",
      color: "blue",
      ontitlechange,
    });

    await fireEvent.click(getByRole("button", { name: "编辑标题：旧标题" }));
    const input = getByLabelText("便签标题");
    await fireEvent.input(input, { target: { value: "   " } });
    await fireEvent.blur(input);

    expect(ontitlechange).toHaveBeenCalledWith("无标题便签");
    expect(getByRole("button", { name: "编辑标题：无标题便签" })).toBeInTheDocument();
  });

  it("cancels the edit with Escape", async () => {
    const ontitlechange = vi.fn();
    const { getByLabelText, getByRole } = render(NoteTitlebar, {
      title: "保留标题",
      color: "green",
      ontitlechange,
    });

    await fireEvent.click(getByRole("button", { name: "编辑标题：保留标题" }));
    const input = getByLabelText("便签标题");
    await fireEvent.input(input, { target: { value: "不应保存" } });
    await fireEvent.keyDown(input, { key: "Escape" });

    expect(ontitlechange).not.toHaveBeenCalled();
    expect(getByRole("button", { name: "编辑标题：保留标题" })).toBeInTheDocument();
  });
});

describe("NoteTitlebar editor style control", () => {
  it("toggles the current note style without opening a dropdown", async () => {
    const oneditormodechange = vi.fn();
    const { getByRole, queryByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "purple",
      editorMode: "typora",
      oneditormodechange,
    });
    const button = getByRole("button", { name: "切换编辑样式（当前：Markdown）" });

    expect(button).toHaveAttribute("data-tooltip", "当前编辑样式：Markdown");
    expect(button).toHaveAttribute("aria-pressed", "false");
    expect(queryByRole("combobox", { name: "便签编辑样式" })).not.toBeInTheDocument();

    await fireEvent.click(button);

    expect(oneditormodechange).toHaveBeenCalledWith("plain");
  });

  it("shows the plain-text state in the tooltip and toggles back to Markdown", async () => {
    const oneditormodechange = vi.fn();
    const { getByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "purple",
      editorMode: "plain",
      oneditormodechange,
    });
    const button = getByRole("button", { name: "切换编辑样式（当前：纯文本）" });

    expect(button).toHaveAttribute("data-tooltip", "当前编辑样式：纯文本");
    expect(button).toHaveAttribute("aria-pressed", "true");
    await fireEvent.click(button);
    expect(oneditormodechange).toHaveBeenCalledWith("typora");
  });
});

describe("NoteTitlebar utility controls", () => {
  it("keeps the left-edge new-note tooltip inside narrow note windows", () => {
    const { getByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      onnew: vi.fn(),
    });

    const button = getByRole("button", { name: "新建便签" });
    expect(button).toHaveAttribute("data-tooltip-placement", "bottom");
    expect(button).toHaveAttribute("data-tooltip-align", "start");
  });

  it("opens a compact tools menu immediately before close and launches the selected tool", async () => {
    const ontoolopen = vi.fn();
    const { getByRole, queryByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      ontoolopen,
      onclose: vi.fn(),
    });
    const toolsButton = getByRole("button", { name: "小工具" });
    const closeButton = getByRole("button", { name: "关闭窗口" });

    expect(toolsButton.parentElement?.nextElementSibling).toBe(closeButton);
    await fireEvent.click(toolsButton);

    expect(getByRole("menu", { name: "小工具" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "计时器" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "提醒" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "任务甘特图" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "截图(F1)" })).toBeInTheDocument();

    await fireEvent.click(getByRole("menuitem", { name: "任务甘特图" }));
    expect(ontoolopen).toHaveBeenCalledWith("gantt");
    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();
  });

  it("shows the configured screenshot shortcut in the tools menu", async () => {
    const { getByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      screenshotShortcut: "Ctrl+Shift+S",
      ontoolopen: vi.fn(),
    });

    await fireEvent.click(getByRole("button", { name: "小工具" }));
    expect(getByRole("menuitem", { name: "截图(Ctrl+Shift+S)" })).toBeInTheDocument();
  });

  it("closes the tools menu with Escape without launching anything", async () => {
    const ontoolopen = vi.fn();
    const { getByRole, queryByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      ontoolopen,
      onclose: vi.fn(),
    });

    await fireEvent.click(getByRole("button", { name: "小工具" }));
    await fireEvent.keyDown(window, { key: "Escape" });

    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();
    expect(ontoolopen).not.toHaveBeenCalled();
  });
});

describe("NoteTitlebar readonly control", () => {
  it("exposes a pressed lock button and reports readonly changes", async () => {
    const onreadonlychange = vi.fn();
    const { getByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      readonly: false,
      onreadonlychange,
    });

    const button = getByRole("button", { name: "进入只读模式" });
    expect(button).toHaveAttribute("aria-pressed", "false");
    expect(button).toHaveAttribute("data-tooltip", "只读模式");

    await fireEvent.click(button);

    expect(onreadonlychange).toHaveBeenCalledOnce();
    expect(onreadonlychange).toHaveBeenCalledWith(true);
  });

  it("keeps the bold title display and prevents editing while readonly", () => {
    const { getByRole, getByTitle, queryByRole } = render(NoteTitlebar, {
      title: "只读标题",
      color: "blue",
      readonly: true,
      ontitlechange: vi.fn(),
      onreadonlychange: vi.fn(),
    });

    expect(queryByRole("button", { name: "编辑标题：只读标题" })).not.toBeInTheDocument();
    expect(getByTitle("只读标题")).toHaveClass("window-title");
    expect(getByRole("button", { name: "退出只读模式" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("cancels an active title edit when readonly is enabled", async () => {
    const ontitlechange = vi.fn();
    const rendered = render(NoteTitlebar, {
      title: "原标题",
      color: "green",
      readonly: false,
      ontitlechange,
    });

    await fireEvent.click(rendered.getByRole("button", { name: "编辑标题：原标题" }));
    const input = rendered.getByLabelText("便签标题");
    await fireEvent.input(input, { target: { value: "未保存标题" } });
    await rendered.rerender({
      title: "原标题",
      color: "green",
      readonly: true,
      ontitlechange,
    });

    expect(rendered.queryByRole("textbox", { name: "便签标题" })).not.toBeInTheDocument();
    expect(rendered.getByTitle("原标题")).toBeInTheDocument();
    expect(ontitlechange).not.toHaveBeenCalled();
  });
});

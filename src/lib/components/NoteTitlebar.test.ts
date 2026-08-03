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
    const onnotesopen = vi.fn();
    const onnoteopen = vi.fn();
    const { getByRole, queryByRole } = render(NoteTitlebar, {
      title: "便签",
      color: "yellow",
      ontoolopen,
      onnotesopen,
      onnoteopen,
      onclose: vi.fn(),
    });
    const toolsButton = getByRole("button", { name: "小工具" });
    const notesButton = getByRole("button", { name: "便签列表" });
    const closeButton = getByRole("button", { name: "关闭窗口" });

    expect(toolsButton.parentElement?.nextElementSibling).toBe(notesButton.parentElement);
    expect(notesButton.parentElement?.nextElementSibling).toBe(closeButton);
    await fireEvent.click(toolsButton);

    expect(getByRole("menu", { name: "小工具" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "计时器" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "提醒" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "任务甘特图" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "密码管理器" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "截图(F1)" })).toBeInTheDocument();

    await fireEvent.click(getByRole("menuitem", { name: "任务甘特图" }));
    expect(ontoolopen).toHaveBeenCalledWith("gantt");
    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();
  });

  it("loads all notes, marks the current note, and opens a selected note", async () => {
    const onnotesopen = vi.fn().mockResolvedValue(undefined);
    const onnoteopen = vi.fn().mockResolvedValue(undefined);
    const longTitle = "这是一个用于验证标题截断提示的非常非常长的便签标题";
    const { getByRole, queryByRole, container } = render(NoteTitlebar, {
      title: "当前便签",
      color: "yellow",
      currentNoteId: "note-current",
      notes: [
        {
          id: "note-current",
          title: "当前便签",
          color: "yellow",
          pinned: true,
          updatedAt: "2026-08-03T08:00:00.000Z",
        },
        {
          id: "note-other",
          title: longTitle,
          color: "blue",
          pinned: false,
          updatedAt: "2026-08-03T09:00:00.000Z",
        },
      ],
      onnotesopen,
      onnoteopen,
      onclose: vi.fn(),
    });

    await fireEvent.click(getByRole("button", { name: "便签列表" }));

    expect(onnotesopen).toHaveBeenCalledOnce();
    expect(getByRole("menu", { name: "便签列表" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: /当前便签/ })).toHaveAttribute("aria-current", "page");
    const other = getByRole("menuitem", { name: longTitle });
    expect(other).toHaveAttribute("title", longTitle);
    expect(container.querySelector('[data-note-id="note-current"] .note-color-dot')).toHaveAttribute(
      "data-color",
      "yellow",
    );

    await fireEvent.click(other);

    expect(onnoteopen).toHaveBeenCalledWith("note-other");
    expect(queryByRole("menu", { name: "便签列表" })).not.toBeInTheDocument();
  });

  it("shows loading and empty states and closes the note list with Escape or outside click", async () => {
    const rendered = render(NoteTitlebar, {
      title: "便签",
      color: "gray",
      notes: [],
      notesLoading: true,
      onnotesopen: vi.fn(),
      onnoteopen: vi.fn(),
    });

    await fireEvent.click(rendered.getByRole("button", { name: "便签列表" }));
    expect(rendered.getByRole("status")).toHaveTextContent("正在加载便签");

    await rendered.rerender({
      title: "便签",
      color: "gray",
      notes: [],
      notesLoading: false,
      onnotesopen: vi.fn(),
      onnoteopen: vi.fn(),
    });
    expect(rendered.getByText("暂无便签")).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Escape" });
    expect(rendered.queryByRole("menu", { name: "便签列表" })).not.toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "便签列表" }));
    await fireEvent.click(document.body);
    expect(rendered.queryByRole("menu", { name: "便签列表" })).not.toBeInTheDocument();
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

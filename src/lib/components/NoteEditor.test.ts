import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import type { EditorView } from "@codemirror/view";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import NoteEditor from "./NoteEditor.svelte";

const clipboardWriteText = vi.fn(async (_text: string) => undefined);
let previousClipboardDescriptor: PropertyDescriptor | undefined;

beforeEach(() => {
  previousClipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  clipboardWriteText.mockReset();
  clipboardWriteText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: clipboardWriteText },
  });
});

afterEach(() => {
  window.getSelection()?.removeAllRanges();
  cleanup();
  if (previousClipboardDescriptor) {
    Object.defineProperty(navigator, "clipboard", previousClipboardDescriptor);
  } else {
    Reflect.deleteProperty(navigator, "clipboard");
  }
});

describe("NoteEditor", () => {
  it("mounts CodeMirror in Typora mode by default", async () => {
    const { container } = render(NoteEditor, {
      value: "# 标题\n\n正文",
    });

    await waitFor(() => expect(container.querySelector(".cm-editor")).not.toBeNull());
    expect(container.querySelector(".cm-content")?.getAttribute("contenteditable")).toBe("true");
    expect(container.querySelector(".note-editor")).toHaveClass("is-typora");
  });

  it("keeps Markdown characters literal and hides Markdown actions in plain text mode", async () => {
    const { container, queryByRole } = render(NoteEditor, {
      value: "# 标题 **粗体**",
      mode: "plain",
    });

    await waitFor(() => expect(container.querySelector(".cm-editor")).not.toBeNull());
    expect(container.querySelector(".note-editor")).toHaveClass("is-plain");
    expect(container.querySelector(".cm-line")?.textContent).toBe("# 标题 **粗体**");
    expect(container.querySelector(".cm-typora-heading")).not.toBeInTheDocument();
    expect(queryByRole("button", { name: "粗体" })).not.toBeInTheDocument();
    expect(queryByRole("button", { name: "插入图片" })).not.toBeInTheDocument();
    expect(queryByRole("button", { name: "插入分割线" })).not.toBeInTheDocument();
    expect(queryByRole("button", { name: "切换源码模式" })).not.toBeInTheDocument();
    expect(queryByRole("button", { name: "查找" })).toBeInTheDocument();
  });

  it("inserts a horizontal rule from the Typora toolbar", async () => {
    let view: EditorView | undefined;
    const { getByRole, container } = render(NoteEditor, {
      value: "前后",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    await fireEvent.click(getByRole("button", { name: "插入分割线" }));

    expect(view?.state.doc.toString()).toBe("---\n\n前后");
    expect(container.querySelector(".md-typora-horizontal-rule")).toBeInTheDocument();
  });

  it("highlights selected text from the Typora toolbar", async () => {
    let view: EditorView | undefined;
    const { getByRole, container } = render(NoteEditor, {
      value: "需要高亮",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    view!.dispatch({ selection: { anchor: 2, head: 4 } });
    await fireEvent.click(getByRole("button", { name: "高亮" }));

    expect(view!.state.doc.toString()).toBe("需要==高亮==");
    expect(view!.state.sliceDoc(view!.state.selection.main.from, view!.state.selection.main.to)).toBe("高亮");
    expect(container.querySelector(".cm-typora-highlight")).toBeInTheDocument();
  });

  it("copies only the latest keyboard-style editor selection after a short debounce", async () => {
    let view: EditorView | undefined;
    const { container } = render(NoteEditor, {
      value: "甲乙 丙丁",
      mode: "plain",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    const editorContent = container.querySelector<HTMLElement>(".cm-content")!;
    view!.dispatch({ selection: { anchor: 0, head: 2 } });
    await fireEvent.keyUp(editorContent, { key: "ArrowRight", shiftKey: true });
    view!.dispatch({ selection: { anchor: 3, head: 5 } });
    await fireEvent.keyUp(editorContent, { key: "ArrowRight", shiftKey: true });

    await waitFor(() => expect(clipboardWriteText).toHaveBeenCalledOnce());
    expect(clipboardWriteText).toHaveBeenCalledWith("丙丁");
  });

  it("does not copy programmatic selection changes or formatting key releases", async () => {
    let view: EditorView | undefined;
    const { container } = render(NoteEditor, {
      value: "程序选区",
      mode: "plain",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    view!.dispatch({ selection: { anchor: 0, head: 4 } });
    view!.dispatch({
      changes: [
        { from: 0, insert: "**" },
        { from: 4, insert: "**" },
      ],
      selection: { anchor: 2, head: 6 },
    });
    await fireEvent.keyUp(container.querySelector<HTMLElement>(".cm-content")!, {
      key: "b",
      ctrlKey: true,
    });
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(clipboardWriteText).not.toHaveBeenCalled();
    expect(view!.state.doc.toString()).toBe("**程序选区**");
  });

  it("copies a mouse-selected rendered block but ignores selections outside the note body", async () => {
    const { container } = render(NoteEditor, {
      value: [
        "光标行",
        "",
        "| 名称 | 状态 |",
        "| --- | --- |",
        "| 官网 | 完成 |",
      ].join("\n"),
      mode: "typora",
      readonly: true,
    });

    const renderedCell = await waitFor(() => {
      const cell = container.querySelector<HTMLElement>(".md-typora-table-widget tbody td");
      expect(cell).toBeInTheDocument();
      return cell!;
    });
    const selection = window.getSelection()!;
    const range = document.createRange();
    range.selectNodeContents(renderedCell);
    selection.removeAllRanges();
    selection.addRange(range);
    await fireEvent.pointerUp(renderedCell, { button: 0 });

    await waitFor(() => expect(clipboardWriteText).toHaveBeenCalledWith("官网"));

    clipboardWriteText.mockClear();
    const outside = document.createElement("div");
    outside.textContent = "外部内容";
    document.body.append(outside);
    const outsideRange = document.createRange();
    outsideRange.selectNodeContents(outside);
    selection.removeAllRanges();
    selection.addRange(outsideRange);
    await fireEvent.pointerUp(container.querySelector<HTMLElement>(".editor-host")!, { button: 0 });

    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(clipboardWriteText).not.toHaveBeenCalled();
    outside.remove();
  });

  it("ignores collapsed, whitespace-only, and IME composition selections", async () => {
    let view: EditorView | undefined;
    const { container } = render(NoteEditor, {
      value: "正文  输入",
      mode: "plain",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    const content = container.querySelector<HTMLElement>(".cm-content")!;
    view!.dispatch({ selection: { anchor: 1 } });
    await fireEvent.keyUp(content, { key: "ArrowLeft" });
    view!.dispatch({ selection: { anchor: 2, head: 4 } });
    await fireEvent.keyUp(content, { key: "ArrowRight", shiftKey: true });
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(clipboardWriteText).not.toHaveBeenCalled();

    await fireEvent.compositionStart(content);
    view!.dispatch({ selection: { anchor: 0, head: 2 } });
    await fireEvent.keyUp(content, { key: "Process", keyCode: 229, isComposing: true });
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(clipboardWriteText).not.toHaveBeenCalled();
    await fireEvent.compositionEnd(content);
  });

  it("keeps editing usable when clipboard access fails", async () => {
    let view: EditorView | undefined;
    clipboardWriteText.mockRejectedValueOnce(new Error("clipboard denied"));
    const { container } = render(NoteEditor, {
      value: "可继续编辑",
      mode: "plain",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    view!.dispatch({ selection: { anchor: 0, head: 2 } });
    await fireEvent.keyUp(container.querySelector<HTMLElement>(".cm-content")!, {
      key: "ArrowRight",
      shiftKey: true,
    });
    await waitFor(() => expect(clipboardWriteText).toHaveBeenCalledWith("可继"));

    view!.dispatch({ changes: { from: view!.state.doc.length, insert: "。" } });
    expect(view!.state.doc.toString()).toBe("可继续编辑。");
  });

  it("cancels a pending selection copy when the editor unmounts", async () => {
    let view: EditorView | undefined;
    const { container, unmount } = render(NoteEditor, {
      value: "卸载测试",
      mode: "plain",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    view!.dispatch({ selection: { anchor: 0, head: 2 } });
    await fireEvent.keyUp(container.querySelector<HTMLElement>(".cm-content")!, {
      key: "ArrowRight",
      shiftKey: true,
    });
    unmount();

    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(clipboardWriteText).not.toHaveBeenCalled();
  });
});

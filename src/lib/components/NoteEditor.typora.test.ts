import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import type { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it, vi } from "vitest";
import NoteEditor from "./NoteEditor.svelte";

const looseListMarkdown = "### asaa\n\n- 111\n\n- 222\n\n- 333\n\n- 5555\n\n-";

afterEach(cleanup);

async function renderTyporaEditor(onchange = vi.fn()) {
  let view: EditorView | undefined;
  const rendered = render(NoteEditor, {
    value: looseListMarkdown,
    mode: "typora",
    onchange,
    onready: (detail) => {
      view = detail.view;
    },
  });

  await waitFor(() => expect(view).toBeDefined());
  return { ...rendered, onchange, view: view! };
}

describe("NoteEditor Typora mode", () => {
  it("keeps loose lists compact without replacing whole Markdown blocks", async () => {
    const { container, view } = await renderTyporaEditor();

    await waitFor(() => {
      expect(container.querySelectorAll(".md-typora-list-marker")).toHaveLength(5);
      expect(container.querySelectorAll(".cm-typora-collapsed-gap")).toHaveLength(5);
    });

    expect(container.querySelector(".note-editor")).toHaveClass("is-typora");
    expect(view.state.doc.toString()).toBe(looseListMarkdown);
  });

  it("reveals and edits a trailing empty list item when the cursor enters its line", async () => {
    const { container, onchange, view } = await renderTyporaEditor();

    await waitFor(() => expect(container.querySelectorAll(".md-typora-list-marker")).toHaveLength(5));
    view.dispatch({ selection: { anchor: looseListMarkdown.length } });

    await waitFor(() => {
      expect(container.querySelectorAll(".md-typora-list-marker")).toHaveLength(4);
      const lines = container.querySelectorAll<HTMLElement>(".cm-line");
      expect(lines.item(lines.length - 1).textContent?.trim()).toBe("-");
    });

    view.dispatch({ changes: { from: looseListMarkdown.length, insert: " 最后一项" } });

    await waitFor(() => {
      expect(view.state.doc.toString()).toBe(`${looseListMarkdown} 最后一项`);
      expect(onchange).toHaveBeenLastCalledWith(`${looseListMarkdown} 最后一项`);
    });
  });

  it("only hides a heading marker while the cursor is outside that heading", async () => {
    const { container, view } = await renderTyporaEditor();
    view.dispatch({ selection: { anchor: looseListMarkdown.length } });

    await waitFor(() => {
      expect(container.querySelector(".cm-typora-heading")).toBeInTheDocument();
      expect(container.querySelector(".cm-typora-hidden-mark")).toBeInTheDocument();
      expect(container.querySelector<HTMLElement>(".cm-line")?.textContent).toBe("asaa");
    });

    view.dispatch({ selection: { anchor: 1 } });

    await waitFor(() => {
      expect(container.querySelector(".cm-typora-hidden-mark")).not.toBeInTheDocument();
      expect(container.querySelector<HTMLElement>(".cm-line")?.textContent).toBe("### asaa");
    });
    expect(view.state.doc.toString()).toBe(looseListMarkdown);
  });

  it("renders highlight markers outside the active line and reveals them for editing", async () => {
    let view: EditorView | undefined;
    const value = "光标行\n\n==高亮文字==";
    const { container } = render(NoteEditor, {
      value,
      mode: "typora",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    await waitFor(() => {
      expect(container.querySelector(".cm-typora-highlight")).toBeInTheDocument();
      expect(container.querySelectorAll(".cm-typora-hidden-mark")).toHaveLength(2);
      expect(container.querySelectorAll<HTMLElement>(".cm-line").item(2).textContent).toBe("高亮文字");
    });

    view!.dispatch({ selection: { anchor: value.indexOf("高亮") } });
    await waitFor(() => {
      expect(container.querySelectorAll(".cm-typora-hidden-mark")).toHaveLength(0);
      expect(container.querySelectorAll(".cm-typora-source-mark")).toHaveLength(2);
      expect(container.querySelectorAll<HTMLElement>(".cm-line").item(2).textContent).toBe("==高亮文字==");
    });
  });

  it("keeps highlight syntax literal in source mode", async () => {
    let view: EditorView | undefined;
    const value = "==高亮文字==";
    const { container, getByRole } = render(NoteEditor, {
      value,
      mode: "typora",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    await fireEvent.click(getByRole("button", { name: "切换源码模式" }));

    await waitFor(() => expect(container.querySelector(".note-editor")).toHaveClass("is-source"));
    expect(container.querySelector(".cm-typora-highlight")).not.toBeInTheDocument();
    expect(container.querySelector<HTMLElement>(".cm-line")?.textContent).toBe(value);
    expect(view!.state.doc.toString()).toBe(value);
  });

  it("renders highlighting without markers or a toolbar while read-only", async () => {
    const { container, queryByRole } = render(NoteEditor, {
      value: "==只读高亮==",
      mode: "typora",
      readonly: true,
    });

    await waitFor(() => expect(container.querySelector(".cm-typora-highlight")).toBeInTheDocument());
    expect(queryByRole("toolbar", { name: "便签编辑工具栏" })).not.toBeInTheDocument();
    expect(container.querySelector<HTMLElement>(".cm-line")?.textContent).toBe("只读高亮");
    expect(container.querySelectorAll(".cm-typora-hidden-mark")).toHaveLength(2);
  });

  it("toggles source locally and preserves the document and cursor", async () => {
    const rendered = await renderTyporaEditor();
    const { container, getByRole, view } = rendered;
    const anchor = looseListMarkdown.indexOf("333") + 2;
    view.dispatch({ selection: { anchor } });

    await fireEvent.click(getByRole("button", { name: "切换源码模式" }));
    await waitFor(() => expect(container.querySelector(".note-editor")).toHaveClass("is-source"));
    expect(view.state.doc.toString()).toBe(looseListMarkdown);
    expect(view.state.selection.main.head).toBe(anchor);

    await fireEvent.click(getByRole("button", { name: "切换源码模式" }));
    await waitFor(() => expect(container.querySelector(".note-editor")).toHaveClass("is-typora"));
    expect(view.state.doc.toString()).toBe(looseListMarkdown);
    expect(view.state.selection.main.head).toBe(anchor);
  });

  it("leaves source mode and keeps every Markdown marker hidden when made read-only", async () => {
    let view: EditorView | undefined;
    const value = "# 标题\n\n**粗体** 和 [链接](https://example.com)";
    const { container, getByRole, queryByRole, rerender } = render(NoteEditor, {
      value,
      mode: "typora",
      onready: (detail) => {
        view = detail.view;
      },
    });

    await waitFor(() => expect(view).toBeDefined());
    await fireEvent.click(getByRole("button", { name: "切换源码模式" }));
    await waitFor(() => expect(container.querySelector(".note-editor")).toHaveClass("is-source"));

    await rerender({ readonly: true });
    await waitFor(() => {
      expect(container.querySelector(".note-editor")).toHaveClass("is-typora");
      expect(queryByRole("toolbar", { name: "便签编辑工具栏" })).not.toBeInTheDocument();
      expect(container.querySelector(".cm-content")).toHaveAttribute("contenteditable", "false");
      expect(container.querySelector<HTMLElement>(".cm-line")?.textContent).toBe("标题");
      expect(container.querySelector(".cm-typora-source-mark")).not.toBeInTheDocument();
    });

    view!.dispatch({ selection: { anchor: value.indexOf("粗体") } });
    await waitFor(() => {
      expect(container.querySelector(".cm-typora-source-mark")).not.toBeInTheDocument();
      expect(container.querySelectorAll(".cm-typora-hidden-mark").length).toBeGreaterThan(0);
      expect(container.querySelectorAll<HTMLElement>(".cm-line").item(2).textContent).toBe("粗体 和 链接");
    });
  });

  it("opens rendered links only with Ctrl or Meta plus left click", async () => {
    const onopenlink = vi.fn();
    const { container } = render(NoteEditor, {
      value: "光标在这里\n\n[百度](https://baidu.com)",
      mode: "typora",
      onopenlink,
    });

    const link = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".cm-typora-link[data-link-url]");
      expect(element).toBeInTheDocument();
      return element!;
    });

    await fireEvent.click(link, { button: 0 });
    expect(onopenlink).not.toHaveBeenCalled();

    await fireEvent.click(link, { button: 0, ctrlKey: true });
    expect(onopenlink).toHaveBeenCalledOnce();
    expect(onopenlink).toHaveBeenCalledWith("https://baidu.com");
  });

  it("opens bare URLs with Ctrl plus left click", async () => {
    const onopenlink = vi.fn();
    const { container } = render(NoteEditor, {
      value: "正文 https://example.com/path?q=1。",
      mode: "typora",
      onopenlink,
    });

    const link = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".cm-typora-link[data-link-url]");
      expect(element).toBeInTheDocument();
      expect(element).toHaveAttribute("data-link-url", "https://example.com/path?q=1");
      return element!;
    });

    await fireEvent.click(link, { button: 0 });
    expect(onopenlink).not.toHaveBeenCalled();
    await fireEvent.click(link, { button: 0, ctrlKey: true });
    expect(onopenlink).toHaveBeenCalledOnce();
    expect(onopenlink).toHaveBeenCalledWith("https://example.com/path?q=1");
  });

  it("shows a hand cursor for safe links only while Ctrl is held", async () => {
    const { container } = render(NoteEditor, {
      value: "https://example.com and [unsafe](javascript:alert(1))",
      mode: "typora",
    });

    const editor = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".cm-editor");
      expect(element).toBeInTheDocument();
      return element!;
    });
    const safeLink = container.querySelector<HTMLElement>(".cm-typora-link[data-link-url]");
    expect(safeLink).toBeInTheDocument();
    expect(container.querySelector(".note-editor")).not.toHaveClass("is-link-modifier");

    await fireEvent.keyDown(editor, { key: "Control", ctrlKey: true });
    expect(container.querySelector(".note-editor")).toHaveClass("is-link-modifier");

    await fireEvent.keyUp(editor, { key: "Control", ctrlKey: false });
    expect(container.querySelector(".note-editor")).not.toHaveClass("is-link-modifier");
  });

  it("opens safe rendered links with Ctrl plus left click while read-only", async () => {
    const onopenlink = vi.fn();
    const { container, queryByRole } = render(NoteEditor, {
      value: "[官网](https://example.com)",
      mode: "typora",
      readonly: true,
      onopenlink,
    });

    const link = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".cm-typora-link[data-link-url]");
      expect(element).toBeInTheDocument();
      return element!;
    });

    expect(queryByRole("toolbar", { name: "便签编辑工具栏" })).not.toBeInTheDocument();
    await fireEvent.click(link, { button: 0, ctrlKey: true });
    expect(onopenlink).toHaveBeenCalledOnce();
    expect(onopenlink).toHaveBeenCalledWith("https://example.com");
  });

  it("never opens unsafe link protocols", async () => {
    const onopenlink = vi.fn();
    const { container } = render(NoteEditor, {
      value: "光标\n\n[危险](javascript:alert(1))",
      mode: "typora",
      onopenlink,
    });

    const link = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".cm-typora-link");
      expect(element).toBeInTheDocument();
      expect(element).not.toHaveAttribute("data-link-url");
      return element!;
    });
    await fireEvent.click(link, { button: 0, ctrlKey: true });

    expect(onopenlink).not.toHaveBeenCalled();
  });
});

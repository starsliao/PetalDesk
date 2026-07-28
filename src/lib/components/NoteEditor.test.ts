import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import type { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it } from "vitest";
import NoteEditor from "./NoteEditor.svelte";

afterEach(cleanup);

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
});

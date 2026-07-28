import { describe, expect, it } from "vitest";
import { EditorSelection, EditorState } from "@codemirror/state";
import { history, undo } from "@codemirror/commands";
import { EditorView } from "@codemirror/view";
import {
  applyInlineFormat,
  escapeMarkdownAlt,
  insertHorizontalRule,
  insertImageMarkdown,
} from "./format";

function createView(doc: string, from: number, to = from): EditorView {
  return new EditorView({
    state: EditorState.create({
      doc,
      selection: EditorSelection.single(from, to),
      extensions: [history()],
    }),
  });
}

describe("Markdown editor formatting", () => {
  it("wraps selected text as bold in one undoable edit", () => {
    const view = createView("一段文字", 2, 4);

    applyInlineFormat(view, "bold");

    expect(view.state.doc.toString()).toBe("一段**文字**");
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe("文字");
    view.destroy();
  });

  it("inserts a link template and selects its label", () => {
    const view = createView("", 0);

    applyInlineFormat(view, "link");

    expect(view.state.doc.toString()).toBe("[链接文字](https://)");
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe("链接文字");
    view.destroy();
  });

  it("wraps a selection as a separately undoable highlight", () => {
    const view = createView("已有 高亮内容", 3, 7);
    view.dispatch({ changes: { from: 2, insert: "新" } });

    applyInlineFormat(view, "highlight");

    expect(view.state.doc.toString()).toBe("已有新 ==高亮内容==");
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe("高亮内容");
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("已有新 高亮内容");
    view.destroy();
  });

  it("inserts a highlight template and selects its placeholder", () => {
    const view = createView("", 0);

    applyInlineFormat(view, "highlight");

    expect(view.state.doc.toString()).toBe("==高亮文字==");
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe("高亮文字");
    view.destroy();
  });

  it("escapes alt text and encodes unsafe path punctuation", () => {
    const view = createView("前后", 1);

    const markdown = insertImageMarkdown(view, "assets/a (1).png", "图[1]");

    expect(markdown).toBe("![图\\[1\\]](assets/a%20%281%29.png)");
    expect(view.state.doc.toString()).toBe(`前\n\n${markdown}\n\n后`);
    expect(escapeMarkdownAlt("a]b")).toBe("a\\]b");
    view.destroy();
  });

  it("separates an image from content at the end of the note", () => {
    const view = createView("列表项", 3);

    const markdown = insertImageMarkdown(view, "assets/image.png", "图片");

    expect(view.state.doc.toString()).toBe(`列表项\n\n${markdown}`);
    view.destroy();
  });

  it("inserts a horizontal rule as a separate undoable block", () => {
    const view = createView("前", 1);
    view.dispatch({
      changes: { from: 1, insert: "后" },
      selection: { anchor: 2 },
    });

    expect(insertHorizontalRule(view)).toBe("---");
    expect(view.state.doc.toString()).toBe("前后\n\n---\n\n");
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("前后");
    view.destroy();
  });

  it("does not duplicate existing blank lines around a horizontal rule", () => {
    const view = createView("正文\n\n下一段", 4);

    insertHorizontalRule(view);

    expect(view.state.doc.toString()).toBe("正文\n\n---\n\n下一段");
    view.destroy();
  });
});

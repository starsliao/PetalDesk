import { EditorSelection, type ChangeSpec } from "@codemirror/state";
import { isolateHistory } from "@codemirror/commands";
import type { EditorView } from "@codemirror/view";

export type InlineFormat = "bold" | "italic" | "highlight" | "link";

const FORMAT_MARKERS: Record<Exclude<InlineFormat, "link">, [string, string]> = {
  bold: ["**", "**"],
  italic: ["*", "*"],
  highlight: ["==", "=="],
};

const FORMAT_PLACEHOLDERS: Record<Exclude<InlineFormat, "link">, string> = {
  bold: "粗体文字",
  italic: "斜体文字",
  highlight: "高亮文字",
};

function wrapSelection(view: EditorView, before: string, after: string, placeholder: string): boolean {
  const changes: ChangeSpec[] = [];
  const ranges = view.state.selection.ranges.map((range) => {
    const selected = view.state.sliceDoc(range.from, range.to);
    const content = selected || placeholder;
    changes.push({ from: range.from, to: range.to, insert: `${before}${content}${after}` });

    const from = range.from + before.length;
    return EditorSelection.range(from, from + content.length);
  });

  view.dispatch({
    changes,
    selection: EditorSelection.create(ranges, view.state.selection.mainIndex),
    annotations: isolateHistory.of("full"),
    scrollIntoView: true,
  });
  view.focus();
  return true;
}

export function applyInlineFormat(view: EditorView, format: InlineFormat): boolean {
  if (format === "link") {
    return wrapSelection(view, "[", "](https://)", "链接文字");
  }

  const [before, after] = FORMAT_MARKERS[format];
  return wrapSelection(view, before, after, FORMAT_PLACEHOLDERS[format]);
}

export function escapeMarkdownAlt(text: string): string {
  return text.replaceAll("\\", "\\\\").replaceAll("]", "\\]").replaceAll("[", "\\[");
}

export function insertImageMarkdown(view: EditorView, path: string, alt: string): string {
  const safePath = path.replaceAll(" ", "%20").replaceAll("(", "%28").replaceAll(")", "%29");
  const markdown = `![${escapeMarkdownAlt(alt)}](${safePath})`;
  const range = view.state.selection.main;
  const before = view.state.sliceDoc(0, range.from);
  const after = view.state.sliceDoc(range.to);
  const prefix =
    before.length === 0 || before.endsWith("\n\n") ? "" : before.endsWith("\n") ? "\n" : "\n\n";
  const suffix =
    after.length === 0 || after.startsWith("\n\n") ? "" : after.startsWith("\n") ? "\n" : "\n\n";
  const insertion = `${prefix}${markdown}${suffix}`;

  view.dispatch({
    changes: { from: range.from, to: range.to, insert: insertion },
    selection: { anchor: range.from + prefix.length + markdown.length },
    annotations: isolateHistory.of("full"),
    scrollIntoView: true,
  });
  view.focus();
  return markdown;
}

export function insertHorizontalRule(view: EditorView): string {
  const range = view.state.selection.main;
  const before = view.state.sliceDoc(0, range.from);
  const after = view.state.sliceDoc(range.to);
  const prefix =
    before.length === 0 || before.endsWith("\n\n") ? "" : before.endsWith("\n") ? "\n" : "\n\n";
  const suffix =
    after.startsWith("\n\n") ? "" : after.startsWith("\n") ? "\n" : "\n\n";
  const insertion = `${prefix}---${suffix}`;

  view.dispatch({
    changes: { from: range.from, to: range.to, insert: insertion },
    selection: { anchor: range.from + insertion.length },
    annotations: isolateHistory.of("full"),
    scrollIntoView: true,
  });
  view.focus();
  return "---";
}

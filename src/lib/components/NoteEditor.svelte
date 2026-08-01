<script lang="ts">
  import { onMount } from "svelte";
  import { defaultKeymap, history, historyKeymap, redo, redoDepth, undo, undoDepth } from "@codemirror/commands";
  import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
  import { syntaxTree } from "@codemirror/language";
  import { openSearchPanel, search, searchKeymap } from "@codemirror/search";
  import {
    Compartment,
    EditorSelection,
    EditorState,
    StateField,
    type Range,
  } from "@codemirror/state";
  import {
    Decoration,
    dropCursor,
    EditorView,
    WidgetType,
    drawSelection,
    keymap,
    placeholder as editorPlaceholder,
    type DecorationSet,
  } from "@codemirror/view";
  import {
    Bold,
    Code2,
    Highlighter,
    ImagePlus,
    Italic,
    Link,
    Minus,
    Redo2,
    Search,
    Undo2,
  } from "@lucide/svelte";
  import {
    applyInlineFormat,
    insertHorizontalRule,
    insertImageMarkdown,
    renderMarkdown,
    type AssetImporter,
    type AssetInsertDetail,
    type EditorErrorDetail,
    type EditorMode,
    type EditorReadyDetail,
  } from "../editor";

  interface Props {
    value?: string;
    mode?: EditorMode;
    placeholder?: string;
    autofocus?: boolean;
    readonly?: boolean;
    assetUrls?: Readonly<Record<string, string>>;
    onasset?: AssetImporter;
    onchange?: (value: string) => void;
    onopenlink?: (url: string) => void;
    onassetinsert?: (detail: AssetInsertDetail) => void;
    onerror?: (detail: EditorErrorDetail) => void;
    onready?: (detail: EditorReadyDetail) => void;
  }

  let {
    value = "",
    mode = "typora",
    placeholder = "写点什么……",
    autofocus = false,
    readonly = false,
    assetUrls = {},
    onasset,
    onchange,
    onopenlink,
    onassetinsert,
    onerror,
    onready,
  }: Props = $props();

  let editorHost: HTMLDivElement;
  let fileInput: HTMLInputElement;
  let view: EditorView | undefined;
  let internalValue = "";
  let canUndo = $state(false);
  let canRedo = $state(false);
  let importing = $state(false);
  let sourceMode = $state(false);
  let linkModifierActive = $state(false);
  let previewAssetUrls: Readonly<Record<string, string>> = {};

  const modeCompartment = new Compartment();
  const readonlyCompartment = new Compartment();
  const editableCompartment = new Compartment();

  interface HighlightInlineContext {
    char(position: number): number;
    slice(from: number, to: number): string;
    addDelimiter(
      type: { resolve: string; mark: string },
      from: number,
      to: number,
      open: boolean,
      close: boolean,
    ): number;
  }

  const highlightDelimiter = { resolve: "Highlight", mark: "HighlightMark" };
  const highlightMarkdownExtension = {
    defineNodes: ["Highlight", "HighlightMark"],
    parseInline: [{
      name: "Highlight",
      before: "Emphasis",
      parse(context: HighlightInlineContext, next: number, position: number): number {
        if (next !== 61 || context.char(position + 1) !== 61 || context.char(position + 2) === 61) {
          return -1;
        }

        const before = context.slice(position - 1, position);
        const after = context.slice(position + 2, position + 3);
        return context.addDelimiter(
          highlightDelimiter,
          position,
          position + 2,
          after.length > 0 && !/\s/.test(after),
          before.length > 0 && !/\s/.test(before),
        );
      },
    }],
  };

  // Keep link metadata restricted to protocols accepted by the existing open-link
  // path.  The click handler still performs the final validation before invoking
  // the parent callback, so decorations cannot bypass the safety check.
  function normalizeLinkUrl(url: string): string {
    let normalized = url.trim();
    // Markdown's URL token includes sentence punctuation in a few cases (for
    // example, a full-width Chinese stop). Do not send that punctuation to the
    // external opener when the URL is followed by prose.
    while (/[.,!?;:，。！？；：、…]$/u.test(normalized)) {
      normalized = normalized.slice(0, -1);
    }
    for (const [opening, closing] of [["(", ")"], ["[", "]"], ["{", "}"], ["<", ">"]]) {
      const closingCount = [...normalized].filter((character) => character === closing).length;
      const openingCount = [...normalized].filter((character) => character === opening).length;
      if (closingCount > openingCount && normalized.endsWith(closing)) {
        normalized = normalized.slice(0, -1);
      }
    }
    return normalized;
  }

  function isOpenableLink(url: string): boolean {
    return /^(?:https?:|mailto:|tel:)/i.test(normalizeLinkUrl(url));
  }

  function moveCursorToWidget(view: EditorView, position: number, event: MouseEvent): void {
    if (event.button !== 0) return;
    event.preventDefault();
    view.dispatch({ selection: EditorSelection.cursor(position), scrollIntoView: true });
    view.focus();
  }

  class HiddenMarkWidget extends WidgetType {
    toDOM(): HTMLElement {
      const element = document.createElement("span");
      element.className = "cm-typora-hidden-mark";
      element.setAttribute("aria-hidden", "true");
      return element;
    }
  }

  class ListMarkerWidget extends WidgetType {
    readonly marker: string;
    readonly from: number;

    constructor(
      marker: string,
      from: number,
    ) {
      super();
      this.marker = marker;
      this.from = from;
    }

    eq(other: ListMarkerWidget): boolean {
      return this.marker === other.marker && this.from === other.from;
    }

    toDOM(view: EditorView): HTMLElement {
      const element = document.createElement("span");
      element.className = "md-typora-list-marker";
      element.textContent = /^[-+*]$/.test(this.marker) ? "•" : this.marker;
      element.addEventListener("mousedown", (event) => moveCursorToWidget(view, this.from, event));
      return element;
    }
  }

  class TaskMarkerWidget extends WidgetType {
    readonly checked: boolean;
    readonly from: number;

    constructor(
      checked: boolean,
      from: number,
    ) {
      super();
      this.checked = checked;
      this.from = from;
    }

    eq(other: TaskMarkerWidget): boolean {
      return this.checked === other.checked && this.from === other.from;
    }

    toDOM(view: EditorView): HTMLElement {
      const element = document.createElement("span");
      element.className = "md-typora-task-marker";
      element.setAttribute("role", "checkbox");
      element.setAttribute("aria-checked", String(this.checked));
      element.textContent = this.checked ? "☑" : "☐";
      element.addEventListener("mousedown", (event) => moveCursorToWidget(view, this.from, event));
      return element;
    }
  }

  class HorizontalRuleWidget extends WidgetType {
    readonly from: number;

    constructor(from: number) {
      super();
      this.from = from;
    }

    eq(other: HorizontalRuleWidget): boolean {
      return this.from === other.from;
    }

    toDOM(view: EditorView): HTMLElement {
      const element = document.createElement("span");
      element.className = "md-typora-horizontal-rule";
      element.setAttribute("aria-hidden", "true");
      element.addEventListener("mousedown", (event) => moveCursorToWidget(view, this.from, event));
      return element;
    }
  }

  class MarkdownImageWidget extends WidgetType {
    readonly markdownSource: string;
    readonly urls: Readonly<Record<string, string>>;
    readonly from: number;

    constructor(
      markdownSource: string,
      urls: Readonly<Record<string, string>>,
      from: number,
    ) {
      super();
      this.markdownSource = markdownSource;
      this.urls = urls;
      this.from = from;
    }

    eq(other: MarkdownImageWidget): boolean {
      return this.markdownSource === other.markdownSource && this.urls === other.urls && this.from === other.from;
    }

    toDOM(view: EditorView): HTMLElement {
      const element = document.createElement("span");
      element.className = "md-typora-image-widget";
      const template = document.createElement("template");
      template.innerHTML = renderMarkdown(this.markdownSource, { assetUrls: this.urls });
      const rendered = template.content.querySelector("img, .md-image-placeholder");
      if (rendered) {
        element.append(rendered);
      } else {
        element.textContent = this.markdownSource;
      }
      element.addEventListener("mousedown", (event) => moveCursorToWidget(view, this.from, event));
      return element;
    }
  }

  class MarkdownTableWidget extends WidgetType {
    readonly markdownSource: string;
    readonly urls: Readonly<Record<string, string>>;
    readonly from: number;

    constructor(
      markdownSource: string,
      urls: Readonly<Record<string, string>>,
      from: number,
    ) {
      super();
      this.markdownSource = markdownSource;
      this.urls = urls;
      this.from = from;
    }

    eq(other: MarkdownTableWidget): boolean {
      return this.markdownSource === other.markdownSource && this.urls === other.urls && this.from === other.from;
    }

    toDOM(view: EditorView): HTMLElement {
      const element = document.createElement("div");
      element.className = "md-typora-table-widget";
      const template = document.createElement("template");
      template.innerHTML = renderMarkdown(this.markdownSource, { assetUrls: this.urls });
      const table = template.content.querySelector("table");
      if (table) {
        element.append(table);
      } else {
        element.textContent = this.markdownSource;
      }
      element.addEventListener("mousedown", (event) => moveCursorToWidget(view, this.from, event));
      return element;
    }
  }

  function typoraPreviewExtension(
    urls: Readonly<Record<string, string>>,
    revealActiveLines: boolean,
  ) {
    const hiddenMarkWidget = new HiddenMarkWidget();

    /// Which line starts the caret currently reveals. This is the only part of
    /// the selection the decorations depend on, so it doubles as the cache key
    /// that lets same-line cursor moves reuse the previous set.
    const activeLineStartsOf = (state: EditorState): Set<number> => {
      const starts = new Set<number>();
      if (!revealActiveLines) return starts;
      for (const selection of state.selection.ranges) {
        const firstLine = state.doc.lineAt(selection.from);
        const endPosition = selection.empty ? selection.to : Math.max(selection.from, selection.to - 1);
        if (endPosition <= firstLine.to) {
          starts.add(firstLine.from);
          continue;
        }
        const lastLine = state.doc.lineAt(endPosition);
        for (let lineNumber = firstLine.number; lineNumber <= lastLine.number; lineNumber += 1) {
          starts.add(state.doc.line(lineNumber).from);
        }
      }
      return starts;
    };

    const buildDecorations = (state: EditorState, activeLineStarts: Set<number>): DecorationSet => {
      const ranges: Range<Decoration>[] = [];
      const preserveBlankLineStarts = new Set<number>();
      const collapsedLineStarts = new Set<number>();

      const isLineActive = (position: number): boolean => activeLineStarts.has(state.doc.lineAt(position).from);
      const touchesActiveLine = (from: number, to: number): boolean => {
        const firstLine = state.doc.lineAt(from).number;
        const lastLine = state.doc.lineAt(Math.max(from, to - 1)).number;
        for (let lineNumber = firstLine; lineNumber <= lastLine; lineNumber += 1) {
          if (activeLineStarts.has(state.doc.line(lineNumber).from)) return true;
        }
        return false;
      };
      const addLineClass = (position: number, className: string): void => {
        ranges.push(Decoration.line({ class: className }).range(state.doc.lineAt(position).from));
      };
      const hideMark = (from: number, to: number): void => {
        ranges.push(Decoration.replace({ widget: hiddenMarkWidget }).range(from, to));
      };
      const revealMark = (from: number, to: number): void => {
        ranges.push(Decoration.mark({ class: "cm-typora-source-mark" }).range(from, to));
      };

      syntaxTree(state).iterate({
        enter(node) {
          const parentName = node.node.parent?.name;
          // Only some branches below need the node text; slicing it up front
          // allocated a string for every node in the document instead.
          let sourceCache: string | undefined;
          const readSource = (): string => (sourceCache ??= state.sliceDoc(node.from, node.to));
          const active = touchesActiveLine(node.from, node.to);
          const heading = /^(?:ATXHeading([1-6])|SetextHeading([12]))$/.exec(node.name);

          if (heading) {
            const level = Number(heading[1] ?? heading[2]);
            addLineClass(node.from, `cm-typora-heading cm-typora-h${level}`);
            return;
          }

          switch (node.name) {
            case "Table":
              if (!active) {
                ranges.push(
                  Decoration.replace({
                    widget: new MarkdownTableWidget(readSource(), urls, node.from),
                    block: true,
                  }).range(node.from, node.to),
                );
              }
              // A table is edited as one source block. Skipping its descendants
              // prevents inline formatting in inactive rows from hiding syntax.
              return false;
            case "Highlight": {
              const marks = node.node.getChildren("HighlightMark");
              if (marks.length === 2 && marks[0].to < marks[1].from) {
                ranges.push(
                  Decoration.mark({ class: "cm-typora-highlight" }).range(marks[0].to, marks[1].from),
                );
              }
              break;
            }
            case "StrongEmphasis":
              ranges.push(Decoration.mark({ class: "cm-typora-strong" }).range(node.from, node.to));
              break;
            case "Emphasis":
              ranges.push(Decoration.mark({ class: "cm-typora-emphasis" }).range(node.from, node.to));
              break;
            case "Strikethrough":
              ranges.push(Decoration.mark({ class: "cm-typora-strikethrough" }).range(node.from, node.to));
              break;
            case "InlineCode":
              ranges.push(Decoration.mark({ class: "cm-typora-inline-code" }).range(node.from, node.to));
              break;
            case "Link":
            case "Autolink": {
              const urlNode = node.node.getChild("URL");
              const url = urlNode ? normalizeLinkUrl(state.sliceDoc(urlNode.from, urlNode.to)) : "";
              ranges.push(
                Decoration.mark({
                  class: "cm-typora-link",
                  attributes: url && isOpenableLink(url) ? { "data-link-url": url } : undefined,
                }).range(node.from, node.to),
              );
              break;
            }
            case "Image":
              if (!active) {
                ranges.push(
                  Decoration.replace({
                    widget: new MarkdownImageWidget(readSource(), urls, node.from),
                  }).range(node.from, node.to),
                );
                return false;
              }
              break;
            case "HeaderMark":
            case "EmphasisMark":
            case "StrikethroughMark":
            case "HighlightMark":
              if (isLineActive(node.from)) revealMark(node.from, node.to);
              else {
                const hideTo = node.name === "HeaderMark"
                  && parentName?.startsWith("ATXHeading")
                  && state.sliceDoc(node.to, node.to + 1) === " "
                  ? node.to + 1
                  : node.to;
                hideMark(node.from, hideTo);
              }
              if (node.name === "HeaderMark" && parentName?.startsWith("SetextHeading") && !isLineActive(node.from)) {
                collapsedLineStarts.add(state.doc.lineAt(node.from).from);
              }
              break;
            case "CodeMark":
              if (isLineActive(node.from)) {
                revealMark(node.from, node.to);
              } else {
                hideMark(node.from, node.to);
                if (parentName === "FencedCode" && node.from > (node.node.parent?.from ?? node.from)) {
                  collapsedLineStarts.add(state.doc.lineAt(node.from).from);
                }
              }
              break;
            case "LinkMark":
              if (isLineActive(node.from)) revealMark(node.from, node.to);
              else hideMark(node.from, node.to);
              break;
            case "URL":
              if (parentName === "Link" && !isLineActive(node.from)) {
                hideMark(node.from, node.to);
              } else if (parentName === "Image") {
                revealMark(node.from, node.to);
              } else {
                ranges.push(
                  Decoration.mark({
                    class: "cm-typora-link",
                    attributes: isOpenableLink(readSource())
                      ? { "data-link-url": normalizeLinkUrl(readSource()) }
                      : undefined,
                  }).range(node.from, node.to),
                );
              }
              break;
            case "ListItem":
              addLineClass(node.from, "cm-typora-list-line");
              break;
            case "ListMark":
              if (isLineActive(node.from)) {
                revealMark(node.from, node.to);
              } else {
                ranges.push(
                  Decoration.replace({
                    widget: new ListMarkerWidget(readSource(), node.from),
                  }).range(node.from, node.to),
                );
              }
              break;
            case "TaskMarker":
              if (isLineActive(node.from)) {
                revealMark(node.from, node.to);
              } else {
                ranges.push(
                  Decoration.replace({
                    widget: new TaskMarkerWidget(/x/i.test(readSource()), node.from),
                  }).range(node.from, node.to),
                );
              }
              break;
            case "QuoteMark":
              addLineClass(node.from, "cm-typora-quote-line");
              if (isLineActive(node.from)) revealMark(node.from, node.to);
              else hideMark(node.from, node.to);
              break;
            case "Blockquote": {
              const firstLine = state.doc.lineAt(node.from);
              const lastLine = state.doc.lineAt(Math.max(node.from, node.to - 1));
              for (let lineNumber = firstLine.number; lineNumber <= lastLine.number; lineNumber += 1) {
                addLineClass(state.doc.line(lineNumber).from, "cm-typora-quote-line");
              }
              break;
            }
            case "HorizontalRule":
              if (!active) {
                ranges.push(
                  Decoration.replace({
                    widget: new HorizontalRuleWidget(node.from),
                  }).range(node.from, node.to),
                );
                return false;
              }
              break;
            case "FencedCode": {
              const firstLine = state.doc.lineAt(node.from);
              const lastLine = state.doc.lineAt(Math.max(node.from, node.to - 1));
              for (let lineNumber = firstLine.number; lineNumber <= lastLine.number; lineNumber += 1) {
                const line = state.doc.line(lineNumber);
                preserveBlankLineStarts.add(line.from);
                const edgeClass = lineNumber === firstLine.number
                  ? " cm-typora-code-first"
                  : lineNumber === lastLine.number
                    ? " cm-typora-code-last"
                    : "";
                addLineClass(line.from, `cm-typora-code-line${edgeClass}`);
              }
              break;
            }
            case "CodeInfo":
              ranges.push(Decoration.mark({ class: "cm-typora-code-info" }).range(node.from, node.to));
              break;
            case "Escape":
              if (!active && node.to > node.from + 1 && readSource().startsWith("\\")) {
                hideMark(node.from, node.from + 1);
              }
              break;
          }
        },
      });

      // Sequential iteration instead of `doc.line(n)` per line: the latter is a
      // random-access lookup into the rope, repeated once per line of the whole
      // document on every rebuild.
      let lineStart = 0;
      for (const text of state.doc.iterLines()) {
        if (
          text.trim().length === 0
          && !activeLineStarts.has(lineStart)
          && !preserveBlankLineStarts.has(lineStart)
        ) {
          collapsedLineStarts.add(lineStart);
        }
        lineStart += text.length + 1;
      }

      for (const lineStart of collapsedLineStarts) {
        if (!activeLineStarts.has(lineStart)) addLineClass(lineStart, "cm-typora-collapsed-gap");
      }

      return Decoration.set(ranges, true);
    };

    // Rebuilding walked the whole syntax tree, so doing it for every selection
    // change meant a full pass per arrow keypress. Only the set of revealed
    // lines matters, and that is unchanged while the caret moves within a line.
    let lastActiveLineStarts: Set<number> | null = null;
    const sameActiveLines = (next: Set<number>): boolean => {
      if (!lastActiveLineStarts || lastActiveLineStarts.size !== next.size) return false;
      for (const start of next) {
        if (!lastActiveLineStarts.has(start)) return false;
      }
      return true;
    };

    return StateField.define<DecorationSet>({
      create(state) {
        const activeLineStarts = activeLineStartsOf(state);
        lastActiveLineStarts = activeLineStarts;
        return buildDecorations(state, activeLineStarts);
      },
      update(decorations, transaction) {
        if (!transaction.docChanged && !transaction.selection) return decorations;
        const activeLineStarts = activeLineStartsOf(transaction.state);
        if (!transaction.docChanged && sameActiveLines(activeLineStarts)) return decorations;
        lastActiveLineStarts = activeLineStarts;
        return buildDecorations(transaction.state, activeLineStarts);
      },
      provide: (field) => EditorView.decorations.from(field),
    });
  }

  function currentModeExtension(currentMode: EditorMode, showSource: boolean, isReadonly: boolean) {
    if (currentMode === "typora") {
      return [
        markdown({
          base: markdownLanguage,
          completeHTMLTags: false,
          extensions: highlightMarkdownExtension,
        }),
        ...(showSource ? [] : [typoraPreviewExtension(previewAssetUrls, !isReadonly)]),
      ];
    }
    return [];
  }

  function readonlyExtensions(isReadonly: boolean) {
    return [
      EditorState.readOnly.of(isReadonly),
      EditorView.editable.of(!isReadonly),
    ];
  }

  function updateHistoryState(): void {
    if (!view) return;
    canUndo = undoDepth(view.state) > 0;
    canRedo = redoDepth(view.state) > 0;
  }

  function format(formatName: "bold" | "italic" | "highlight" | "link"): void {
    if (view && mode === "typora" && !readonly) applyInlineFormat(view, formatName);
  }

  function insertRule(): void {
    if (view && mode === "typora" && !readonly) insertHorizontalRule(view);
  }

  function runUndo(): void {
    if (view) undo(view);
  }

  function runRedo(): void {
    if (view) redo(view);
  }

  function find(): void {
    if (view) openSearchPanel(view);
  }

  function toggleSourceMode(): void {
    if (mode === "typora" && !readonly) sourceMode = !sourceMode;
  }

  function handleEditorClick(event: MouseEvent): boolean {
    if (
      event.button !== 0
      || (!event.ctrlKey && !event.metaKey)
      || mode !== "typora"
      || sourceMode
      || !onopenlink
    ) {
      return false;
    }

    const target = event.target instanceof Element
      ? event.target
      : event.target instanceof Node
        ? event.target.parentElement
        : null;
    const link = target?.closest<HTMLElement>(".cm-typora-link[data-link-url]");
    const url = link?.dataset.linkUrl ? normalizeLinkUrl(link.dataset.linkUrl) : "";
    if (!url || !isOpenableLink(url)) return false;

    event.preventDefault();
    event.stopPropagation();
    onopenlink(url);
    return true;
  }

  function updateLinkModifier(event: KeyboardEvent | MouseEvent): void {
    linkModifierActive = event.ctrlKey || event.metaKey;
  }

  function clearLinkModifier(): void {
    linkModifierActive = false;
  }

  async function importFiles(files: FileList | File[]): Promise<void> {
    if (!view || !onasset || readonly || mode !== "typora") return;
    const images = Array.from(files).filter((file) => file.type.startsWith("image/"));
    if (images.length === 0) return;

    importing = true;
    try {
      for (const file of images) {
        const path = await onasset(file);
        if (!path || path.toLowerCase().startsWith("file:")) {
          throw new Error("资源导入必须返回飞花 - PetalDesk 数据存储中的相对路径");
        }
        const markdownText = insertImageMarkdown(view, path, file.name.replace(/\.[^.]+$/, ""));
        onassetinsert?.({ file, path, markdown: markdownText });
      }
    } catch (error) {
      onerror?.({ operation: "asset-import", error });
    } finally {
      importing = false;
    }
  }

  function handlePaste(event: ClipboardEvent): void {
    if (mode !== "typora") return;
    const files = event.clipboardData?.files;
    if (!files || !Array.from(files).some((file) => file.type.startsWith("image/"))) return;
    if (!onasset) return;
    event.preventDefault();
    void importFiles(files);
  }

  function handleDrop(event: DragEvent): void {
    if (mode !== "typora") return;
    const files = event.dataTransfer?.files;
    if (!files || !Array.from(files).some((file) => file.type.startsWith("image/"))) return;
    if (!onasset) return;
    event.preventDefault();
    if (view) {
      const position = view.posAtCoords({ x: event.clientX, y: event.clientY });
      if (position !== null) view.dispatch({ selection: { anchor: position } });
    }
    void importFiles(files);
  }

  function handleDragOver(event: DragEvent): void {
    if (!onasset || readonly || mode !== "typora") return;
    if (event.dataTransfer?.types.includes("Files")) event.preventDefault();
  }

  function openImagePicker(): void {
    if (onasset && !readonly && mode === "typora") fileInput.click();
  }

  onMount(() => {
    internalValue = value;
    previewAssetUrls = assetUrls;
    const state = EditorState.create({
      doc: value,
      extensions: [
        dropCursor(),
        drawSelection(),
        history({ minDepth: 200, newGroupDelay: 500 }),
        search({ top: true }),
        keymap.of([
          {
            key: "Mod-b",
            preventDefault: true,
            run: (currentView) => mode === "typora" && !readonly
              ? applyInlineFormat(currentView, "bold")
              : false,
          },
          {
            key: "Mod-i",
            preventDefault: true,
            run: (currentView) => mode === "typora" && !readonly
              ? applyInlineFormat(currentView, "italic")
              : false,
          },
          {
            key: "Mod-k",
            preventDefault: true,
            run: (currentView) => mode === "typora" && !readonly
              ? applyInlineFormat(currentView, "link")
              : false,
          },
          ...searchKeymap,
          ...historyKeymap,
          ...defaultKeymap,
        ]),
        editorPlaceholder(placeholder),
        modeCompartment.of(currentModeExtension(mode, sourceMode, readonly)),
        readonlyCompartment.of(EditorState.readOnly.of(readonly)),
        editableCompartment.of(EditorView.editable.of(!readonly)),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({
          "aria-label": "便签正文",
          spellcheck: "true",
        }),
        EditorView.domEventHandlers({
          click: (event) => handleEditorClick(event),
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            internalValue = update.state.doc.toString();
            value = internalValue;
            onchange?.(internalValue);
          }
          if (update.docChanged || update.selectionSet) updateHistoryState();
        }),
        EditorView.theme({
          "&": { height: "100%", backgroundColor: "transparent" },
          ".cm-scroller": { fontFamily: "inherit", lineHeight: "1.55", overflow: "auto" },
          ".cm-content": { padding: "14px 18px 48px", caretColor: "currentColor" },
          ".cm-line": { padding: "0" },
          ".cm-focused": { outline: "none" },
          ".cm-selectionBackground, ::selection": { backgroundColor: "var(--note-selection, rgba(68, 63, 44, 0.2)) !important" },
          ".cm-panels": { backgroundColor: "var(--app-surface, rgba(255, 255, 255, 0.9))", color: "var(--app-fg, #25231d)" },
          ".cm-panels-top": { borderBottom: "1px solid rgba(60, 55, 35, 0.12)" },
          ".cm-search": { padding: "6px 10px" },
          ".cm-search input": { fontFamily: "inherit", borderRadius: "6px", border: "1px solid rgba(0, 0, 0, 0.15)" },
          ".cm-search button": { borderRadius: "6px", border: "0", background: "rgba(0, 0, 0, 0.07)" },
        }),
      ],
    });

    view = new EditorView({ state, parent: editorHost });
    editorHost.addEventListener("paste", handlePaste);
    editorHost.addEventListener("drop", handleDrop);
    editorHost.addEventListener("dragover", handleDragOver);
    editorHost.addEventListener("keydown", updateLinkModifier);
    editorHost.addEventListener("keyup", updateLinkModifier);
    editorHost.addEventListener("mousemove", updateLinkModifier);
    editorHost.addEventListener("mouseleave", clearLinkModifier);
    editorHost.addEventListener("blur", clearLinkModifier, true);
    window.addEventListener("keydown", updateLinkModifier);
    window.addEventListener("keyup", updateLinkModifier);
    window.addEventListener("blur", clearLinkModifier);
    updateHistoryState();
    onready?.({ view });

    if (autofocus) requestAnimationFrame(() => view?.focus());

    return () => {
      editorHost.removeEventListener("paste", handlePaste);
      editorHost.removeEventListener("drop", handleDrop);
      editorHost.removeEventListener("dragover", handleDragOver);
      editorHost.removeEventListener("keydown", updateLinkModifier);
      editorHost.removeEventListener("keyup", updateLinkModifier);
      editorHost.removeEventListener("mousemove", updateLinkModifier);
      editorHost.removeEventListener("mouseleave", clearLinkModifier);
      editorHost.removeEventListener("blur", clearLinkModifier, true);
      window.removeEventListener("keydown", updateLinkModifier);
      window.removeEventListener("keyup", updateLinkModifier);
      window.removeEventListener("blur", clearLinkModifier);
      view?.destroy();
      view = undefined;
    };
  });

  $effect(() => {
    if (!view || value === internalValue) return;
    internalValue = value;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: value },
      selection: EditorSelection.cursor(Math.min(view.state.selection.main.head, value.length)),
    });
  });

  $effect(() => {
    if (mode === "plain" || readonly) sourceMode = false;
    if (view) {
      view.dispatch({ effects: modeCompartment.reconfigure(currentModeExtension(mode, sourceMode, readonly)) });
    }
  });

  $effect(() => {
    if (view) {
      view.dispatch({
        effects: [
          readonlyCompartment.reconfigure(readonlyExtensions(readonly)[0]),
          editableCompartment.reconfigure(readonlyExtensions(readonly)[1]),
        ],
      });
    }
  });

  $effect(() => {
    previewAssetUrls = assetUrls;
    if (view && mode === "typora" && !sourceMode) {
      view.dispatch({ effects: modeCompartment.reconfigure(currentModeExtension(mode, sourceMode, readonly)) });
    }
  });
</script>

<section
  class="note-editor"
  class:is-source={mode === "typora" && sourceMode}
  class:is-typora={mode === "typora" && !sourceMode}
  class:is-plain={mode === "plain"}
  class:is-link-modifier={linkModifierActive && mode === "typora" && !sourceMode}
  aria-busy={importing}
>
  {#if !readonly}
    <div class="toolbar" role="toolbar" aria-label="便签编辑工具栏">
      <button type="button" title="撤回 (Ctrl+Z)" aria-label="撤回" disabled={!canUndo} onclick={runUndo}><Undo2 size={17} /></button>
      <button type="button" title="重做 (Ctrl+Y)" aria-label="重做" disabled={!canRedo} onclick={runRedo}><Redo2 size={17} /></button>
      <span class="separator"></span>
      {#if mode === "typora"}
        <button type="button" title="粗体 (Ctrl+B)" aria-label="粗体" onclick={() => format("bold")}><Bold size={17} /></button>
        <button type="button" title="斜体 (Ctrl+I)" aria-label="斜体" onclick={() => format("italic")}><Italic size={17} /></button>
        <button type="button" title="高亮" aria-label="高亮" onclick={() => format("highlight")}><Highlighter size={17} /></button>
        <button type="button" title="链接 (Ctrl+K)" aria-label="链接" onclick={() => format("link")}><Link size={17} /></button>
        <button type="button" title="插入图片" aria-label="插入图片" disabled={!onasset || importing} onclick={openImagePicker}><ImagePlus size={17} /></button>
        <button type="button" title="插入分割线" aria-label="插入分割线" onclick={insertRule}><Minus size={18} /></button>
        <span class="separator"></span>
      {/if}
      <button type="button" title="查找 (Ctrl+F)" aria-label="查找" onclick={find}><Search size={17} /></button>
      {#if mode === "typora"}
        <button type="button" class:active={sourceMode} title="切换源码模式" aria-label="切换源码模式" aria-pressed={sourceMode} onclick={toggleSourceMode}><Code2 size={17} /></button>
      {/if}
      {#if importing}<span class="importing">正在导入图片…</span>{/if}
    </div>
  {/if}

  <div class="editor-host" bind:this={editorHost}></div>
  <input
    class="file-input"
    bind:this={fileInput}
    type="file"
    accept="image/png,image/jpeg,image/gif,image/webp"
    multiple
    onchange={(event) => {
      const files = event.currentTarget.files;
      if (files) void importFiles(files);
      event.currentTarget.value = "";
    }}
  />
</section>

<style>
  .note-editor { display: flex; min-height: 220px; height: 100%; flex-direction: column; color: var(--note-fg, #25231d); }
  .toolbar { display: flex; min-height: 36px; align-items: center; gap: 2px; padding: 3px 8px; border-bottom: 1px solid var(--note-toolbar-border, color-mix(in srgb, var(--note-fg), transparent 90%)); opacity: var(--note-toolbar-opacity, 0.3); transition: opacity 120ms ease; }
  .note-editor:hover .toolbar, .toolbar:focus-within, .is-source .toolbar { opacity: 1; }
  button { display: inline-grid; width: 29px; height: 29px; place-items: center; padding: 0; border: 0; border-radius: 6px; color: inherit; background: transparent; cursor: pointer; }
  button:hover:not(:disabled) { background: var(--note-control-hover-bg, color-mix(in srgb, var(--note-fg), transparent 88%)); }
  button.active { color: var(--note-control-active-fg, inherit); background: var(--note-control-active-bg, color-mix(in srgb, var(--note-fg), transparent 88%)); }
  button:focus-visible { outline: 2px solid var(--note-focus, rgba(55, 91, 157, 0.6)); outline-offset: 1px; }
  button:disabled { opacity: 0.3; cursor: default; }
  .separator { width: 1px; height: 17px; margin: 0 3px; background: var(--note-separator, color-mix(in srgb, var(--note-fg), transparent 86%)); }
  .importing { margin-left: auto; font-size: 12px; opacity: 0.65; }
  .editor-host { min-height: 0; flex: 1; overflow: hidden; }
  .file-input { display: none; }

  :global(.cm-typora-hidden-mark) { display: inline-block; width: 0; height: 0; overflow: hidden; }
  :global(.cm-typora-source-mark) { color: var(--note-muted, rgba(37, 35, 29, 0.58)); font-weight: 500; }
  :global(.cm-typora-heading) { font-weight: 700; line-height: 1.28; }
  :global(.cm-typora-heading.cm-typora-h1) { padding-top: 0.28em !important; padding-bottom: 0.1em !important; font-size: 1.55em; }
  :global(.cm-typora-heading.cm-typora-h2) { padding-top: 0.24em !important; padding-bottom: 0.08em !important; font-size: 1.32em; }
  :global(.cm-typora-heading.cm-typora-h3) { padding-top: 0.18em !important; padding-bottom: 0.05em !important; font-size: 1.15em; }
  :global(.cm-typora-heading.cm-typora-h4) { font-size: 1.06em; }
  :global(.cm-typora-heading.cm-typora-h5), :global(.cm-typora-heading.cm-typora-h6) { font-size: 1em; }
  :global(.cm-typora-strong) { font-weight: 700; }
  :global(.cm-typora-emphasis) { font-style: italic; }
  :global(.cm-typora-highlight) { border-radius: 2px; background: color-mix(in srgb, #ffd43b 72%, transparent); }
  :global(.cm-typora-strikethrough) { text-decoration: line-through; opacity: 0.78; }
  :global(.cm-typora-inline-code) { border-radius: 4px; padding: 0.08em 0.25em; background: var(--note-code-bg, color-mix(in srgb, var(--note-fg), transparent 91%)); font-family: "Cascadia Code", Consolas, monospace; font-size: 0.9em; }
  :global(.cm-typora-link) { color: var(--note-link, #315d99); text-decoration: underline; text-decoration-thickness: 1px; text-underline-offset: 2px; }
  .is-link-modifier :global(.cm-typora-link[data-link-url]) { cursor: pointer; }
  :global(.cm-typora-list-line) { line-height: 1.55; }
  :global(.md-typora-list-marker) { display: inline-block; min-width: 0.95em; color: color-mix(in srgb, var(--note-fg), transparent 18%); font-weight: 650; text-align: center; }
  :global(.md-typora-task-marker) { display: inline-block; min-width: 1.15em; color: var(--note-link, #44689e); font-family: "Segoe UI Symbol", sans-serif; text-align: center; }
  :global(.cm-typora-quote-line) { border-left: 3px solid color-mix(in srgb, var(--note-fg), transparent 74%); padding-left: 0.72em !important; color: var(--note-muted, rgba(37, 35, 29, 0.72)); }
  :global(.cm-typora-code-line) { padding-right: 0.65em !important; padding-left: 0.75em !important; background: var(--note-code-bg, color-mix(in srgb, var(--note-fg), transparent 92%)); font-family: "Cascadia Code", Consolas, monospace; font-size: 0.9em; }
  :global(.cm-typora-code-first) { border-radius: 7px 7px 0 0; padding-top: 0.35em !important; }
  :global(.cm-typora-code-last) { border-radius: 0 0 7px 7px; padding-bottom: 0.35em !important; }
  :global(.cm-typora-code-info) { color: var(--note-muted, rgba(37, 35, 29, 0.58)); font-family: inherit; font-size: 0.82em; }
  :global(.cm-typora-collapsed-gap) { min-height: 0 !important; height: 0.35em; overflow: hidden; line-height: 0.35em !important; }
  :global(.md-typora-horizontal-rule) { display: inline-block; width: 100%; border-top: 1px solid var(--note-rule, color-mix(in srgb, var(--note-fg), transparent 74%)); vertical-align: middle; }
  :global(.md-typora-image-widget) { display: inline-block; box-sizing: border-box; width: 100%; padding: 0.35em 0; vertical-align: top; }
  :global(.md-typora-image-widget img) { display: block; max-width: min(100%, 720px); max-height: 60vh; margin: 0 auto; border-radius: 8px; object-fit: contain; }
  :global(.md-typora-image-widget .md-image-placeholder) { display: inline-flex; align-items: center; max-width: 100%; border: 1px dashed var(--note-rule, color-mix(in srgb, var(--note-fg), transparent 70%)); border-radius: 7px; padding: 0.45em 0.65em; color: var(--note-muted, rgba(37, 35, 29, 0.62)); font-size: 0.9em; }
  :global(.md-typora-table-widget) { box-sizing: border-box; width: 100%; overflow-x: auto; padding: 0.35em 0; cursor: text; }
  :global(.md-typora-table-widget table) { width: 100%; min-width: max-content; border-spacing: 0; border-collapse: separate; border: 1px solid var(--note-rule, color-mix(in srgb, var(--note-fg), transparent 76%)); border-radius: 6px; overflow: hidden; font-size: 0.94em; line-height: 1.45; }
  :global(.md-typora-table-widget th), :global(.md-typora-table-widget td) { min-width: 5.5em; padding: 0.48em 0.65em; border-right: 1px solid var(--note-rule, color-mix(in srgb, var(--note-fg), transparent 82%)); border-bottom: 1px solid var(--note-rule, color-mix(in srgb, var(--note-fg), transparent 82%)); text-align: left; vertical-align: top; }
  :global(.md-typora-table-widget th) { background: var(--note-code-bg, color-mix(in srgb, var(--note-fg), transparent 91%)); font-weight: 700; }
  :global(.md-typora-table-widget tbody tr:nth-child(even)) { background: color-mix(in srgb, var(--note-fg), transparent 96%); }
  :global(.md-typora-table-widget tr > :last-child) { border-right: 0; }
  :global(.md-typora-table-widget tbody tr:last-child > td) { border-bottom: 0; }
  :global(.md-typora-table-widget .md-align-center) { text-align: center; }
  :global(.md-typora-table-widget .md-align-right) { text-align: right; }
</style>

<script lang="ts">
  import type { Snippet } from "svelte";
  import type { EditorMode } from "$lib/editor";
  import type { ToolName } from "$lib/tools";
  import NoteTitlebar from "./NoteTitlebar.svelte";
  import type { NoteColor } from "./types";

  interface Props {
    title?: string;
    color: NoteColor;
    pinned?: boolean;
    readonly?: boolean;
    editorMode?: EditorMode;
    screenshotShortcut?: string;
    saving?: boolean;
    saveError?: string | null;
    children: Snippet;
    onnew?: () => void;
    ontitlechange?: (title: string) => void;
    oncolorchange?: (color: NoteColor) => void;
    oneditormodechange?: (mode: EditorMode) => void;
    onreadonlychange?: (readonly: boolean) => void;
    ontogglepin?: (pinned: boolean) => void;
    ontoolopen?: (tool: ToolName) => void | Promise<void>;
    ondelete?: () => void;
    onclose?: () => void;
  }

  let {
    title,
    color,
    pinned = false,
    readonly = false,
    editorMode = "typora",
    screenshotShortcut = "F1",
    saving = false,
    saveError = null,
    children,
    onnew,
    ontitlechange,
    oncolorchange,
    oneditormodechange,
    onreadonlychange,
    ontogglepin,
    ontoolopen,
    ondelete,
    onclose,
  }: Props = $props();
</script>

<section class="note-shell" data-note-color={color} aria-label={title || "便签"}>
  <NoteTitlebar
    {title}
    {color}
    {pinned}
    {readonly}
    {editorMode}
    {screenshotShortcut}
    {onnew}
    {ontitlechange}
    {oncolorchange}
    {oneditormodechange}
    {onreadonlychange}
    {ontogglepin}
    {ontoolopen}
    {ondelete}
    {onclose}
  />
  <main class="note-content">
    {@render children()}
  </main>
  {#if saving || saveError}
    <div class:error={Boolean(saveError)} class="save-status" role="status" aria-live="polite">
      {saveError ? `保存失败：${saveError}` : "正在保存…"}
    </div>
  {/if}
</section>

<style>
  .note-shell {
    position: relative;
    display: grid;
    width: 100%;
    height: 100%;
    min-width: 220px;
    min-height: 180px;
    overflow: hidden;
    grid-template-rows: 42px minmax(0, 1fr);
    color: var(--note-fg);
    background: var(--note-bg);
    container-name: note-shell;
    container-type: inline-size;
  }

  .note-content {
    min-width: 0;
    min-height: 0;
    overflow: auto;
    background: var(--note-bg);
  }

  .save-status {
    position: absolute;
    z-index: 15;
    right: 8px;
    bottom: 7px;
    max-width: calc(100% - 16px);
    padding: 3px 7px;
    overflow: hidden;
    color: var(--note-muted);
    font-size: 10.5px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
    pointer-events: none;
    background: var(--note-status-bg, color-mix(in srgb, var(--note-bg), #ffffff 34%));
    border-radius: 3px;
  }

  .save-status.error {
    color: #8c1d14;
    background: #fff0ed;
  }
</style>

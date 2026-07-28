<script lang="ts">
  import { tick } from "svelte";
  import {
    FilePenLine,
    FileText,
    Lock,
    LockOpen,
    Palette,
    Pin,
    Plus,
    ScanLine,
    Trash2,
    Bell,
    ChartGantt,
    Timer,
    Wrench,
    X,
  } from "@lucide/svelte";
  import ColorPicker from "./ColorPicker.svelte";
  import type { EditorMode } from "$lib/editor";
  import type { ToolName } from "$lib/tools";
  import type { NoteColor } from "./types";

  interface Props {
    title?: string;
    color: NoteColor;
    pinned?: boolean;
    readonly?: boolean;
    editorMode?: EditorMode;
    screenshotShortcut?: string;
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
    title = "无标题便签",
    color,
    pinned = false,
    readonly = false,
    editorMode = "typora",
    screenshotShortcut = "F1",
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

  let colorOpen = $state(false);
  let colorControl = $state<HTMLDivElement>();
  let toolsOpen = $state(false);
  let toolsControl = $state<HTMLDivElement>();
  let editingTitle = $state(false);
  let titleDraft = $state("无标题便签");
  let lastTitle = $state<string | null>(null);
  let titleInput = $state<HTMLInputElement>();

  function editorModeLabel(mode: EditorMode): string {
    return mode === "plain" ? "纯文本" : "Markdown";
  }

  $effect(() => {
    if (title !== lastTitle) {
      lastTitle = title;
      if (!editingTitle) titleDraft = title;
    }
  });

  $effect(() => {
    if (readonly && editingTitle) {
      titleDraft = title;
      editingTitle = false;
    }
  });

  async function startTitleEdit() {
    if (readonly || !ontitlechange || editingTitle) return;
    editingTitle = true;
    await tick();
    titleInput?.focus();
    titleInput?.select();
  }

  function commitTitle() {
    if (!editingTitle) return;
    const nextTitle = titleDraft.trim() || "无标题便签";
    titleDraft = nextTitle;
    editingTitle = false;
    if (nextTitle !== title) ontitlechange?.(nextTitle);
  }

  function cancelTitleEdit() {
    titleDraft = title;
    editingTitle = false;
  }

  function handleTitleKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      commitTitle();
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      cancelTitleEdit();
    }
  }

  function handleColorChange(nextColor: NoteColor) {
    oncolorchange?.(nextColor);
    colorOpen = false;
  }

  function handleWindowKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      if (colorOpen) colorOpen = false;
      if (toolsOpen) toolsOpen = false;
    }
  }

  function handleWindowClick(event: MouseEvent) {
    if (colorOpen && !colorControl?.contains(event.target as Node)) {
      colorOpen = false;
    }
    if (toolsOpen && !toolsControl?.contains(event.target as Node)) {
      toolsOpen = false;
    }
  }

  async function openTool(tool: ToolName): Promise<void> {
    toolsOpen = false;
    await ontoolopen?.(tool);
  }
</script>

<svelte:window onclick={handleWindowClick} onkeydown={handleWindowKeydown} />

<header class="note-titlebar" data-tauri-drag-region>
  <div class="leading-actions">
    {#if onnew}
      <button
        type="button"
        class="icon-button"
        aria-label="新建便签"
        data-tooltip="新建便签"
        data-tooltip-placement="bottom"
        data-tooltip-align="start"
        onclick={onnew}
      >
        <Plus size={18} strokeWidth={2.2} aria-hidden="true" />
      </button>
    {/if}
  </div>

  <div class="title-area">
    {#if editingTitle}
      <input
        bind:this={titleInput}
        class="title-input"
        aria-label="便签标题"
        value={titleDraft}
        maxlength="200"
        onpointerdown={(event) => event.stopPropagation()}
        oninput={(event) => (titleDraft = event.currentTarget.value)}
        onkeydown={handleTitleKeydown}
        onblur={commitTitle}
      />
    {:else if ontitlechange && !readonly}
      <button
        type="button"
        class="window-title editable"
        aria-label={`编辑标题：${titleDraft}`}
        title="点击编辑标题"
        onpointerdown={(event) => event.stopPropagation()}
        onclick={(event) => {
          event.stopPropagation();
          void startTitleEdit();
        }}
      >{titleDraft}</button>
    {:else}
      <div class="window-title" data-tauri-drag-region title={titleDraft}>{titleDraft}</div>
    {/if}
  </div>

  <div class="titlebar-actions" aria-label="便签工具">
    {#if oneditormodechange}
      <button
        type="button"
        class="icon-button editor-mode-control"
        aria-label={`切换编辑样式（当前：${editorModeLabel(editorMode)}）`}
        aria-pressed={editorMode === "plain"}
        data-tooltip={`当前编辑样式：${editorModeLabel(editorMode)}`}
        data-tooltip-placement="bottom"
        onclick={() => oneditormodechange?.(editorMode === "plain" ? "typora" : "plain")}
      >
        {#if editorMode === "plain"}
          <FileText size={16} aria-hidden="true" />
        {:else}
          <FilePenLine size={16} aria-hidden="true" />
        {/if}
      </button>
    {/if}

    {#if onreadonlychange}
      <button
        type="button"
        class="icon-button"
        aria-label={readonly ? "退出只读模式" : "进入只读模式"}
        aria-pressed={readonly}
        data-tooltip={readonly ? "退出只读模式" : "只读模式"}
        data-tooltip-placement="bottom"
        onclick={() => onreadonlychange?.(!readonly)}
      >
        {#if readonly}
          <Lock size={16} aria-hidden="true" />
        {:else}
          <LockOpen size={16} aria-hidden="true" />
        {/if}
      </button>
    {/if}

    {#if oncolorchange}
      <div class="color-control" bind:this={colorControl}>
        <button
          type="button"
          class="icon-button"
          aria-label="更改背景色"
          aria-expanded={colorOpen}
          aria-haspopup="dialog"
          data-tooltip="背景色"
          data-tooltip-placement="bottom"
          onclick={() => {
            toolsOpen = false;
            colorOpen = !colorOpen;
          }}
        >
          <Palette size={17} aria-hidden="true" />
        </button>
        {#if colorOpen}
          <div class="color-popover" role="dialog" aria-label="更改背景色">
            <ColorPicker value={color} onchange={handleColorChange} />
          </div>
        {/if}
      </div>
    {/if}

    {#if ontogglepin}
      <button
        type="button"
        class="icon-button"
        aria-label={pinned ? "取消置顶" : "置顶"}
        aria-pressed={pinned}
        data-tooltip={pinned ? "取消置顶" : "置顶"}
        data-tooltip-placement="bottom"
        onclick={() => ontogglepin?.(!pinned)}
      >
        <Pin size={16} fill={pinned ? "currentColor" : "none"} aria-hidden="true" />
      </button>
    {/if}

    {#if ondelete}
      <button
        type="button"
        class="icon-button delete-button"
        aria-label="删除便签"
        data-tooltip="删除"
        data-tooltip-placement="bottom"
        onclick={ondelete}
      >
        <Trash2 size={16} aria-hidden="true" />
      </button>
    {/if}

    {#if ontoolopen}
      <div class="tools-control" bind:this={toolsControl}>
        <button
          type="button"
          class="icon-button"
          aria-label="小工具"
          aria-expanded={toolsOpen}
          aria-haspopup="menu"
          data-tooltip="小工具"
          data-tooltip-placement="bottom"
          onclick={() => {
            colorOpen = false;
            toolsOpen = !toolsOpen;
          }}
        >
          <Wrench size={16} aria-hidden="true" />
        </button>
        {#if toolsOpen}
          <div class="tools-menu" role="menu" aria-label="小工具">
            <button type="button" role="menuitem" onclick={() => void openTool("timer")}>
              <Timer size={15} aria-hidden="true" />
              <span>计时器</span>
            </button>
            <button type="button" role="menuitem" onclick={() => void openTool("reminder")}>
              <Bell size={15} aria-hidden="true" />
              <span>提醒</span>
            </button>
            <button type="button" role="menuitem" onclick={() => void openTool("gantt")}>
              <ChartGantt size={15} aria-hidden="true" />
              <span>任务甘特图</span>
            </button>
            <button type="button" role="menuitem" onclick={() => void openTool("screenshot")}>
              <ScanLine size={15} aria-hidden="true" />
              <span>截图({screenshotShortcut})</span>
            </button>
          </div>
        {/if}
      </div>
    {/if}

    {#if onclose}
      <button
        type="button"
        class="icon-button close-button"
        aria-label="关闭窗口"
        data-tooltip="关闭"
        data-tooltip-placement="bottom"
        onclick={onclose}
      >
        <X size={18} aria-hidden="true" />
      </button>
    {/if}
  </div>
</header>

<style>
  .note-titlebar {
    position: relative;
    z-index: 20;
    display: grid;
    height: 42px;
    min-width: 0;
    align-items: center;
    grid-template-columns: auto minmax(0, 1fr) auto;
    padding: 4px 5px;
    color: var(--note-fg);
    background: var(--note-bg-strong);
    border-bottom: 1px solid rgb(0 0 0 / 10%);
    user-select: none;
  }

  .leading-actions,
  .titlebar-actions {
    display: flex;
    min-width: 0;
    align-items: center;
  }

  .leading-actions:empty {
    width: 4px;
  }

  .title-area {
    width: 80%;
    min-width: 0;
    padding: 0 5px;
    justify-self: start;
  }

  .window-title,
  .title-input {
    width: 100%;
    min-width: 0;
    height: 30px;
    padding: 0 5px;
    overflow: hidden;
    color: var(--note-fg);
    font-size: 13px;
    font-weight: 700;
    line-height: 30px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button.window-title {
    display: block;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 3px;
    cursor: text;
  }

  button.window-title:hover {
    background: var(--note-title-hover-bg, rgb(255 255 255 / 24%));
    border-color: rgb(0 0 0 / 8%);
  }

  .title-input {
    display: block;
    background: var(--note-title-input-bg, color-mix(in srgb, var(--note-bg), #ffffff 30%));
    border: 1px solid var(--note-title-input-border, color-mix(in srgb, var(--note-border), #000000 10%));
    border-radius: 3px;
    outline: 0;
  }

  .title-input:focus {
    border-color: var(--note-focus, var(--app-focus, #005fb8));
    box-shadow: inset 0 -2px var(--note-focus, var(--app-focus, #005fb8));
  }

  .titlebar-actions {
    flex: 0 0 auto;
    gap: 1px;
  }

  .note-titlebar :global(.icon-button) {
    flex-basis: 30px;
    width: 30px;
    height: 30px;
    color: var(--note-fg);
  }

  .note-titlebar :global(.icon-button:hover) {
    background: var(--note-control-hover-bg, rgb(0 0 0 / 7%));
  }

  .note-titlebar :global(.icon-button:active),
  .note-titlebar :global(.icon-button[aria-pressed="true"]) {
    color: var(--note-control-active-fg, var(--app-accent, #0067c0));
    background: var(--note-control-active-bg, rgb(0 103 192 / 11%));
  }

  .note-titlebar :global(.icon-button:focus-visible),
  button.window-title:focus-visible {
    outline-color: var(--note-focus, var(--app-focus, #005fb8));
  }

  .color-control {
    position: relative;
  }

  .color-popover {
    position: fixed;
    z-index: 200;
    top: 43px;
    right: 8px;
    width: max-content;
    max-width: calc(100vw - 16px);
    padding: 12px;
    color: var(--app-fg, #202020);
    background: var(--app-surface, #ffffff);
    border: 1px solid var(--app-border, #d8d8d8);
    border-radius: 6px;
    box-shadow: var(--shadow-flyout, 0 8px 24px rgb(0 0 0 / 16%));
  }

  .tools-control {
    position: relative;
  }

  .tools-menu {
    position: fixed;
    z-index: 210;
    top: 43px;
    right: 8px;
    display: grid;
    width: min(180px, calc(100vw - 16px));
    padding: 4px;
    color: var(--app-fg, #202020);
    background: var(--app-surface, #ffffff);
    border: 1px solid var(--app-border, #d8d8d8);
    border-radius: 6px;
    box-shadow: var(--shadow-flyout, 0 8px 24px rgb(0 0 0 / 16%));
  }

  .tools-menu button {
    display: flex;
    width: 100%;
    min-height: 30px;
    padding: 5px 8px;
    align-items: center;
    gap: 8px;
    color: inherit;
    font-size: 12.5px;
    text-align: left;
    background: transparent;
    border: 0;
    border-radius: 4px;
    cursor: default;
  }

  .tools-menu button:hover,
  .tools-menu button:focus-visible {
    background: var(--app-surface-hover, #f5f5f5);
    outline: 0;
  }

  .delete-button:hover {
    color: var(--note-danger, var(--app-danger, #c42b1c)) !important;
  }

  .close-button:hover {
    color: #ffffff !important;
    background: #c42b1c !important;
  }

  @container note-shell (max-width: 380px) {
    .window-title {
      padding-inline: 3px;
      font-size: 11px;
    }

  }

  @container note-shell (max-width: 280px) {
    .title-area {
      width: 0;
      padding: 0;
    }
  }

</style>

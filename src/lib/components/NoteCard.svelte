<script lang="ts">
  import { ExternalLink, GripVertical, Pin, Trash2 } from "@lucide/svelte";
  import type { NoteListItem } from "./types";

  interface Props {
    note: NoteListItem;
    selected?: boolean;
    reorderable?: boolean;
    dragging?: boolean;
    onselect?: (id: string) => void;
    onopen?: (id: string) => void;
    onreorderpointerdown?: (id: string, event: PointerEvent) => void;
    onreorderkeydown?: (id: string, event: KeyboardEvent) => void;
    ontogglepin?: (id: string, pinned: boolean) => void;
    ondelete?: (id: string) => void;
  }

  let {
    note,
    selected = false,
    reorderable = false,
    dragging = false,
    onselect,
    onopen,
    onreorderpointerdown,
    onreorderkeydown,
    ontogglepin,
    ondelete,
  }: Props = $props();

  let summary = $derived(note.preview ?? note.excerpt ?? "");

  const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

  function formatUpdatedAt(value: string): string {
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : dateFormatter.format(date);
  }

  function stopCardClick(event: MouseEvent): void {
    event.stopPropagation();
  }
</script>

<article
  class:selected
  class:reorderable
  class:dragging
  class="note-card"
  data-note-id={note.id}
  data-note-color={note.color}
  aria-label={note.title || "无标题便签"}
>
  {#if reorderable}
    <button
      type="button"
      class="reorder-handle"
      aria-label={`调整“${note.title || "无标题便签"}”的顺序`}
      aria-pressed={dragging}
      data-tooltip="拖动排序；方向键移动"
      onpointerdown={(event) => onreorderpointerdown?.(note.id, event)}
      onkeydown={(event) => onreorderkeydown?.(note.id, event)}
      onclick={stopCardClick}
      ondblclick={stopCardClick}
    >
      <GripVertical size={17} strokeWidth={2.1} aria-hidden="true" />
    </button>
  {/if}

  <button
    type="button"
    class="note-content"
    aria-current={selected ? "true" : undefined}
    onclick={() => onselect?.(note.id)}
    ondblclick={() => onopen?.(note.id)}
  >
    <span class="title-row">
      <strong>{note.title || "无标题便签"}</strong>
      {#if note.pinned}
        <Pin class="pin-mark" size={13} fill="currentColor" aria-label="已置顶" />
      {/if}
    </span>
    {#if summary}
      <span class="preview">{summary}</span>
    {/if}
    <time datetime={note.updatedAt}>{formatUpdatedAt(note.updatedAt)}</time>
  </button>

  <div class="card-actions" aria-label="便签操作">
    {#if onopen}
      <button
        type="button"
        class="icon-button"
        aria-label="在独立窗口打开"
        data-tooltip="独立窗口"
        onclick={() => onopen?.(note.id)}
      >
        <ExternalLink size={15} aria-hidden="true" />
      </button>
    {/if}
    {#if ontogglepin}
      <button
        type="button"
        class="icon-button"
        aria-label={note.pinned ? "取消置顶" : "置顶"}
        aria-pressed={note.pinned}
        data-tooltip={note.pinned ? "取消置顶" : "置顶"}
        onclick={() => ontogglepin?.(note.id, !note.pinned)}
      >
        <Pin size={15} fill={note.pinned ? "currentColor" : "none"} aria-hidden="true" />
      </button>
    {/if}
    {#if ondelete}
      <button
        type="button"
        class="icon-button delete-button"
        aria-label="删除"
        data-tooltip="删除"
        onclick={() => ondelete?.(note.id)}
      >
        <Trash2 size={15} aria-hidden="true" />
      </button>
    {/if}
  </div>
</article>

<style>
  .note-card {
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    min-height: 104px;
    overflow: visible;
    color: var(--note-fg);
    background: var(--note-bg);
    border: 1px solid var(--note-border);
    border-radius: 5px;
    box-shadow: 0 1px 2px rgb(0 0 0 / 6%);
  }

  .note-card:hover {
    border-color: color-mix(in srgb, var(--note-border), #202020 22%);
    box-shadow: 0 2px 7px rgb(0 0 0 / 10%);
  }

  .note-card.selected {
    border-color: var(--note-focus, var(--app-focus, #005fb8));
    box-shadow: 0 0 0 1px var(--note-focus, var(--app-focus, #005fb8));
  }

  .note-card.dragging {
    z-index: 2;
    opacity: 0.78;
    box-shadow: 0 7px 18px rgb(0 0 0 / 18%);
    transform: scale(0.985);
  }

  .note-content {
    display: flex;
    min-width: 0;
    padding: 13px 8px 12px 13px;
    align-items: stretch;
    flex-direction: column;
    color: inherit;
    text-align: left;
    background: transparent;
    border: 0;
    border-radius: 4px 0 0 4px;
    cursor: default;
  }

  .note-card.reorderable .note-content {
    padding-left: 31px;
  }

  .reorder-handle {
    position: absolute;
    z-index: 3;
    top: 8px;
    left: 5px;
    display: grid;
    width: 24px;
    height: 28px;
    padding: 0;
    place-items: center;
    color: var(--note-muted);
    touch-action: none;
    background: transparent;
    border: 0;
    border-radius: 4px;
    cursor: grab;
    opacity: 0.58;
  }

  .reorder-handle:hover,
  .reorder-handle:focus-visible,
  .note-card.dragging .reorder-handle {
    color: var(--note-fg);
    background: var(--note-control-hover-bg, rgb(0 0 0 / 7%));
    opacity: 1;
  }

  .reorder-handle:focus-visible {
    outline: 2px solid var(--note-focus, var(--app-focus, #005fb8));
    outline-offset: 1px;
  }

  .reorder-handle:active {
    cursor: grabbing;
  }

  .note-content:focus-visible {
    outline-color: var(--note-focus, var(--app-focus, #005fb8));
    outline-offset: -2px;
  }

  .title-row {
    display: flex;
    min-width: 0;
    align-items: center;
    gap: 6px;
  }

  strong {
    min-width: 0;
    overflow: hidden;
    font-size: 14px;
    font-weight: 650;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  :global(.pin-mark) {
    flex: 0 0 auto;
    color: var(--note-muted);
  }

  .preview {
    display: -webkit-box;
    min-width: 0;
    margin-top: 6px;
    overflow: hidden;
    color: var(--note-muted);
    font-size: 12.5px;
    line-height: 1.4;
    overflow-wrap: anywhere;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  time {
    margin-top: auto;
    padding-top: 9px;
    color: var(--note-muted);
    font-size: 11.5px;
    line-height: 1;
  }

  .card-actions {
    display: flex;
    width: 33px;
    padding: 6px 4px 6px 0;
    align-items: center;
    flex-direction: column;
    gap: 1px;
    opacity: 0;
    transition: opacity 100ms ease;
  }

  .note-card:hover .card-actions,
  .note-card:focus-within .card-actions,
  .note-card.selected .card-actions {
    opacity: 1;
  }

  .card-actions :global(.icon-button) {
    flex-basis: 28px;
    width: 28px;
    height: 28px;
    color: var(--note-muted);
  }

  .card-actions :global(.icon-button:hover) {
    background: var(--note-control-hover-bg, rgb(0 0 0 / 7%));
  }

  .card-actions :global(.icon-button[aria-pressed="true"]) {
    color: var(--note-control-active-fg, var(--app-accent, #0067c0));
    background: var(--note-control-active-bg, rgb(0 103 192 / 11%));
  }

  .card-actions :global(.icon-button:focus-visible) {
    outline-color: var(--note-focus, var(--app-focus, #005fb8));
  }

  .card-actions :global(.delete-button:hover) {
    color: var(--note-danger, var(--app-danger, #c42b1c));
  }

  @media (hover: none) {
    .card-actions {
      opacity: 1;
    }
  }

  @media (max-width: 320px) {
    .note-card {
      min-height: 94px;
    }

    .preview {
      -webkit-line-clamp: 1;
      line-clamp: 1;
    }
  }
</style>

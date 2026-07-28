<script lang="ts">
  import { ArrowLeft, RotateCcw, Trash2, X } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import type { TrashListItem } from "./types";

  interface Props {
    notes: TrashListItem[];
    loading?: boolean;
    onback?: () => void;
    onrestore?: (id: string) => void;
    onempty?: () => void | Promise<void>;
  }

  let {
    notes,
    loading = false,
    onback,
    onrestore,
    onempty,
  }: Props = $props();

  let confirmOpen = $state(false);
  let emptying = $state(false);

  const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

  function formatDeletedAt(value: string): string {
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : dateFormatter.format(date);
  }

  async function emptyTrash() {
    if (!onempty || emptying) return;
    emptying = true;
    try {
      await onempty();
      confirmOpen = false;
    } finally {
      emptying = false;
    }
  }
</script>

<svelte:window
  onkeydown={(event) => {
    if (event.key === "Escape" && !emptying) confirmOpen = false;
  }}
/>

<section class="trash-view" aria-label="回收站">
  <header>
    <div class="title-row">
      {#if onback}
        <button
          type="button"
          class="icon-button"
          aria-label="返回便签列表"
          data-tooltip="返回"
          data-tooltip-placement="bottom"
          onclick={onback}
        >
          <ArrowLeft size={19} aria-hidden="true" />
        </button>
      {/if}
      <h1>回收站</h1>
      <span class="item-count">{notes.length}</span>
    </div>
    {#if notes.length > 0 && onempty}
      <button type="button" class="empty-button" onclick={() => (confirmOpen = true)}>
        <Trash2 size={15} aria-hidden="true" />
        <span>清空</span>
      </button>
    {/if}
  </header>

  <div class="trash-body">
    {#if loading}
      <div class="loading" role="status" aria-busy="true">正在加载…</div>
    {:else if notes.length === 0}
      <EmptyState variant="trash" />
    {:else}
      <ul>
        {#each notes as note (note.id)}
          <li>
            <span class="color-marker" data-note-color={note.color} aria-hidden="true"></span>
            <div class="note-summary">
              <strong>{note.title || "无标题便签"}</strong>
              {#if note.preview ?? note.excerpt}
                <p>{note.preview ?? note.excerpt}</p>
              {/if}
              <time datetime={note.deletedAt}>{formatDeletedAt(note.deletedAt)} 删除</time>
            </div>
            {#if onrestore}
              <button
                type="button"
                class="icon-button restore-button"
                aria-label={`恢复「${note.title || "无标题便签"}」`}
                data-tooltip="恢复"
                onclick={() => onrestore?.(note.id)}
              >
                <RotateCcw size={17} aria-hidden="true" />
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</section>

{#if confirmOpen}
  <div class="dialog-backdrop">
    <button
      type="button"
      class="backdrop-dismiss"
      aria-label="关闭清空回收站确认框"
      disabled={emptying}
      onclick={() => (confirmOpen = false)}
    ></button>
    <div
      class="confirm-dialog"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="empty-trash-title"
      aria-describedby="empty-trash-detail"
    >
      <div class="dialog-heading">
        <h2 id="empty-trash-title">永久删除所有便签？</h2>
        <button
          type="button"
          class="icon-button"
          aria-label="关闭"
          disabled={emptying}
          onclick={() => (confirmOpen = false)}
        >
          <X size={17} aria-hidden="true" />
        </button>
      </div>
      <p id="empty-trash-detail">回收站中的 {notes.length} 条便签将无法恢复。</p>
      <div class="dialog-actions">
        <button type="button" class="secondary-button" disabled={emptying} onclick={() => (confirmOpen = false)}>
          取消
        </button>
        <button type="button" class="danger-button" disabled={emptying} onclick={emptyTrash}>
          {emptying ? "正在清空…" : "清空回收站"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .trash-view {
    display: grid;
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    grid-template-rows: auto minmax(0, 1fr);
    color: var(--app-fg, #202020);
    background: var(--app-bg, #f3f3f3);
  }

  header {
    display: flex;
    min-height: 62px;
    padding: 13px 18px;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--app-border, #d8d8d8);
  }

  .title-row,
  .dialog-heading,
  .dialog-actions {
    display: flex;
    min-width: 0;
    align-items: center;
  }

  .title-row {
    gap: 7px;
  }

  h1,
  h2,
  p {
    margin: 0;
  }

  h1 {
    font-size: 20px;
    font-weight: 650;
    line-height: 1.3;
  }

  .item-count {
    min-width: 22px;
    height: 20px;
    padding: 0 6px;
    color: var(--app-muted, #686868);
    font-size: 11px;
    line-height: 20px;
    text-align: center;
    background: #e2e2e2;
    border-radius: 10px;
  }

  .empty-button,
  .secondary-button,
  .danger-button {
    display: inline-flex;
    min-height: 32px;
    padding: 5px 11px;
    align-items: center;
    justify-content: center;
    gap: 6px;
    font-size: 12.5px;
    font-weight: 600;
    border-radius: 4px;
    cursor: default;
  }

  .empty-button {
    color: var(--app-danger, #c42b1c);
    background: transparent;
    border: 1px solid transparent;
  }

  .empty-button:hover {
    background: #f9e8e6;
  }

  .trash-body {
    min-height: 0;
    overflow: auto;
  }

  ul {
    max-width: 840px;
    padding: 8px 18px 24px;
    margin: 0 auto;
    list-style: none;
  }

  li {
    display: grid;
    min-height: 82px;
    align-items: center;
    grid-template-columns: 7px minmax(0, 1fr) 34px;
    gap: 12px;
    border-bottom: 1px solid var(--app-border, #d8d8d8);
  }

  .color-marker {
    width: 7px;
    height: 48px;
    background: var(--note-bg-strong);
    border: 1px solid var(--note-border);
    border-radius: 3px;
  }

  .note-summary {
    min-width: 0;
    padding: 11px 0;
  }

  strong,
  .note-summary p {
    display: block;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    font-size: 13.5px;
    font-weight: 650;
  }

  .note-summary p {
    margin-top: 4px;
    color: var(--app-muted, #686868);
    font-size: 12px;
  }

  time {
    display: block;
    margin-top: 5px;
    color: #777777;
    font-size: 10.5px;
  }

  .restore-button {
    color: var(--app-accent, #0067c0);
  }

  .loading {
    padding: 48px 24px;
    color: var(--app-muted, #686868);
    font-size: 13px;
    text-align: center;
  }

  .dialog-backdrop {
    position: fixed;
    z-index: 1000;
    inset: 0;
    display: grid;
    padding: 18px;
    place-items: center;
    background: rgb(0 0 0 / 28%);
  }

  .backdrop-dismiss {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    padding: 0;
    background: transparent;
    border: 0;
  }

  .confirm-dialog {
    position: relative;
    width: min(100%, 390px);
    padding: 18px;
    color: var(--app-fg, #202020);
    background: var(--app-surface, #ffffff);
    border: 1px solid var(--app-border, #d8d8d8);
    border-radius: 7px;
    box-shadow: var(--shadow-flyout, 0 8px 24px rgb(0 0 0 / 16%));
  }

  .dialog-heading {
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  h2 {
    padding-top: 4px;
    font-size: 16px;
    font-weight: 650;
    line-height: 1.35;
  }

  .confirm-dialog > p {
    margin-top: 10px;
    color: var(--app-muted, #686868);
    font-size: 13px;
    line-height: 1.5;
  }

  .dialog-actions {
    margin-top: 20px;
    justify-content: flex-end;
    gap: 8px;
  }

  .secondary-button {
    color: var(--app-fg, #202020);
    background: #ffffff;
    border: 1px solid var(--app-border-strong, #b9b9b9);
  }

  .danger-button {
    color: #ffffff;
    background: var(--app-danger, #c42b1c);
    border: 1px solid #a7281b;
  }

  .secondary-button:hover {
    background: #f5f5f5;
  }

  .danger-button:hover {
    background: #ab2619;
  }

  .secondary-button:disabled,
  .danger-button:disabled {
    cursor: not-allowed;
    opacity: 0.62;
  }

  @media (max-width: 420px) {
    header {
      padding-inline: 12px;
    }

    ul {
      padding-inline: 12px;
    }

    .empty-button span {
      display: none;
    }

    .empty-button {
      width: 32px;
      padding: 0;
    }

    .dialog-actions {
      align-items: stretch;
      flex-direction: column-reverse;
    }

    .dialog-actions button {
      width: 100%;
    }
  }
</style>

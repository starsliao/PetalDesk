<script lang="ts">
  import { SearchX, StickyNote, Trash2 } from "@lucide/svelte";

  type EmptyVariant = "notes" | "search" | "trash";

  interface Props {
    variant?: EmptyVariant;
    title?: string;
    detail?: string;
    actionLabel?: string;
    onaction?: () => void;
    compact?: boolean;
  }

  let {
    variant = "notes",
    title,
    detail,
    actionLabel,
    onaction,
    compact = false,
  }: Props = $props();

  const defaults: Record<EmptyVariant, { title: string; detail: string }> = {
    notes: { title: "还没有便签", detail: "新建一条，记下此刻的想法。" },
    search: { title: "没有找到结果", detail: "试试其他关键词。" },
    trash: { title: "回收站是空的", detail: "删除的便签会暂存在这里。" },
  };
</script>

<div class:compact class="empty-state" role="status">
  <div class="empty-icon" aria-hidden="true">
    {#if variant === "search"}
      <SearchX size={compact ? 24 : 30} strokeWidth={1.6} />
    {:else if variant === "trash"}
      <Trash2 size={compact ? 24 : 30} strokeWidth={1.6} />
    {:else}
      <StickyNote size={compact ? 24 : 30} strokeWidth={1.6} />
    {/if}
  </div>
  <h2>{title ?? defaults[variant].title}</h2>
  <p>{detail ?? defaults[variant].detail}</p>
  {#if actionLabel && onaction}
    <button type="button" onclick={onaction}>{actionLabel}</button>
  {/if}
</div>

<style>
  .empty-state {
    display: flex;
    min-height: 240px;
    padding: 38px 24px;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    color: var(--app-muted, #686868);
    text-align: center;
  }

  .empty-state.compact {
    min-height: 150px;
    padding: 24px 16px;
  }

  .empty-icon {
    display: grid;
    width: 50px;
    height: 50px;
    margin-bottom: 13px;
    place-items: center;
    color: #5d5d5d;
    background: #e7e7e7;
    border-radius: 50%;
  }

  .compact .empty-icon {
    width: 42px;
    height: 42px;
    margin-bottom: 10px;
  }

  h2 {
    margin: 0;
    color: var(--app-fg, #202020);
    font-size: 16px;
    font-weight: 600;
    line-height: 1.35;
  }

  p {
    max-width: 280px;
    margin: 7px 0 0;
    font-size: 13px;
    line-height: 1.5;
  }

  button {
    min-height: 32px;
    padding: 5px 14px;
    margin-top: 17px;
    color: #ffffff;
    font-size: 13px;
    font-weight: 600;
    background: var(--app-accent, #0067c0);
    border: 1px solid #00589f;
    border-radius: 4px;
    cursor: default;
  }

  button:hover {
    background: #005da9;
  }
</style>

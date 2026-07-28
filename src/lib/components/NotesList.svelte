<script lang="ts">
  import {
    Bell,
    ChartGantt,
    Plus,
    ScanLine,
    Search,
    Settings,
    Timer,
    Trash2,
    Wrench,
    X,
  } from "@lucide/svelte";
  import type { ToolName } from "$lib/tools";
  import EmptyState from "./EmptyState.svelte";
  import NoteCard from "./NoteCard.svelte";
  import type { NoteListItem } from "./types";

  type ReorderPointer = {
    pointerId: number;
    sourceId: string;
    moved: boolean;
  };

  interface Props {
    notes: NoteListItem[];
    selectedId?: string | null;
    query?: string;
    loading?: boolean;
    reorderBusy?: boolean;
    trashCount?: number;
    screenshotShortcut?: string;
    onquerychange?: (query: string) => void;
    onreorder?: (orderedIds: string[]) => void | Promise<void>;
    oncreate?: () => void;
    onselect?: (id: string) => void;
    onopen?: (id: string) => void;
    ontogglepin?: (id: string, pinned: boolean) => void;
    ondelete?: (id: string) => void;
    onshowtrash?: () => void;
    onsettingsopen?: () => void;
    ontoolopen?: (tool: ToolName) => void | Promise<void>;
  }

  let {
    notes,
    selectedId = null,
    query = "",
    loading = false,
    reorderBusy = false,
    trashCount = 0,
    screenshotShortcut = "F1",
    onquerychange,
    onreorder,
    oncreate,
    onselect,
    onopen,
    ontogglepin,
    ondelete,
    onshowtrash,
    onsettingsopen,
    ontoolopen,
  }: Props = $props();

  let toolsOpen = $state(false);
  let toolsControl = $state<HTMLDivElement>();
  let listBody = $state<HTMLDivElement>();
  let noteGrid = $state<HTMLDivElement>();
  let reorderPointer = $state<ReorderPointer | null>(null);
  let reorderPreviewIds = $state<string[] | null>(null);

  let orderedNotes = $derived.by(() => {
    if (!reorderPreviewIds) return notes;
    const byId = new Map(notes.map((note) => [note.id, note]));
    const ordered = reorderPreviewIds.flatMap((id) => {
      const note = byId.get(id);
      return note ? [note] : [];
    });
    const included = new Set(ordered.map((note) => note.id));
    return [...ordered, ...notes.filter((note) => !included.has(note.id))];
  });

  function handleWindowClick(event: MouseEvent): void {
    if (toolsOpen && !toolsControl?.contains(event.target as Node)) {
      toolsOpen = false;
    }
  }

  function handleWindowKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && reorderPointer) {
      event.preventDefault();
      reorderPointer = null;
      reorderPreviewIds = null;
      return;
    }
    if (event.key === "Escape" && toolsOpen) {
      toolsOpen = false;
    }
  }

  async function openTool(tool: ToolName): Promise<void> {
    toolsOpen = false;
    await ontoolopen?.(tool);
  }

  function startReorder(id: string, event: PointerEvent): void {
    if (event.button !== 0 || query.trim() || notes.length < 2 || !onreorder) return;
    event.preventDefault();
    event.stopPropagation();
    (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
    reorderPreviewIds = notes.map((note) => note.id);
    reorderPointer = { pointerId: event.pointerId, sourceId: id, moved: false };
  }

  function cardAtPoint(x: number, y: number): HTMLElement | null {
    const hit = document.elementFromPoint?.(x, y);
    const hitCard = hit?.closest<HTMLElement>("[data-note-id]");
    if (hitCard && noteGrid?.contains(hitCard)) return hitCard;
    return (
      Array.from(noteGrid?.querySelectorAll<HTMLElement>("[data-note-id]") ?? []).find((card) => {
        const bounds = card.getBoundingClientRect();
        return x >= bounds.left && x <= bounds.right && y >= bounds.top && y <= bounds.bottom;
      }) ?? null
    );
  }

  function updateReorder(event: PointerEvent): void {
    const pointer = reorderPointer;
    if (!pointer || event.pointerId !== pointer.pointerId || !reorderPreviewIds) return;
    event.preventDefault();
    const scrollContainer = listBody;
    const scrollBounds = scrollContainer?.getBoundingClientRect();
    if (scrollContainer && scrollBounds) {
      const edge = Math.min(48, scrollBounds.height / 4);
      if (event.clientY < scrollBounds.top + edge) scrollContainer.scrollTop -= 14;
      else if (event.clientY > scrollBounds.bottom - edge) scrollContainer.scrollTop += 14;
    }
    const targetId = cardAtPoint(event.clientX, event.clientY)?.dataset.noteId;
    if (!targetId || targetId === pointer.sourceId) return;

    const next = [...reorderPreviewIds];
    const sourceIndex = next.indexOf(pointer.sourceId);
    const targetIndex = next.indexOf(targetId);
    if (sourceIndex < 0 || targetIndex < 0) return;
    next.splice(sourceIndex, 1);
    const targetAfterRemoval = next.indexOf(targetId);
    next.splice(targetAfterRemoval + (sourceIndex < targetIndex ? 1 : 0), 0, pointer.sourceId);
    reorderPreviewIds = next;
    reorderPointer = { ...pointer, moved: true };
  }

  function finishReorder(event: PointerEvent, cancelled = false): void {
    const pointer = reorderPointer;
    if (!pointer || event.pointerId !== pointer.pointerId) return;
    const orderedIds = reorderPreviewIds;
    reorderPointer = null;
    reorderPreviewIds = null;
    if (!cancelled && pointer.moved && orderedIds) void onreorder?.(orderedIds);
  }

  function moveWithKeyboard(id: string, event: KeyboardEvent): void {
    if (query.trim() || notes.length < 2 || !onreorder) return;
    const currentIndex = notes.findIndex((note) => note.id === id);
    if (currentIndex < 0) return;

    let targetIndex = currentIndex;
    if (event.key === "ArrowUp" || event.key === "ArrowLeft") targetIndex -= 1;
    else if (event.key === "ArrowDown" || event.key === "ArrowRight") targetIndex += 1;
    else if (event.key === "Home") targetIndex = 0;
    else if (event.key === "End") targetIndex = notes.length - 1;
    else return;

    event.preventDefault();
    event.stopPropagation();
    targetIndex = Math.max(0, Math.min(notes.length - 1, targetIndex));
    if (targetIndex === currentIndex) return;
    const orderedIds = notes.map((note) => note.id);
    const [movedId] = orderedIds.splice(currentIndex, 1);
    orderedIds.splice(targetIndex, 0, movedId);
    void onreorder(orderedIds);
  }
</script>

<svelte:window
  onclick={handleWindowClick}
  onkeydown={handleWindowKeydown}
  onpointermove={updateReorder}
  onpointerup={(event) => finishReorder(event)}
  onpointercancel={(event) => finishReorder(event, true)}
/>

<section class="notes-list" aria-label="便签列表">
  <header class="list-header">
    <div class="brand-row">
      <h1>飞花 - PetalDesk</h1>
      <div class="header-actions">
        {#if onsettingsopen}
          <button
            type="button"
            class="icon-button settings-button"
            aria-label="打开设置"
            data-tooltip="设置"
            data-tooltip-placement="bottom"
            onclick={onsettingsopen}
          >
            <Settings size={18} aria-hidden="true" />
          </button>
        {/if}
        {#if onshowtrash}
          <button
            type="button"
            class="icon-button trash-button"
            aria-label={trashCount > 0 ? `回收站，${trashCount} 条便签` : "回收站"}
            data-tooltip="回收站"
            data-tooltip-placement="bottom"
            onclick={onshowtrash}
          >
            <Trash2 size={18} aria-hidden="true" />
            {#if trashCount > 0}
              <span class="count-badge" aria-hidden="true">{trashCount > 99 ? "99+" : trashCount}</span>
            {/if}
          </button>
        {/if}
        {#if ontoolopen}
          <div class="tools-control" bind:this={toolsControl}>
            <button
              type="button"
              class="icon-button tools-button"
              aria-label="小工具"
              aria-expanded={toolsOpen}
              aria-haspopup="menu"
              data-tooltip="小工具"
              data-tooltip-placement="bottom"
              onclick={() => (toolsOpen = !toolsOpen)}
            >
              <Wrench size={20} strokeWidth={2.2} aria-hidden="true" />
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
      </div>
    </div>

    <div class="search-row">
      <label class="search-box">
        <Search size={16} aria-hidden="true" />
        <span class="sr-only">搜索便签</span>
        <input
          type="search"
          value={query}
          placeholder="搜索便签"
          autocomplete="off"
          oninput={(event) => onquerychange?.(event.currentTarget.value)}
        />
        {#if query}
          <button
            type="button"
            class="clear-search"
            aria-label="清除搜索"
            onclick={() => onquerychange?.("")}
          >
            <X size={14} aria-hidden="true" />
          </button>
        {/if}
      </label>

    </div>
  </header>

  <div class="list-body" bind:this={listBody}>
    {#if loading}
      <div class="loading-list" aria-label="正在加载便签" aria-busy="true">
        {#each Array(4) as _}
          <div class="skeleton" aria-hidden="true">
            <span></span><span></span><span></span>
          </div>
        {/each}
      </div>
    {:else if notes.length > 0 || !query.trim()}
      <div class="note-grid" bind:this={noteGrid} aria-busy={reorderBusy}>
        {#each orderedNotes as note (note.id)}
          <NoteCard
            {note}
            selected={selectedId === note.id}
            reorderable={!query.trim() && notes.length > 1 && Boolean(onreorder)}
            dragging={reorderPointer?.sourceId === note.id}
            {onselect}
            {onopen}
            onreorderpointerdown={startReorder}
            onreorderkeydown={moveWithKeyboard}
            {ontogglepin}
            {ondelete}
          />
        {/each}
        {#if !query.trim() && oncreate}
          <button type="button" class="create-note-tile" onclick={oncreate}>
            <span class="create-note-icon" aria-hidden="true">
              <Plus size={32} strokeWidth={1.9} />
            </span>
            <span>新建便签</span>
          </button>
        {/if}
      </div>
    {:else if query.trim()}
      <EmptyState variant="search" compact />
    {/if}
  </div>

  {#if !loading && notes.length > 0}
    <footer>{notes.length} 条便签</footer>
  {/if}
</section>

<style>
  .notes-list {
    display: grid;
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    grid-template-rows: auto minmax(0, 1fr) auto;
    color: var(--app-fg, #202020);
    background: var(--app-bg, #f3f3f3);
  }

  .list-header {
    position: relative;
    z-index: 3;
    padding: 15px 18px 12px;
    background: rgb(243 243 243 / 95%);
    border-bottom: 1px solid var(--app-border, #d8d8d8);
    backdrop-filter: blur(16px);
  }

  .brand-row,
  .search-row,
  .header-actions {
    display: flex;
    min-width: 0;
    align-items: center;
  }

  .brand-row {
    min-height: 34px;
    justify-content: space-between;
    gap: 12px;
  }

  h1 {
    min-width: 0;
    margin: 0;
    overflow: hidden;
    color: #181818;
    font-size: 21px;
    font-weight: 650;
    line-height: 1.2;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .header-actions {
    flex: 0 0 auto;
    gap: 3px;
  }

  .tools-control {
    position: relative;
  }

  .tools-button {
    color: #ffffff;
    background: var(--app-accent, #0067c0);
    border-color: #00589f;
  }

  .tools-button:hover,
  .tools-button[aria-expanded="true"] {
    color: #ffffff;
    background: #005da9;
  }

  .tools-menu {
    position: absolute;
    z-index: 20;
    top: calc(100% + 6px);
    right: 0;
    display: grid;
    width: min(180px, calc(100vw - 26px));
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
    min-height: 32px;
    padding: 6px 8px;
    align-items: center;
    gap: 8px;
    color: inherit;
    font-size: 12.5px;
    text-align: left;
    background: transparent;
    border: 0;
    border-radius: 4px;
  }

  .tools-menu button:hover,
  .tools-menu button:focus-visible {
    background: var(--app-surface-hover, #f5f5f5);
    outline: 0;
  }

  .trash-button {
    overflow: visible;
  }

  .count-badge {
    position: absolute;
    top: -3px;
    right: -5px;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    color: #ffffff;
    font-size: 9px;
    font-weight: 700;
    line-height: 16px;
    text-align: center;
    background: var(--app-danger, #c42b1c);
    border: 2px solid var(--app-bg, #f3f3f3);
    border-radius: 8px;
  }

  .search-row {
    margin-top: 12px;
    gap: 8px;
  }

  .search-box {
    display: grid;
    min-width: 0;
    height: 34px;
    flex: 1 1 auto;
    align-items: center;
    grid-template-columns: 20px minmax(0, 1fr) auto;
    padding: 0 7px 0 10px;
    color: #666666;
    background: #ffffff;
    border: 1px solid var(--app-border-strong, #b9b9b9);
    border-radius: 5px;
  }

  .search-box:focus-within {
    border-color: var(--app-focus, #005fb8);
    box-shadow: inset 0 -2px var(--app-focus, #005fb8);
  }

  input {
    width: 100%;
    min-width: 0;
    height: 32px;
    padding: 0 5px;
    color: var(--app-fg, #202020);
    font-size: 13px;
    background: transparent;
    border: 0;
    outline: 0;
  }

  input::-webkit-search-cancel-button {
    display: none;
  }

  .clear-search {
    display: grid;
    width: 24px;
    height: 24px;
    padding: 0;
    place-items: center;
    color: #686868;
    background: transparent;
    border: 0;
    border-radius: 3px;
  }

  .clear-search:hover {
    color: #202020;
    background: #ededed;
  }

  .list-body {
    min-height: 0;
    overflow: auto;
    overscroll-behavior: contain;
  }

  .note-grid,
  .loading-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 230px), 1fr));
    gap: 10px;
    padding: 14px 18px 18px;
  }

  .create-note-tile {
    display: flex;
    min-width: 0;
    min-height: 122px;
    padding: 18px;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    gap: 10px;
    color: var(--app-accent, #0067c0);
    font-size: 13px;
    font-weight: 600;
    background: rgb(255 255 255 / 72%);
    border: 1px dashed color-mix(in srgb, var(--app-accent, #0067c0), transparent 48%);
    border-radius: 5px;
    box-shadow: 0 1px 2px rgb(0 0 0 / 4%);
  }

  .create-note-tile:hover {
    background: #ffffff;
    border-color: var(--app-accent, #0067c0);
    box-shadow: 0 3px 10px rgb(0 0 0 / 9%);
  }

  .create-note-tile:active {
    background: #f4f9fd;
    transform: translateY(1px);
  }

  .create-note-tile:focus-visible {
    outline: 2px solid var(--app-focus, #005fb8);
    outline-offset: 2px;
  }

  .create-note-icon {
    display: grid;
    width: 48px;
    height: 48px;
    place-items: center;
    color: #ffffff;
    background: var(--app-accent, #0067c0);
    border-radius: 50%;
    box-shadow: 0 3px 9px rgb(0 103 192 / 24%);
  }

  .skeleton {
    display: flex;
    min-height: 104px;
    padding: 14px;
    flex-direction: column;
    background: #e7e7e7;
    border: 1px solid #dedede;
    border-radius: 5px;
    animation: pulse 1.25s ease-in-out infinite alternate;
  }

  .skeleton span {
    height: 10px;
    margin-bottom: 9px;
    background: #d4d4d4;
    border-radius: 3px;
  }

  .skeleton span:first-child {
    width: 58%;
    height: 13px;
  }

  .skeleton span:last-child {
    width: 34%;
    margin-top: auto;
    margin-bottom: 0;
  }

  footer {
    min-height: 27px;
    padding: 5px 18px 6px;
    color: var(--app-muted, #686868);
    font-size: 11.5px;
    line-height: 16px;
    background: var(--app-bg, #f3f3f3);
    border-top: 1px solid var(--app-border, #d8d8d8);
  }

  @keyframes pulse {
    from { opacity: 0.62; }
    to { opacity: 1; }
  }

  @media (max-width: 520px) {
    .list-header {
      padding-inline: 13px;
    }

    .note-grid,
    .loading-list {
      grid-template-columns: 1fr;
      padding-inline: 13px;
    }

    footer {
      padding-inline: 13px;
    }
  }
</style>

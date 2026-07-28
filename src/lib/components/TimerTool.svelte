<script lang="ts">
  import { onMount } from "svelte";
  import { List, Pause, Play, RotateCcw, Trash2, X } from "@lucide/svelte";
  import { notesApi } from "../bridge";
  import {
    DEFAULT_TIMER_DIGIT_OPACITY,
    MAX_TIMER_DIGIT_OPACITY,
    MIN_TIMER_DIGIT_OPACITY,
    createTimerStore,
    formatTimerExact,
    formatTimerMain,
    formatTimerTimestamp,
    loadTimerDigitOpacity,
    normalizeTimerDigitOpacity,
    parseTimerPersistedState,
    saveTimerDigitOpacity,
    timerActionLabel,
    type TimerActionFilter,
    type TimerData,
    type TimerLogEntry,
    type TimerStorage,
    type TimerStore,
  } from "../timer";

  let store: TimerStore | null = null;
  let elapsedMs = $state(0);
  let isRunning = $state(true);
  let logs = $state<TimerLogEntry[]>([]);
  let historyOpen = $state(false);
  let actionFilter = $state<TimerActionFilter>("all");
  let digitOpacity = $state(DEFAULT_TIMER_DIGIT_OPACITY);
  let collapsedWindowSize: { width: number; height: number } | null = null;
  let resizeGeneration = 0;
  let closeRecorded = false;
  let desktopPersistence = false;
  let persistenceQueue: Promise<void> = Promise.resolve();

  const COLLAPSED_MAX_WIDTH = 320;
  const COLLAPSED_MAX_HEIGHT = 194;

  const filters: ReadonlyArray<{ value: TimerActionFilter; label: string }> = [
    { value: "all", label: "全部" },
    { value: "reset", label: "重置" },
    { value: "pause", label: "暂停" },
    { value: "resume", label: "继续" },
  ];

  const sevenSegments = ["a", "b", "c", "d", "e", "f", "g"] as const;
  type SevenSegment = (typeof sevenSegments)[number];

  const activeSegments: Readonly<Record<string, ReadonlyArray<SevenSegment>>> = {
    "0": ["a", "b", "c", "d", "e", "f"],
    "1": ["b", "c"],
    "2": ["a", "b", "d", "e", "g"],
    "3": ["a", "b", "c", "d", "g"],
    "4": ["b", "c", "f", "g"],
    "5": ["a", "c", "d", "f", "g"],
    "6": ["a", "c", "d", "e", "f", "g"],
    "7": ["a", "b", "c"],
    "8": ["a", "b", "c", "d", "e", "f", "g"],
    "9": ["a", "b", "c", "d", "f", "g"],
  };

  function segmentIsActive(digit: string, segment: SevenSegment): boolean {
    return activeSegments[digit]?.includes(segment) ?? false;
  }

  let visibleLogs = $derived.by(() => {
    const matches =
      actionFilter === "all" ? logs : logs.filter((entry) => entry.action === actionFilter);
    return [...matches].reverse();
  });

  let displayParts = $derived.by(() => {
    const text = formatTimerMain(elapsedMs);
    const [hours = "00", minutes = "00"] = text.split(":");
    return {
      text,
      hours: Array.from(hours),
      minutes: Array.from(minutes),
      digitCount: hours.length + minutes.length,
    };
  });

  function applySnapshot(snapshot: ReturnType<TimerStore["snapshot"]>): void {
    elapsedMs = snapshot.elapsedMs;
    isRunning = snapshot.isRunning;
    logs = snapshot.logs;
  }

  function refreshClock(): void {
    if (store) applySnapshot(store.snapshot());
  }

  function resetTimer(): void {
    if (store) applySnapshot(store.reset());
  }

  function toggleTimer(): void {
    if (store) applySnapshot(store.toggle());
  }

  function clearHistory(): void {
    if (!store || !window.confirm("确定要清空所有计时记录吗？")) return;
    applySnapshot(store.clearLogs());
  }

  function queueDesktopPersist(rawState?: string): void {
    if (!desktopPersistence) return;
    const state = rawState
      ? parseTimerPersistedState(rawState)
      : store?.persistedState() ?? null;
    if (!state) return;
    const data: TimerData = { ...state, digitOpacity };
    persistenceQueue = persistenceQueue
      .then(async () => {
        await notesApi.saveTimerData(data);
      })
      .catch(() => undefined);
  }

  async function flushTimerPersistence(): Promise<void> {
    try {
      await persistenceQueue;
    } catch {
      // A transient persistence failure must not trap an otherwise closable tool window.
    }
  }

  function createDesktopTimerStorage(data: TimerData): TimerStorage {
    let rawState = JSON.stringify({
      version: data.version,
      accumulatedMs: data.accumulatedMs,
      runningSince: data.runningSince,
      logs: data.logs,
    });
    return {
      getItem: () => rawState,
      setItem: (_key, value) => {
        rawState = value;
        queueDesktopPersist(value);
      },
    };
  }

  function updateDigitOpacity(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    if (desktopPersistence) {
      digitOpacity = normalizeTimerDigitOpacity(Number(input.value));
      queueDesktopPersist();
    } else {
      digitOpacity = saveTimerDigitOpacity(window.localStorage, Number(input.value));
    }
  }

  async function resizeWindowForHistory(expanded: boolean): Promise<void> {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const generation = ++resizeGeneration;

    try {
      const [{ LogicalSize }, { getCurrentWindow }] = await Promise.all([
        import("@tauri-apps/api/dpi"),
        import("@tauri-apps/api/window"),
      ]);
      const appWindow = getCurrentWindow();
      const [physicalSize, scaleFactor] = await Promise.all([
        appWindow.innerSize(),
        appWindow.scaleFactor(),
      ]);
      if (generation !== resizeGeneration) return;

      const logicalSize = physicalSize.toLogical(scaleFactor);
      if (expanded) {
        collapsedWindowSize ??= { width: logicalSize.width, height: logicalSize.height };
        const expandedWidth = Math.max(520, logicalSize.width);
        const expandedHeight = Math.max(380, logicalSize.height + 260);
        await appWindow.setMaxSize(null);
        await appWindow.setSize(new LogicalSize(expandedWidth, expandedHeight));
      } else if (collapsedWindowSize !== null) {
        await appWindow.setSize(
          new LogicalSize(collapsedWindowSize.width, collapsedWindowSize.height),
        );
        collapsedWindowSize = null;
        await appWindow.setMaxSize(
          new LogicalSize(COLLAPSED_MAX_WIDTH, COLLAPSED_MAX_HEIGHT),
        );
      }
    } catch {
      // Browser preview and older capability sets can safely keep the inline list clipped.
    }
  }

  function toggleHistory(): void {
    historyOpen = !historyOpen;
    void resizeWindowForHistory(historyOpen);
  }

  async function beginDrag(event: PointerEvent): Promise<void> {
    if (event.button !== 0 || (event.target as Element | null)?.closest("button")) return;
    event.preventDefault();
    if (!("__TAURI_INTERNALS__" in window)) return;

    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().startDragging();
    } catch {
      // Native dragging is unavailable in a regular browser preview.
    }
  }

  function stopButtonDrag(event: PointerEvent): void {
    event.stopPropagation();
  }

  function recordClosePause(): void {
    if (closeRecorded || !store) return;
    store.pauseForClose();
    closeRecorded = true;
  }

  async function closeTimer(): Promise<void> {
    if (!("__TAURI_INTERNALS__" in window)) return;
    recordClosePause();
    if (historyOpen) {
      historyOpen = false;
      await resizeWindowForHistory(false);
    }
    await flushTimerPersistence();

    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().close();
    } catch {
      // Browser previews do not own a native window.
    }
  }

  async function beginResize(event: PointerEvent): Promise<void> {
    event.preventDefault();
    event.stopPropagation();
    if (!("__TAURI_INTERNALS__" in window)) return;

    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().startResizeDragging("SouthEast");
    } catch {
      // Native resize is unavailable in a regular browser preview.
    }
  }

  onMount(() => {
    let disposed = false;
    document.documentElement.classList.add("timer-tool-window");
    document.body.classList.add("timer-tool-window");

    const initializeStore = async () => {
      if (notesApi.isDesktop()) {
        desktopPersistence = true;
        let data: TimerData = {
          version: 1,
          accumulatedMs: 0,
          runningSince: null,
          logs: [],
          digitOpacity: DEFAULT_TIMER_DIGIT_OPACITY,
        };
        try {
          await notesApi.migrateLegacyTimerData();
          data = await notesApi.getTimerData();
        } catch {
          // Keep the timer usable in memory if the desktop backend is temporarily unavailable.
        }
        if (disposed) return;
        digitOpacity = normalizeTimerDigitOpacity(data.digitOpacity);
        store = createTimerStore({ storage: createDesktopTimerStorage(data) });
      } else {
        desktopPersistence = false;
        store = createTimerStore({ storage: window.localStorage });
        digitOpacity = loadTimerDigitOpacity(window.localStorage);
      }
      applySnapshot(store.startSession());
      closeRecorded = false;
    };
    void initializeStore();

    const handleBeforeUnload = recordClosePause;

    const interval = window.setInterval(refreshClock, 250);
    const handleVisibilityChange = () => {
      if (!document.hidden) refreshClock();
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      disposed = true;
      window.clearInterval(interval);
      window.removeEventListener("beforeunload", handleBeforeUnload);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      document.documentElement.classList.remove("timer-tool-window");
      document.body.classList.remove("timer-tool-window");
    };
  });
</script>

<section
  class:history-open={historyOpen}
  class="timer-tool"
  aria-label="计时器"
  data-testid="timer-tool"
>
  <div class="timer-stack" class:history-open={historyOpen}>
    <div
      class="timer-face"
      role="group"
      aria-label="计时器表盘"
      data-tauri-drag-region
      onpointerdown={beginDrag}
    >
    <output
      class="timer-value"
      aria-label="已计时时间"
      style={`--digit-count: ${displayParts.digitCount}; --digit-opacity: ${digitOpacity}`}
      data-tauri-drag-region
    >
      <span class="visually-hidden">{displayParts.text}</span>
      <span class="digit-group" aria-hidden="true">
        {#each displayParts.hours as digit, index (index)}
          <span class="seven-digit" data-digit={digit} data-testid="seven-segment-digit">
            {#each sevenSegments as segment}
              <span
                class={`segment segment-${segment}`}
                class:is-on={segmentIsActive(digit, segment)}
              ></span>
            {/each}
          </span>
        {/each}
      </span>
      <span
        class="timer-colon"
        class:is-running={isRunning}
        class:is-paused={!isRunning}
        data-testid="timer-colon"
        aria-hidden="true"
      >
        <span></span>
        <span></span>
      </span>
      <span class="digit-group" aria-hidden="true">
        {#each displayParts.minutes as digit, index (index)}
          <span class="seven-digit" data-digit={digit} data-testid="seven-segment-digit">
            {#each sevenSegments as segment}
              <span
                class={`segment segment-${segment}`}
                class:is-on={segmentIsActive(digit, segment)}
              ></span>
            {/each}
          </span>
        {/each}
      </span>
    </output>

    <div
      class="timer-actions full-card-overlay"
      role="toolbar"
      aria-label="计时器操作"
      data-testid="timer-control-overlay"
      data-overlay="full-card"
    >
      <button
        class="tool-button"
        type="button"
        aria-label="重置计时"
        title="重置"
        onpointerdown={stopButtonDrag}
        onclick={resetTimer}
      >
        <RotateCcw size="1em" strokeWidth={2} aria-hidden="true" />
      </button>
      <button
        class="tool-button"
        type="button"
        aria-label={isRunning ? "暂停计时" : "继续计时"}
        title={isRunning ? "暂停" : "继续"}
        onpointerdown={stopButtonDrag}
        onclick={toggleTimer}
      >
        {#if isRunning}
          <Pause size="1em" strokeWidth={2.2} aria-hidden="true" />
        {:else}
          <Play size="1em" strokeWidth={2.2} aria-hidden="true" />
        {/if}
      </button>
      <button
        class="tool-button"
        class:active={historyOpen}
        type="button"
        aria-label={historyOpen ? "收起计时记录" : "展开计时记录"}
        aria-expanded={historyOpen}
        title="记录"
        onpointerdown={stopButtonDrag}
        onclick={toggleHistory}
      >
        <List size="1em" strokeWidth={2} aria-hidden="true" />
      </button>
      <button
        class="tool-button"
        type="button"
        aria-label="关闭计时器"
        title="关闭"
        onpointerdown={stopButtonDrag}
        onclick={closeTimer}
      >
        <X size="1em" strokeWidth={2.2} aria-hidden="true" />
      </button>
      <input
        class="opacity-slider"
        type="range"
        min={MIN_TIMER_DIGIT_OPACITY}
        max={MAX_TIMER_DIGIT_OPACITY}
        step="0.05"
        value={digitOpacity}
        aria-label="数字透明度"
        aria-valuetext={`${Math.round(digitOpacity * 100)}%`}
        title={`数字透明度 ${Math.round(digitOpacity * 100)}%`}
        onpointerdown={stopButtonDrag}
        oninput={updateDigitOpacity}
      />
      </div>
    </div>

    {#if historyOpen}
      <section class="timer-history" aria-label="计时记录">
      <div class="history-filters" role="group" aria-label="按动作筛选">
        {#each filters as filter}
          <button
            type="button"
            class:active={actionFilter === filter.value}
            aria-pressed={actionFilter === filter.value}
            onclick={() => (actionFilter = filter.value)}
          >{filter.label}</button>
        {/each}
        <button class="clear-history" type="button" onclick={clearHistory}>
          <Trash2 size="1em" strokeWidth={2} aria-hidden="true" />
          清空
        </button>
      </div>

      <div class="history-table">
        <div class="history-columns" aria-hidden="true">
          <span>时间</span>
          <span>动作</span>
          <span>计时</span>
        </div>
        <ol class="history-list" aria-live="polite">
          {#each visibleLogs as entry}
            <li>
              <time datetime={new Date(entry.timestamp).toISOString()}>
                {formatTimerTimestamp(entry.timestamp)}
              </time>
              <span>{timerActionLabel(entry.action)}</span>
              <span class="elapsed">{formatTimerExact(entry.elapsedMs)}</span>
            </li>
          {:else}
            <li class="empty-history">暂无符合条件的记录</li>
          {/each}
        </ol>
      </div>
      </section>
    {/if}

    <button
      type="button"
      class="resize-handle"
      aria-label="调整计时器大小"
      title="拖动调整大小"
      onpointerdown={beginResize}
    ></button>
  </div>
</section>

<style>
  :global(html.timer-tool-window),
  :global(body.timer-tool-window) {
    background: transparent !important;
  }

  .timer-tool {
    position: fixed;
    inset: 0;
    display: flex;
    box-sizing: border-box;
    align-items: center;
    justify-content: center;
    min-width: 0;
    min-height: 0;
    padding: clamp(2px, 1.4cqw, 6px);
    overflow: hidden;
    color: #ffffff;
    font-family: "Segoe UI Variable Display", "Segoe UI", "Microsoft YaHei UI", sans-serif;
    background: transparent;
    container-type: size;
    user-select: none;
  }

  .timer-stack {
    position: relative;
    display: flex;
    box-sizing: border-box;
    max-width: 100%;
    max-height: 100%;
    min-width: 0;
    min-height: 0;
    align-items: center;
    flex-direction: column;
  }

  .timer-stack.history-open {
    width: min(500px, 100%);
    height: 100%;
  }

  .timer-face {
    position: relative;
    display: grid;
    flex: 0 0 auto;
    box-sizing: border-box;
    width: max-content;
    max-width: 100%;
    min-width: 0;
    min-height: 0;
    padding: clamp(4px, 2cqw, 10px) clamp(5px, 2.8cqw, 13px);
    place-items: center;
    background: transparent;
    border-radius: clamp(7px, 2.5cqw, 12px);
    cursor: move;
    isolation: isolate;
  }

  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .timer-value {
    display: flex;
    max-width: calc(100cqw - 12px);
    align-items: center;
    gap: clamp(3px, 1.2cqw, 7px);
    overflow: visible;
    pointer-events: none;
    cursor: move;
  }

  .digit-group {
    display: flex;
    flex: 0 1 auto;
    gap: clamp(1px, 1.1cqw, 6px);
    opacity: var(--digit-opacity, 1);
  }

  .seven-digit {
    position: relative;
    display: block;
    flex: 0 1 auto;
    width: clamp(
      9px,
      min(calc((100cqw - 46px) / var(--digit-count)), 30cqh),
      58px
    );
    aspect-ratio: 0.56;
  }

  .segment {
    position: absolute;
    display: block;
    background: transparent;
    filter: none;
    opacity: 0;
    transition: opacity 90ms linear;
  }

  .segment.is-on {
    background: rgb(210 214 218);
    filter: none;
    opacity: 1;
  }

  .segment-a,
  .segment-d,
  .segment-g {
    left: 14%;
    width: 72%;
    height: 8%;
    clip-path: polygon(7% 0, 93% 0, 100% 50%, 93% 100%, 7% 100%, 0 50%);
  }

  .segment-a {
    top: 0;
  }

  .segment-g {
    top: 46%;
  }

  .segment-d {
    bottom: 0;
  }

  .segment-b,
  .segment-c,
  .segment-e,
  .segment-f {
    width: 12%;
    height: 40%;
    clip-path: polygon(50% 0, 100% 8%, 100% 92%, 50% 100%, 0 92%, 0 8%);
  }

  .segment-b,
  .segment-f {
    top: 6%;
  }

  .segment-c,
  .segment-e {
    bottom: 6%;
  }

  .segment-b,
  .segment-c {
    right: 0;
  }

  .segment-e,
  .segment-f {
    left: 0;
  }

  .timer-colon {
    position: relative;
    display: flex;
    flex: 0 0 clamp(4px, 2.4cqw, 14px);
    width: clamp(4px, 2.4cqw, 14px);
    height: clamp(18px, min(45cqh, 76px), 76px);
    flex-direction: column;
    align-items: center;
    justify-content: space-around;
    opacity: 1;
  }

  .timer-colon span {
    width: clamp(2px, 1.4cqw, 8px);
    aspect-ratio: 1;
    background: rgb(210 214 218);
    border-radius: 50%;
    opacity: var(--digit-opacity, 1);
  }

  .timer-colon.is-running {
    animation: timer-colon-blink 2s step-end infinite !important;
  }

  .timer-colon.is-paused {
    animation: none;
    opacity: 1;
  }

  @keyframes timer-colon-blink {
    0%,
    49% {
      opacity: 1;
    }
    50%,
    100% {
      opacity: 0;
    }
  }

  .timer-actions {
    position: absolute;
    inset: 0;
    z-index: 1;
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    grid-template-rows: minmax(0, 1fr) auto;
    gap: clamp(1px, 1.3cqw, 7px);
    align-items: center;
    justify-items: center;
    padding: clamp(3px, 1.5cqw, 8px) clamp(4px, 2cqw, 12px);
    pointer-events: none;
    background: rgb(24 26 30 / 50%);
    border: 1px solid rgb(255 255 255 / 8%);
    border-radius: inherit;
    backdrop-filter: blur(7px);
    opacity: 0;
    transition: opacity 120ms ease;
  }

  .timer-face:hover .timer-actions,
  .timer-face:focus-within .timer-actions {
    pointer-events: auto;
    opacity: 1;
  }

  .tool-button {
    display: grid;
    width: clamp(14px, min(14cqw, 42cqh), 50px);
    height: clamp(14px, min(14cqw, 42cqh), 50px);
    padding: 0;
    place-items: center;
    color: rgb(255 255 255 / 82%);
    font-size: clamp(11px, min(5.8cqw, 17cqh), 23px);
    background: transparent;
    border: 0;
    border-radius: 50%;
    cursor: pointer;
  }

  .tool-button:hover,
  .tool-button:focus-visible,
  .tool-button.active {
    color: #ffffff;
    background: rgb(255 255 255 / 18%);
  }

  .opacity-slider {
    grid-column: 1 / -1;
    width: min(88%, 240px);
    min-width: 0;
    height: clamp(12px, 10cqh, 20px);
    margin: 0;
    accent-color: rgb(255 255 255 / 92%);
    cursor: pointer;
  }

  .opacity-slider:focus-visible {
    outline: 2px solid rgb(255 255 255 / 82%);
    outline-offset: 2px;
  }

  .timer-history {
    display: flex;
    flex: 1 1 auto;
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    min-height: 0;
    padding: 0 clamp(10px, 4cqw, 18px) clamp(12px, 4cqw, 18px);
    margin-top: clamp(6px, 2cqh, 10px);
    overflow-x: auto;
    overflow-y: hidden;
    flex-direction: column;
    background: rgb(18 20 24 / 76%);
    border: 1px solid rgb(255 255 255 / 12%);
    border-radius: clamp(7px, 2.5cqw, 12px);
    backdrop-filter: blur(10px);
  }

  .history-filters {
    display: flex;
    flex: 0 0 auto;
    gap: 4px;
    padding: 9px 0 8px;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .history-filters .clear-history {
    display: inline-flex;
    margin-left: auto;
    align-items: center;
    gap: 4px;
    color: rgb(255 205 205 / 76%);
  }

  .history-filters .clear-history:hover,
  .history-filters .clear-history:focus-visible {
    color: #ffffff;
    background: rgb(200 62 62 / 34%);
  }

  .history-filters::-webkit-scrollbar {
    display: none;
  }

  .history-filters button {
    flex: 0 0 auto;
    padding: 3px 9px 4px;
    color: rgb(255 255 255 / 64%);
    font-size: clamp(11px, 3.7cqw, 13px);
    line-height: 1.35;
    background: transparent;
    border: 0;
    border-radius: 999px;
    cursor: pointer;
  }

  .history-filters button:hover,
  .history-filters button:focus-visible,
  .history-filters button.active {
    color: #ffffff;
    background: rgb(255 255 255 / 16%);
  }

  .history-table {
    display: flex;
    flex: 1 1 auto;
    width: max(100%, 430px);
    min-width: 430px;
    min-height: 0;
    flex-direction: column;
  }

  .history-columns,
  .history-list li {
    display: grid;
    grid-template-columns: minmax(190px, 1fr) 72px minmax(90px, auto);
    gap: clamp(6px, 2.5cqw, 12px);
    align-items: center;
  }

  .history-columns {
    flex: 0 0 auto;
    padding: 0 5px 5px;
    color: rgb(255 255 255 / 42%);
    font-size: clamp(10px, 3.3cqw, 12px);
  }

  .history-columns span:last-child {
    text-align: right;
  }

  .history-list {
    min-height: 0;
    padding: 0;
    margin: 0;
    overflow: auto;
    list-style: none;
    scrollbar-color: rgb(255 255 255 / 28%) transparent;
    scrollbar-width: thin;
  }

  .history-list li {
    min-height: 30px;
    padding: 6px 5px;
    color: rgb(255 255 255 / 78%);
    font-size: clamp(10px, 3.45cqw, 12.5px);
    font-variant-numeric: tabular-nums;
    line-height: 1.35;
    border-top: 1px solid rgb(255 255 255 / 8%);
  }

  .history-list time,
  .history-list .elapsed {
    white-space: nowrap;
  }

  .history-list .elapsed {
    color: rgb(255 255 255 / 92%);
    text-align: right;
  }

  .history-list .empty-history {
    display: grid;
    min-height: 80px;
    color: rgb(255 255 255 / 48%);
    place-items: center;
    border: 0;
  }

  .resize-handle {
    position: absolute;
    right: 0;
    bottom: 0;
    z-index: 3;
    width: clamp(14px, 5cqw, 22px);
    height: clamp(14px, 5cqw, 22px);
    padding: 0;
    pointer-events: none;
    background: linear-gradient(
      135deg,
      transparent 0 46%,
      rgb(255 255 255 / 0%) 46% 55%,
      rgb(255 255 255 / 34%) 55% 60%,
      transparent 60% 70%,
      rgb(255 255 255 / 34%) 70% 75%,
      transparent 75%
    );
    border: 0;
    border-radius: 0 0 8px;
    cursor: nwse-resize;
    opacity: 0;
    transition: opacity 120ms ease;
  }

  .timer-stack:hover .resize-handle,
  .timer-stack:focus-within .resize-handle,
  .timer-stack.history-open .resize-handle {
    pointer-events: auto;
    opacity: 1;
  }

  @media (prefers-reduced-motion: reduce) {
    .timer-actions,
    .segment,
    .resize-handle {
      transition: none;
    }

  }
</style>

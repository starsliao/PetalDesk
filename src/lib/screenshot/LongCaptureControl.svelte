<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { ArrowLeft, Check, LoaderCircle, Pause, Play, RotateCcw, Undo2, X } from "@lucide/svelte";
  import { screenshotApi } from "./api";
  import { longCaptureStatusFromEvent } from "./long-capture";
  import type { LongCaptureStatus, ScreenshotApi } from "./types";

  interface Props {
    jobId: string;
    api?: ScreenshotApi;
    initialStatus?: LongCaptureStatus | null;
    floating?: boolean;
    monitor?: boolean;
    keyboardShortcuts?: boolean;
    controlTimeoutMs?: number;
    statusPollIntervalMs?: number;
    statusRetryBaseMs?: number;
    statusRetryMaxMs?: number;
    statusRetryLimit?: number;
    cancelLabel?: string;
    onstatus?: (status: LongCaptureStatus) => void;
    oncancel?: () => void;
    onready?: (status: LongCaptureStatus) => void;
    onerror?: (message: string) => void;
    onbusychange?: (busy: boolean) => void;
  }

  let {
    jobId,
    api = screenshotApi,
    initialStatus = null,
    floating = false,
    monitor = true,
    keyboardShortcuts = true,
    controlTimeoutMs = 5_000,
    statusPollIntervalMs = 750,
    statusRetryBaseMs = 500,
    statusRetryMaxMs = 5_000,
    statusRetryLimit = 6,
    cancelLabel = "取消长截图",
    onstatus,
    oncancel,
    onready,
    onerror,
    onbusychange,
  }: Props = $props();

  let status = $state<LongCaptureStatus | null>(null);
  let busy = $state(false);
  let error = $state("");
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  let actionGeneration = 0;
  let refreshGeneration = 0;
  let consecutiveRefreshFailures = 0;
  let pollingStopped = $state(false);

  function withTimeout<T>(promise: Promise<T>, message: string): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        reject(new Error(message));
      }, Math.max(250, controlTimeoutMs));
      void promise.then(
        (value) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          resolve(value);
        },
        (reason) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          reject(reason);
        },
      );
    });
  }

  function terminal(state: LongCaptureStatus["state"] | undefined): boolean {
    return state === "ready" || state === "failed" || state === "canceled";
  }

  $effect(() => {
    const next = initialStatus;
    if (next) untrack(() => (status = next));
  });

  function publish(next: LongCaptureStatus): void {
    if (next.jobId !== jobId) return;
    if (terminal(status?.state) && next.state !== status?.state && next.state !== "canceled") return;
    consecutiveRefreshFailures = 0;
    pollingStopped = false;
    status = next;
    error = "";
    onstatus?.(next);
    if (next.state === "ready") onready?.(next);
    else if (next.state === "canceled") oncancel?.();
    schedulePoll();
  }

  function consumeEvent(payload: unknown, fallbackState?: LongCaptureStatus["state"]): void {
    const next = longCaptureStatusFromEvent(payload, status, fallbackState);
    if (next?.jobId === jobId) publish(next);
  }

  function fail(value: unknown): void {
    error = value instanceof Error ? value.message : String(value || "长截图操作失败，请重试。");
    onerror?.(error);
  }

  async function refresh(): Promise<void> {
    const generation = refreshGeneration;
    try {
      const next = await withTimeout(api.getLongCaptureStatus(jobId), "读取长截图状态超时。");
      if (disposed || generation !== refreshGeneration) return;
      if (next) publish(next);
      else if (!terminal(status?.state)) {
        fail("暂时无法读取长截图状态，正在重试。");
        scheduleRetry();
      }
    } catch (value) {
      if (!disposed && generation === refreshGeneration && !terminal(status?.state)) {
        fail(value);
        scheduleRetry();
      }
    }
  }

  function schedulePoll(delay = statusPollIntervalMs): void {
    if (pollTimer) clearTimeout(pollTimer);
    pollTimer = undefined;
    if (!monitor || busy || pollingStopped || terminal(status?.state)) return;
    pollTimer = setTimeout(() => {
      pollTimer = undefined;
      void refresh();
    }, Math.max(1, delay));
  }

  function scheduleRetry(): void {
    consecutiveRefreshFailures += 1;
    if (consecutiveRefreshFailures >= Math.max(1, statusRetryLimit)) {
      pollingStopped = true;
      fail("无法连接长截图任务，请重连或取消。");
      return;
    }
    const exponent = Math.min(consecutiveRefreshFailures, 8);
    const base = Math.max(1, statusRetryBaseMs);
    const maximum = Math.max(base, statusRetryMaxMs);
    const delay = Math.min(maximum, base * 2 ** exponent);
    schedulePoll(delay);
  }

  function reconnect(): void {
    if (busy) return;
    consecutiveRefreshFailures = 0;
    pollingStopped = false;
    error = "";
    void refresh();
  }

  type ControlAction = "pause" | "resume" | "retry" | "undo" | "finish" | "cancel";
  let activeAction = $state<ControlAction | null>(null);

  function canRun(action: ControlAction): boolean {
    if (action === "cancel") return true;
    if (!status) return false;
    if (action === "pause") return status.state === "capturing";
    if (action === "resume" || action === "retry") return status.state === "paused";
    if (action === "undo") {
      return status.state !== "ready" && status.state !== "failed" && status.state !== "canceled" && status.canUndo;
    }
    return status.state !== "ready"
      && status.state !== "failed"
      && status.state !== "canceled"
      && status.frameCount > 0;
  }

  async function run(action: ControlAction): Promise<void> {
    if ((busy && action !== "cancel") || (busy && activeAction === "cancel") || !canRun(action)) return;
    const generation = ++actionGeneration;
    refreshGeneration += 1;
    busy = true;
    activeAction = action;
    onbusychange?.(true);
    error = "";
    try {
      const operation = action === "pause" ? api.pauseLongCapture(jobId)
        : action === "resume" ? api.resumeLongCapture(jobId)
          : action === "retry" ? api.retryLongCapture(jobId)
            : action === "undo" ? api.undoLongCapture(jobId)
              : action === "finish" ? api.finishLongCapture(jobId)
                : api.cancelLongCapture(jobId);
      const next = await withTimeout(operation, `${action === "cancel" ? "取消" : "控制"}长截图超时。`);
      if (disposed || generation !== actionGeneration) return;
      publish(next);
      if (action === "cancel" && next.state !== "canceled") oncancel?.();
    } catch (value) {
      if (!disposed && generation === actionGeneration) fail(value);
    } finally {
      if (!disposed && generation === actionGeneration) {
        busy = false;
        activeAction = null;
        onbusychange?.(false);
        schedulePoll();
      }
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (!keyboardShortcuts) return;
    if (event.key === "Escape") {
      event.preventDefault();
      void run("cancel");
    } else if (!busy && (event.code === "Space" || event.key === " ")) {
      event.preventDefault();
      if (status?.state === "capturing") void run("pause");
      else if (status?.state === "paused") void run("resume");
    } else if (!busy && event.key === "Enter" && canRun("finish")) {
      event.preventDefault();
      void run("finish");
    }
  }

  onMount(() => {
    disposed = false;
    const cleanups: Array<() => void> = [];
    if (!status) void refresh(); else schedulePoll();
    if (monitor && typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      void import("@tauri-apps/api/event").then(async ({ listen }) => {
        const listeners = await Promise.all([
          listen<unknown>("long_capture_progress", (event) => consumeEvent(event.payload, "capturing")),
          listen<unknown>("long_capture_paused", (event) => consumeEvent(event.payload, "paused")),
          listen<unknown>("long_capture_attention_required", (event) => consumeEvent(event.payload, "paused")),
          listen<unknown>("long_capture_ready", (event) => consumeEvent(event.payload, "ready")),
          listen<unknown>("long_capture_failed", (event) => consumeEvent(event.payload, "failed")),
        ]);
        if (disposed) listeners.forEach((cleanup) => cleanup());
        else cleanups.push(...listeners);
      }).catch(fail);
    }
    return () => {
      disposed = true;
      actionGeneration += 1;
      refreshGeneration += 1;
      if (busy) onbusychange?.(false);
      if (pollTimer) clearTimeout(pollTimer);
      cleanups.forEach((cleanup) => cleanup());
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class:floating class:failed={status?.state === "failed"} class="long-capture-control" role="toolbar" aria-label="长截图采集控制">
  {#if busy || !status || status.state === "preparing"}<LoaderCircle class="spin" size={17} />{/if}
  <strong>{status?.state === "failed" ? "长截图失败" : "长截图"}</strong>
  {#if status}
    <span class="stats">{status.frameCount} 帧 · {Math.round(status.height).toLocaleString()} px</span>
  {:else}
    <span class="stats">正在连接</span>
  {/if}
  {#if status?.engine === "manual" && !terminal(status.state)}
    <span class="manual-guide">
      {status.state === "paused" ? "已暂停，点击继续后再滚动" : "在原窗口向下滚动，完成后点“完成”"}
    </span>
  {/if}
  {#if error || status?.message}
    <span class="message" aria-live="polite" title={error || status?.message || ""}>{error || status?.message}</span>
  {/if}
  <span class="separator"></span>
  {#if pollingStopped}
    <button class="labeled" type="button" title="重新连接长截图任务" aria-label="重新连接长截图" onclick={reconnect}>
      <RotateCcw size={17} /><span>重连</span>
    </button>
  {/if}
  {#if status?.state === "failed"}
    <button class="failed-exit" type="button" title={cancelLabel} aria-label={cancelLabel} disabled={busy && activeAction === "cancel"} onclick={() => void run("cancel")}>
      {#if floating}<ArrowLeft size={17} />{:else}<X size={17} />{/if}<span>{cancelLabel}</span>
    </button>
  {:else}
    {#if status?.state === "capturing"}
      <button class:labeled={status.engine === "manual"} type="button" title={keyboardShortcuts ? "暂停 Space" : "暂停"} aria-label="暂停长截图" disabled={busy} onclick={() => void run("pause")}>
        <Pause size={17} />{#if status.engine === "manual"}<span>暂停</span>{/if}
      </button>
    {:else if status?.state === "paused"}
      <button class:labeled={status.engine === "manual"} type="button" title={keyboardShortcuts ? "继续 Space" : "继续"} aria-label="继续长截图" disabled={busy} onclick={() => void run("resume")}>
        <Play size={17} />{#if status.engine === "manual"}<span>继续</span>{/if}
      </button>
      <button type="button" title="重试当前段" aria-label="重试当前段" disabled={busy} onclick={() => void run("retry")}><RotateCcw size={17} /></button>
    {/if}
    <button type="button" title="回退上一段" aria-label="回退上一段" disabled={busy || !canRun("undo")} onclick={() => void run("undo")}><Undo2 size={17} /></button>
    <button class="finish labeled" type="button" title={keyboardShortcuts ? "完成 Enter" : "完成"} aria-label="完成长截图" disabled={busy || !canRun("finish")} onclick={() => void run("finish")}><Check size={17} /><span>完成</span></button>
    <button type="button" title={cancelLabel} aria-label={cancelLabel} disabled={busy && activeAction === "cancel"} onclick={() => void run("cancel")}><X size={17} /></button>
  {/if}
</div>

<style>
  .long-capture-control { display: flex; box-sizing: border-box; width: 100%; height: 100%; min-height: 52px; padding: 6px 9px 6px 12px; align-items: center; gap: 8px; color: #242424; background: rgb(250 250 250 / 98%); border: 1px solid rgb(0 0 0 / 25%); cursor: default; overflow: hidden; font: 12px "Segoe UI", sans-serif; }
  .long-capture-control.floating { position: absolute; z-index: 40; top: 12px; left: 50%; width: auto; min-width: min(540px, calc(100vw - 24px)); height: 44px; min-height: 44px; max-width: calc(100vw - 24px); border-radius: 6px; box-shadow: 0 6px 20px rgb(0 0 0 / 28%); transform: translateX(-50%); }
  strong, .stats { flex: 0 0 auto; white-space: nowrap; }
  strong { font-size: 12px; }
  .stats { color: #555; }
  .manual-guide { flex: 0 0 auto; color: #174f78; font-weight: 600; white-space: nowrap; }
  .message { min-width: 0; flex: 1 1 auto; color: #555; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .long-capture-control.failed .message { color: #8a2d22; }
  .separator { width: 1px; height: 24px; margin-left: auto; background: #d2d2d2; }
  button { display: grid; flex: 0 0 auto; width: 32px; height: 32px; padding: 0; place-items: center; color: #2d2d2d; background: transparent; border: 1px solid transparent; border-radius: 4px; }
  button.labeled { display: flex; width: auto; padding: 0 9px; align-items: center; gap: 4px; white-space: nowrap; }
  button:hover:not(:disabled) { background: #e7e7e7; border-color: #d0d0d0; }
  button:disabled { opacity: .42; }
  button.finish { color: #fff; background: #0067c0; border-color: #005a9e; }
  button.finish:hover:not(:disabled) { color: #fff; background: #004f93; border-color: #003f76; }
  button.failed-exit { display: flex; width: auto; padding: 0 10px; gap: 5px; color: #fff; background: #0067c0; border-color: #005a9e; white-space: nowrap; }
  button.failed-exit:hover:not(:disabled) { background: #005a9e; border-color: #004e8c; }
  :global(.long-capture-control .spin) { animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 520px) { .stats, .message { display: none; } .manual-guide { font-size: 11px; } .long-capture-control.floating { width: calc(100vw - 24px); min-width: 0; overflow-x: auto; } }
</style>

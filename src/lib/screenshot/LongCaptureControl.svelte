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
    cancelLabel?: string;
    onstatus?: (status: LongCaptureStatus) => void;
    oncancel?: () => void;
    onready?: (status: LongCaptureStatus) => void;
    onerror?: (message: string) => void;
  }

  let {
    jobId,
    api = screenshotApi,
    initialStatus = null,
    floating = false,
    monitor = true,
    keyboardShortcuts = true,
    cancelLabel = "取消长截图",
    onstatus,
    oncancel,
    onready,
    onerror,
  }: Props = $props();

  let status = $state<LongCaptureStatus | null>(null);
  let busy = $state(false);
  let error = $state("");
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  $effect(() => {
    const next = initialStatus;
    if (next) untrack(() => (status = next));
  });

  function publish(next: LongCaptureStatus): void {
    if (next.jobId !== jobId) return;
    status = next;
    busy = false;
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
    busy = false;
    error = value instanceof Error ? value.message : String(value || "长截图操作失败，请重试。");
    onerror?.(error);
  }

  async function refresh(): Promise<void> {
    try {
      const next = await api.getLongCaptureStatus(jobId);
      if (disposed) return;
      if (next) publish(next);
      else fail("长截图任务不存在或已结束。");
    } catch (value) {
      if (!disposed) fail(value);
    }
  }

  function schedulePoll(): void {
    if (pollTimer) clearTimeout(pollTimer);
    pollTimer = undefined;
    if (!monitor || !status || status.state === "ready" || status.state === "failed" || status.state === "canceled") return;
    pollTimer = setTimeout(() => {
      pollTimer = undefined;
      void refresh();
    }, 750);
  }

  type ControlAction = "pause" | "resume" | "retry" | "undo" | "finish" | "cancel";

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
    if (busy || !canRun(action)) return;
    busy = true;
    error = "";
    try {
      const next = action === "pause" ? await api.pauseLongCapture(jobId)
        : action === "resume" ? await api.resumeLongCapture(jobId)
          : action === "retry" ? await api.retryLongCapture(jobId)
            : action === "undo" ? await api.undoLongCapture(jobId)
              : action === "finish" ? await api.finishLongCapture(jobId)
                : await api.cancelLongCapture(jobId);
      publish(next);
      if (action === "cancel" && next.state !== "canceled") oncancel?.();
    } catch (value) {
      fail(value);
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
      if (pollTimer) clearTimeout(pollTimer);
      cleanups.forEach((cleanup) => cleanup());
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class:floating class="long-capture-control" role="toolbar" aria-label="长截图采集控制">
  {#if busy || !status || status.state === "preparing"}<LoaderCircle class="spin" size={17} />{/if}
  <strong>{status?.state === "failed" ? "长截图失败" : "长截图"}</strong>
  {#if status}
    <span class="stats">{status.frameCount} 帧 · {Math.round(status.height).toLocaleString()} px</span>
  {:else}
    <span class="stats">正在连接</span>
  {/if}
  {#if error || status?.state === "failed"}
    <span class="message" title={error || status?.message || "长截图失败，请返回后重新开始。"}>{error || status?.message || "长截图失败，请返回后重新开始。"}</span>
  {/if}
  <span class="separator"></span>
  {#if status?.state === "failed"}
    <button class="failed-exit" type="button" title={cancelLabel} aria-label={cancelLabel} disabled={busy} onclick={() => void run("cancel")}>
      {#if floating}<ArrowLeft size={17} />{:else}<X size={17} />{/if}<span>{cancelLabel}</span>
    </button>
  {:else}
    {#if status?.state === "capturing"}
      <button type="button" title="暂停 Space" aria-label="暂停长截图" disabled={busy} onclick={() => void run("pause")}><Pause size={17} /></button>
    {:else if status?.state === "paused"}
      <button type="button" title="继续 Space" aria-label="继续长截图" disabled={busy} onclick={() => void run("resume")}><Play size={17} /></button>
      <button type="button" title="重试当前段" aria-label="重试当前段" disabled={busy} onclick={() => void run("retry")}><RotateCcw size={17} /></button>
    {/if}
    <button type="button" title="回退上一段" aria-label="回退上一段" disabled={busy || !canRun("undo")} onclick={() => void run("undo")}><Undo2 size={17} /></button>
    <button class="finish" type="button" title="完成 Enter" aria-label="完成长截图" disabled={busy || !canRun("finish")} onclick={() => void run("finish")}><Check size={17} /></button>
    <button type="button" title={cancelLabel} aria-label={cancelLabel} disabled={busy} onclick={() => void run("cancel")}><X size={17} /></button>
  {/if}
</div>

<style>
  .long-capture-control { display: flex; box-sizing: border-box; width: 100%; height: 100%; min-height: 52px; padding: 6px 9px 6px 12px; align-items: center; gap: 8px; color: #242424; background: rgb(250 250 250 / 98%); border: 1px solid rgb(0 0 0 / 25%); cursor: default; overflow: hidden; font: 12px "Segoe UI", sans-serif; }
  .long-capture-control.floating { position: absolute; z-index: 40; top: 12px; left: 50%; width: auto; min-width: min(540px, calc(100vw - 24px)); height: 44px; min-height: 44px; max-width: calc(100vw - 24px); border-radius: 6px; box-shadow: 0 6px 20px rgb(0 0 0 / 28%); transform: translateX(-50%); }
  strong, .stats { flex: 0 0 auto; white-space: nowrap; }
  strong { font-size: 12px; }
  .stats { color: #555; }
  .message { min-width: 0; max-width: 210px; color: #8a2d22; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .separator { width: 1px; height: 24px; margin-left: auto; background: #d2d2d2; }
  button { display: grid; flex: 0 0 auto; width: 32px; height: 32px; padding: 0; place-items: center; color: #2d2d2d; background: transparent; border: 1px solid transparent; border-radius: 4px; }
  button:hover:not(:disabled) { background: #e7e7e7; border-color: #d0d0d0; }
  button:disabled { opacity: .42; }
  button.finish { color: #fff; background: #0067c0; border-color: #005a9e; }
  button.failed-exit { display: flex; width: auto; padding: 0 10px; gap: 5px; color: #fff; background: #0067c0; border-color: #005a9e; white-space: nowrap; }
  button.failed-exit:hover:not(:disabled) { background: #005a9e; border-color: #004e8c; }
  :global(.long-capture-control .spin) { animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 520px) { .message { display: none; } .long-capture-control.floating { width: calc(100vw - 24px); min-width: 0; overflow-x: auto; } }
</style>

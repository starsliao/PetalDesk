<script lang="ts">
  import { onMount } from "svelte";
  import { Copy, LoaderCircle, Save, X } from "@lucide/svelte";
  import { pinnedScreenshotApi, pngBlob, type PinnedScreenshotApi } from "$lib/screenshot";
  import { formatShortcut } from "$lib/shortcuts";

  interface Props {
    pinId?: string;
    api?: PinnedScreenshotApi;
    onclose?: () => void;
    onerror?: (message: string) => void;
  }

  interface ContextMenuState {
    x: number;
    y: number;
  }

  let { pinId, api = pinnedScreenshotApi, onclose, onerror }: Props = $props();
  let resolvedPinId = $state("");
  let imageUrl = $state("");
  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let contextMenu = $state<ContextMenuState | null>(null);
  let toast = $state("");
  let aspectRatio = 1;
  let correctingSize = false;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let resizeCleanup: (() => void) | undefined;

  function reportError(value: unknown): void {
    error = value instanceof Error ? value.message : String(value || "贴图操作失败，请重试。");
    onerror?.(error);
  }

  function showToast(message: string): void {
    toast = message;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 1600);
  }

  async function startDragging(event: PointerEvent): Promise<void> {
    if (event.button !== 0 || contextMenu || busy || !("__TAURI_INTERNALS__" in window)) return;
    event.preventDefault();
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().startDragging().catch(() => undefined);
  }

  async function startResizing(event: PointerEvent): Promise<void> {
    if (event.button !== 0 || !("__TAURI_INTERNALS__" in window)) return;
    event.preventDefault();
    event.stopPropagation();
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().startResizeDragging("SouthEast").catch(() => undefined);
  }

  function openContextMenu(event: MouseEvent): void {
    event.preventDefault();
    contextMenu = {
      x: Math.min(event.clientX, Math.max(4, window.innerWidth - 158)),
      y: Math.min(event.clientY, Math.max(4, window.innerHeight - 116)),
    };
  }

  async function copy(): Promise<void> {
    if (!resolvedPinId || busy) return;
    busy = true;
    contextMenu = null;
    try {
      await api.copy(resolvedPinId);
      showToast("已复制到剪贴板");
    } catch (value) {
      reportError(value);
    } finally {
      busy = false;
    }
  }

  async function save(): Promise<void> {
    if (!resolvedPinId || busy) return;
    busy = true;
    contextMenu = null;
    try {
      const result = await api.save(resolvedPinId);
      if (!result.canceled) showToast("截图已保存");
    } catch (value) {
      reportError(value);
    } finally {
      busy = false;
    }
  }

  async function close(): Promise<void> {
    if (!resolvedPinId || busy) return;
    busy = true;
    contextMenu = null;
    try {
      await api.close(resolvedPinId);
      onclose?.();
    } catch (value) {
      reportError(value);
      busy = false;
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      if (contextMenu) contextMenu = null;
      else void close();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
      event.preventDefault();
      void copy();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void save();
    }
  }

  function handleImageLoad(event: Event): void {
    const image = event.currentTarget as HTMLImageElement;
    aspectRatio = image.naturalWidth / Math.max(1, image.naturalHeight);
  }

  onMount(() => {
    let disposed = false;
    document.documentElement.classList.add("pinned-screenshot-page");
    document.body.classList.add("pinned-screenshot-page");
    resolvedPinId = pinId
      ?? (typeof window !== "undefined" ? new URLSearchParams(window.location.search).get("screenshotPin") ?? "" : "");
    if (!resolvedPinId) {
      loading = false;
      reportError("贴图标识缺失，无法显示截图。");
    } else {
      void api.getPng(resolvedPinId).then((bytes) => {
        if (disposed) return;
        imageUrl = URL.createObjectURL(pngBlob(bytes));
        loading = false;
      }).catch((value) => {
        if (!disposed) {
          loading = false;
          reportError(value);
        }
      });
    }

    if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      void (async () => {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const { PhysicalSize } = await import("@tauri-apps/api/dpi");
        const cleanup = await getCurrentWindow().onResized(async ({ payload }) => {
          if (disposed || correctingSize || aspectRatio <= 0) return;
          const expectedHeight = Math.max(72, Math.round(payload.width / aspectRatio));
          if (Math.abs(payload.height - expectedHeight) <= 2) return;
          correctingSize = true;
          try {
            await getCurrentWindow().setSize(new PhysicalSize(payload.width, expectedHeight));
          } finally {
            correctingSize = false;
          }
        });
        if (disposed) cleanup(); else resizeCleanup = cleanup;
      })();
    }

    return () => {
      disposed = true;
      resizeCleanup?.();
      if (toastTimer) clearTimeout(toastTimer);
      if (imageUrl) URL.revokeObjectURL(imageUrl);
      document.documentElement.classList.remove("pinned-screenshot-page");
      document.body.classList.remove("pinned-screenshot-page");
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} onclick={() => (contextMenu = null)} />

<div
  class="pinned-screenshot"
  data-testid="pinned-screenshot"
  role="application"
  aria-label="置顶截图"
  onpointerdown={startDragging}
  oncontextmenu={openContextMenu}
>
  {#if imageUrl}
    <img
      src={imageUrl}
      alt="置顶截图"
      draggable="false"
      onload={handleImageLoad}
    />
  {/if}

  {#if loading}
    <div class="pin-status"><LoaderCircle class="spin" size={22} /><span>正在加载…</span></div>
  {:else if error && !imageUrl}
    <div class="pin-status error"><span>{error}</span><button type="button" onclick={() => void close()}>关闭</button></div>
  {/if}

  {#if contextMenu}
    <div
      class="pin-menu"
      role="menu"
      tabindex="-1"
      style={`left:${contextMenu.x}px;top:${contextMenu.y}px`}
      onpointerdown={(event) => event.stopPropagation()}
      onclick={(event) => event.stopPropagation()}
      onkeydown={(event) => event.stopPropagation()}
    >
      <button type="button" role="menuitem" onclick={() => void copy()}><Copy size={16} /><span>复制</span><kbd>{formatShortcut("Ctrl+C")}</kbd></button>
      <button type="button" role="menuitem" onclick={() => void save()}><Save size={16} /><span>另存为</span><kbd>{formatShortcut("Ctrl+S")}</kbd></button>
      <span class="menu-separator"></span>
      <button type="button" role="menuitem" onclick={() => void close()}><X size={16} /><span>关闭</span><kbd>Esc</kbd></button>
    </div>
  {/if}

  {#if busy}<div class="busy"><LoaderCircle class="spin" size={17} /></div>{/if}
  {#if toast}<div class="pin-toast" role="status">{toast}</div>{/if}
  {#if error && imageUrl}<div class="pin-error" role="alert">{error}</div>{/if}
  <button class="resize-corner" type="button" aria-label="等比调整贴图大小" title="调整大小" onpointerdown={startResizing}></button>
</div>

<style>
  :global(html.pinned-screenshot-page), :global(body.pinned-screenshot-page) { width: 100%; height: 100%; padding: 0; margin: 0; overflow: hidden; background: transparent; }
  .pinned-screenshot { position: fixed; inset: 0; overflow: hidden; background: transparent; cursor: grab; user-select: none; }
  .pinned-screenshot:active { cursor: grabbing; }
  img { display: block; width: 100%; height: 100%; object-fit: fill; pointer-events: none; }
  .pin-status { position: absolute; inset: 0; display: grid; place-content: center; justify-items: center; gap: 8px; color: #fff; background: rgb(28 28 28 / 92%); font: 12px "Segoe UI", sans-serif; }
  .pin-status.error { padding: 16px; text-align: center; }
  .pin-status button { margin-top: 5px; padding: 6px 12px; border: 0; border-radius: 4px; }
  .pin-menu { position: fixed; z-index: 20; display: grid; width: 154px; padding: 4px; color: #222; background: rgb(250 250 250 / 98%); border: 1px solid rgb(0 0 0 / 20%); border-radius: 5px; box-shadow: 0 6px 20px rgb(0 0 0 / 28%); cursor: default; }
  .pin-menu button { display: grid; min-height: 31px; padding: 5px 7px; grid-template-columns: 19px 1fr auto; align-items: center; gap: 6px; color: #222; background: transparent; border: 0; border-radius: 3px; text-align: left; }
  .pin-menu button:hover { background: #e7e7e7; }
  .pin-menu kbd { color: #777; font: 10px "Segoe UI", sans-serif; }
  .menu-separator { height: 1px; margin: 3px 5px; background: #ddd; }
  .resize-corner { position: absolute; z-index: 10; right: 0; bottom: 0; width: 20px; height: 20px; padding: 0; background: linear-gradient(135deg, transparent 48%, rgb(255 255 255 / 75%) 49%, rgb(255 255 255 / 75%) 57%, transparent 58%, transparent 66%, rgb(255 255 255 / 75%) 67%, rgb(255 255 255 / 75%) 75%, transparent 76%); border: 0; cursor: nwse-resize; opacity: 0; }
  .pinned-screenshot:hover .resize-corner, .resize-corner:focus-visible { opacity: 1; }
  .busy { position: absolute; z-index: 15; top: 8px; right: 8px; display: grid; width: 30px; height: 30px; place-items: center; color: #fff; background: rgb(0 0 0 / 60%); border-radius: 4px; }
  .pin-toast, .pin-error { position: absolute; z-index: 15; bottom: 10px; left: 50%; max-width: calc(100% - 20px); padding: 6px 9px; color: #fff; background: rgb(0 0 0 / 72%); border-radius: 3px; transform: translateX(-50%); font: 11px "Segoe UI", sans-serif; white-space: nowrap; }
  .pin-error { background: rgb(150 34 27 / 92%); white-space: normal; }
  :global(.spin) { animation: spin .8s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }
</style>

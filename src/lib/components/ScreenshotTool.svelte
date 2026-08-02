<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import {
    ArrowLeft,
    ArrowRight,
    Bold,
    ChevronDown,
    Copy,
    Eraser,
    GalleryVerticalEnd,
    Highlighter,
    Italic,
    LoaderCircle,
    Pencil,
    Pin,
    Redo2,
    Save,
    ScanLine,
    Shapes,
    Trash2,
    Type,
    Underline,
    Undo2,
    X,
  } from "@lucide/svelte";
  import LongCaptureControl from "$lib/screenshot/LongCaptureControl.svelte";
  import { formatShortcut } from "$lib/shortcuts";
  import {
    DEFAULT_TOOL_SETTINGS,
    annotationBounds,
    clamp,
    clampPoint,
    clampRect,
    commitHistory,
    createHistory,
    decodePng,
    drawAnnotationSelection,
    exportSelectionPng,
    exportAnnotatedLongCapture,
    hitTestAnnotation,
    longCaptureStatusFromEvent,
    matchesScreenshotWindow,
    moveRect,
    normalizeRect,
    placeToolbar,
    redoHistory,
    renderComposite,
    resizeRect,
    roundRect,
    scaleAnnotation,
    screenshotApi,
    selectionHandlePoints,
    translateAnnotation,
    undoHistory,
    validateFrameDimensions,
    visibleLongCaptureTiles,
    type Annotation,
    type ColorFormat,
    type EffectAnnotation,
    type EraserAnnotation,
    type HistoryState,
    type LineAnnotation,
    type LongCaptureCapability,
    type LongCaptureMode,
    type LongCaptureStatus,
    type LongCaptureTileRange,
    type PathAnnotation,
    type Point,
    type Rect,
    type ResizeHandle,
    type ScreenshotApi,
    type ScreenshotExportResult,
    type ScreenshotSession,
    type ScreenshotToolName,
    type ShapeAnnotation,
    type TextAnnotation,
    type ToolSettings,
  } from "$lib/screenshot";

  interface Props {
    sessionId?: string;
    api?: ScreenshotApi;
    loadTimeoutMs?: number;
    longStartTimeoutMs?: number;
    longPollIntervalMs?: number;
    longPollTimeoutMs?: number;
    longPollRetryLimit?: number;
    oncomplete?: (result: ScreenshotExportResult) => void;
    oncancel?: () => void;
    onerror?: (message: string) => void;
  }

  type Interaction =
    | { kind: "create-selection"; pointerId: number; origin: Point }
    | { kind: "move-selection"; pointerId: number; origin: Point; initial: Rect }
    | { kind: "resize-selection"; pointerId: number; handle: ResizeHandle; initial: Rect }
    | { kind: "draw"; pointerId: number; origin: Point }
    | { kind: "move-annotation"; pointerId: number; origin: Point; initial: Annotation }
    | { kind: "resize-annotation"; pointerId: number; handle: ResizeHandle; initial: Annotation; bounds: Rect };

  interface TextDraft {
    annotation: TextAnnotation;
    value: string;
  }

  interface DoubleClickSnapshot {
    selection: Rect;
    history: HistoryState<Annotation[]>;
    previewAnnotations: Annotation[] | null;
    draft: Annotation | null;
    textDraft: TextDraft | null;
    selectedId: string | null;
    activeTool: ScreenshotToolName | null;
    point: Point;
    clientPoint: Point;
    capturedAt: number;
  }

  interface LoadedLongTile extends LongCaptureTileRange {
    image: ImageBitmap | HTMLImageElement;
  }

  interface LongTileRenderParams {
    tile: LoadedLongTile;
    width: number;
    annotations: readonly Annotation[];
    draft: Annotation | null;
    selectedId: string | null;
  }

  type LongPreviewInteraction =
    | { kind: "draw"; pointerId: number; origin: Point }
    | { kind: "move"; pointerId: number; origin: Point; initial: Annotation };

  let {
    sessionId,
    api = screenshotApi,
    loadTimeoutMs = 8_000,
    longStartTimeoutMs = 8_000,
    longPollIntervalMs = 750,
    longPollTimeoutMs = 3_000,
    longPollRetryLimit = 3,
    oncomplete,
    oncancel,
    onerror,
  }: Props = $props();

  const colors = ["#ff3b30", "#ff9500", "#ffcc00", "#34c759", "#00a7d6", "#0a84ff", "#5856d6", "#af52de", "#ffffff", "#111111"];
  const handles: ResizeHandle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let session = $state<ScreenshotSession | null>(null);
  let sourceImage = $state<ImageBitmap | HTMLImageElement | null>(null);
  let sourcePng: Uint8Array | null = null;
  let selection = $state<Rect | null>(null);
  let history = $state<HistoryState<Annotation[]>>(createHistory([]));
  let previewAnnotations = $state<Annotation[] | null>(null);
  let draft = $state<Annotation | null>(null);
  let textDraft = $state<TextDraft | null>(null);
  let selectedId = $state<string | null>(null);
  let activeTool = $state<ScreenshotToolName | null>(null);
  let interaction = $state<Interaction | null>(null);
  let toolSettings = $state<ToolSettings>({ ...DEFAULT_TOOL_SETTINGS });
  let colorFormat = $state<ColorFormat>("hex");
  let hoverPoint = $state<Point | null>(null);
  let hoverCss = $state<Point | null>(null);
  let sampledColor = $state({ r: 0, g: 0, b: 0 });
  let viewportWidth = $state(0);
  let viewportHeight = $state(0);
  let toolbarWidth = $state(720);
  let toolbarHeight = $state(92);
  let toast = $state("");
  let longCapability = $state<LongCaptureCapability | null>(null);
  let longCapabilityLoaded = $state(false);
  let longConfirmOpen = $state(false);
  let longModeMenuOpen = $state(false);
  let pendingLongMode = $state<LongCaptureMode>("manual");
  let choosingScrollAnchor = $state(false);
  let longStatus = $state<LongCaptureStatus | null>(null);
  let longBusy = $state(false);
  let longPreviewScrollTop = $state(0);
  let longTiles = $state<Record<number, LoadedLongTile>>({});
  let longHistory = $state<HistoryState<Annotation[]>>(createHistory([]));
  let longPreviewAnnotations = $state<Annotation[] | null>(null);
  let longDraft = $state<Annotation | null>(null);
  let longTextDraft = $state<TextDraft | null>(null);
  let longSelectedId = $state<string | null>(null);
  let longActiveTool = $state<ScreenshotToolName | null>(null);
  let longInteraction = $state<LongPreviewInteraction | null>(null);

  const frameBounds = $derived<Rect>({ x: 0, y: 0, width: session?.frameWidth ?? 0, height: session?.frameHeight ?? 0 });
  const annotations = $derived(previewAnnotations ?? history.present);
  const selectionLocked = $derived(history.present.length > 0 || draft !== null || textDraft !== null);
  const selectedAnnotation = $derived(selectedId ? annotations.find((item) => item.id === selectedId) ?? null : null);
  const selectedBounds = $derived(selectedAnnotation ? annotationBounds(selectedAnnotation) : null);
  const longCaptureActive = $derived(choosingScrollAnchor || longConfirmOpen || longStatus !== null || longBusy);
  const selectionMoveCursor = $derived(
    !loading
      && !busy
      && !longCaptureActive
      && !selectionLocked
      && !activeTool
      && (interaction?.kind === "move-selection"
        || (!interaction && hoverPoint !== null && hitTestSelection(hoverPoint))),
  );
  const longPreviewScale = $derived(longStatus?.state === "ready" && longStatus.width > 0
    ? Math.min(1, Math.max(0.05, (viewportWidth - 48) / longStatus.width))
    : 1);
  const longVisibleTiles = $derived(longStatus?.state === "ready"
    ? visibleLongCaptureTiles({
      totalHeight: longStatus.height,
      scrollTop: longPreviewScrollTop,
      viewportHeight: Math.max(1, viewportHeight - 58),
      scale: longPreviewScale,
    })
    : []);
  const longAnnotations = $derived(longPreviewAnnotations ?? longHistory.present);

  let stageElement = $state<HTMLDivElement>(undefined!);
  let longModeFirstButton = $state<HTMLButtonElement>(undefined!);
  let longConfirmKeepButton = $state<HTMLButtonElement>(undefined!);
  let displayCanvas = $state<HTMLCanvasElement>(undefined!);
  let sourceCanvas = $state<HTMLCanvasElement>(undefined!);
  let magnifierCanvas = $state<HTMLCanvasElement>(undefined!);
  let toolbarElement = $state<HTMLDivElement>(undefined!);
  let textAreaElement = $state<HTMLTextAreaElement>(undefined!);
  let longPreviewElement = $state<HTMLDivElement>(undefined!);
  let longTextAreaElement = $state<HTMLTextAreaElement>(undefined!);
  let renderFrame: number | undefined;
  let preferenceTimer: ReturnType<typeof setTimeout> | undefined;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let longPollTimer: ReturnType<typeof setTimeout> | undefined;
  let geometryValidationTimer: ReturnType<typeof setTimeout> | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let scheduleWindowGeometryValidation: (() => void) | undefined;
  let doubleClickSnapshot: DoubleClickSnapshot | null = null;
  let doubleClickSnapshotTimer: ReturnType<typeof setTimeout> | undefined;
  let loadGeneration = 0;
  let componentDisposed = false;
  let invalidatingSessionId: string | null = null;
  let pendingSessionId: string | null = null;
  let loadInFlight: Promise<void> | null = null;
  let loadQueued = false;
  let queuedSessionId: string | undefined;
  let longTileGeneration = 0;
  let longStartGeneration = 0;
  let longActiveJobId: string | null = null;
  let longPollFailures = 0;
  const longTilePending = new Set<number>();
  const longDesiredTileYs = new Set<number>();

  const doubleClickWindowMs = 900;
  const doubleClickDistance = 8;

  class ScreenshotLoadTimeoutError extends Error {}

  function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
    const duration = Math.max(10, timeoutMs);
    return new Promise<T>((resolve, reject) => {
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        reject(new ScreenshotLoadTimeoutError(message));
      }, duration);
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

  function id(): string {
    return typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `annotation-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  }

  function reportError(value: unknown): void {
    const message = value instanceof Error ? value.message : String(value || "截图操作失败，请重试。");
    error = message;
    onerror?.(message);
  }

  function showToast(message: string): void {
    toast = message;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 1800);
  }

  async function loadLongCaptureCapability(): Promise<void> {
    if (!api.getLongCaptureCapability) {
      longCapability = { available: false, reason: "当前版本未提供长截图采集接口。" };
      longCapabilityLoaded = true;
      return;
    }
    try {
      longCapability = await api.getLongCaptureCapability();
    } catch (value) {
      longCapability = {
        available: false,
        reason: value instanceof Error ? value.message : "无法检查长截图可用性。",
      };
    } finally {
      longCapabilityLoaded = true;
    }
  }

  function clearLongTiles(): void {
    longTileGeneration += 1;
    longTilePending.clear();
    longDesiredTileYs.clear();
    for (const tile of Object.values(longTiles)) {
      if ("close" in tile.image && typeof tile.image.close === "function") tile.image.close();
    }
    longTiles = {};
  }

  function resetLongAnnotations(): void {
    longHistory = createHistory([]);
    longPreviewAnnotations = null;
    longDraft = null;
    longTextDraft = null;
    longSelectedId = null;
    longActiveTool = null;
    longInteraction = null;
  }

  function resetLongCapture(): void {
    longStartGeneration += 1;
    longActiveJobId = null;
    longPollFailures = 0;
    longModeMenuOpen = false;
    pendingLongMode = "manual";
    choosingScrollAnchor = false;
    longConfirmOpen = false;
    longBusy = false;
    longStatus = null;
    longPreviewScrollTop = 0;
    resetLongAnnotations();
    if (longPollTimer) clearTimeout(longPollTimer);
    longPollTimer = undefined;
    clearLongTiles();
    void tick().then(() => stageElement?.focus());
  }

  function longCaptureJobIsGone(value: unknown): boolean {
    const message = value instanceof Error ? value.message : String(value ?? "");
    return /任务.*(?:不存在|已被替换|已结束)|(?:不存在|已被替换|已结束).*任务/.test(message);
  }

  function recoverFromLongStatusFailure(jobId: string, message: string): void {
    if (longActiveJobId !== jobId) return;
    const activeSessionId = session?.id;
    resetLongCapture();
    reportError(message);
    if (!activeSessionId) return;
    // Recover by owner rather than by the stale job id. The backend treats
    // this as an idempotent request and also restores a hidden capture window
    // when the long job has already disappeared.
    void withTimeout(
      api.cancelLongCaptureSession(activeSessionId),
      5_000,
      "恢复普通截图超时。",
    ).catch(reportError);
  }

  function scheduleLongStatusPoll(): void {
    if (longPollTimer) clearTimeout(longPollTimer);
    longPollTimer = undefined;
    const status = longStatus;
    const currentJobId = longActiveJobId;
    const generation = longStartGeneration;
    if (!status || !currentJobId || status.jobId !== currentJobId || !api.getLongCaptureStatus
      || status.state === "ready" || status.state === "failed" || status.state === "canceled") return;
    longPollTimer = setTimeout(() => {
      longPollTimer = undefined;
      const statusRequest = api.getLongCaptureStatus?.(currentJobId);
      if (!statusRequest) return;
      void withTimeout(statusRequest, longPollTimeoutMs, "读取长截图状态超时。").then((next) => {
        if (generation !== longStartGeneration || longActiveJobId !== currentJobId) return;
        longPollFailures = 0;
        if (next) applyLongCaptureStatus(next, generation);
        else {
          recoverFromLongStatusFailure(currentJobId, "长截图任务已结束，请重新开始。");
        }
      }).catch((value) => {
        if (generation !== longStartGeneration || longActiveJobId !== currentJobId) return;
        if (longCaptureJobIsGone(value)) {
          recoverFromLongStatusFailure(currentJobId, "长截图任务已失效，请重新开始。");
          return;
        }
        longPollFailures += 1;
        if (longPollFailures >= Math.max(1, longPollRetryLimit)) {
          recoverFromLongStatusFailure(currentJobId, "无法连接长截图任务，已恢复普通截图。");
          return;
        }
        scheduleLongStatusPoll();
      });
    }, Math.max(10, longPollIntervalMs));
  }

  function applyLongCaptureStatus(next: LongCaptureStatus, generation = longStartGeneration): void {
    if (generation !== longStartGeneration || !longActiveJobId || next.jobId !== longActiveJobId) return;
    if (!session || next.sessionId !== session.id) return;
    if ((longStatus?.state === "ready" || longStatus?.state === "failed")
      && next.state !== longStatus.state && next.state !== "canceled") return;
    if (next.state === "canceled") {
      resetLongCapture();
      return;
    }
    const enteringPreview = next.state === "ready" && longStatus?.state !== "ready";
    longStatus = next;
    if (enteringPreview) {
      longPreviewScrollTop = 0;
      resetLongAnnotations();
      void tick().then(() => {
        if (longPreviewElement) longPreviewElement.scrollTop = 0;
      });
    }
    scheduleLongStatusPoll();
  }

  function longCaptureEventJobId(payload: unknown): string | null {
    if (!payload || typeof payload !== "object") return null;
    const record = payload as Record<string, unknown>;
    const value = record.status && typeof record.status === "object"
      ? record.status as Record<string, unknown>
      : record;
    return typeof value.jobId === "string" ? value.jobId : null;
  }

  function handleLongCaptureEvent(payload: unknown, fallbackState?: LongCaptureStatus["state"]): void {
    const currentJobId = longActiveJobId;
    const generation = longStartGeneration;
    if (!currentJobId || longCaptureEventJobId(payload) !== currentJobId || longStatus?.jobId !== currentJobId) return;
    const next = longCaptureStatusFromEvent(payload, longStatus, fallbackState);
    if (!next || next.jobId !== currentJobId) return;
    applyLongCaptureStatus(next, generation);
  }

  function requestLongCapture(mode: LongCaptureMode = "manual"): void {
    if (!selection || selection.width < 1 || selection.height < 1) return;
    if (!longCapabilityLoaded || !longCapability?.available) {
      showToast(longCapability?.reason || "长截图暂不可用");
      return;
    }
    pendingLongMode = mode;
    longModeMenuOpen = false;
    finishText(true);
    if (history.present.length > 0 || draft) {
      longConfirmOpen = true;
      void tick().then(() => longConfirmKeepButton?.focus());
      return;
    }
    activeTool = null;
    selectedId = null;
    hoverPoint = null;
    hoverCss = null;
    beginLongCapture();
  }

  function toggleLongModeMenu(): void {
    if (!longCapabilityLoaded || !longCapability?.available) return;
    longModeMenuOpen = !longModeMenuOpen;
    activeTool = null;
    selectedId = null;
    void tick().then(() => {
      updateViewport();
      if (longModeMenuOpen) longModeFirstButton?.focus();
    });
  }

  function dismissLongCapturePreparation(): void {
    longModeMenuOpen = false;
    pendingLongMode = "manual";
    choosingScrollAnchor = false;
    longConfirmOpen = false;
    void tick().then(() => stageElement?.focus());
  }

  function confirmLongCapture(): void {
    history = createHistory([]);
    previewAnnotations = null;
    draft = null;
    textDraft = null;
    selectedId = null;
    activeTool = null;
    longConfirmOpen = false;
    beginLongCapture();
  }

  function beginLongCapture(): void {
    if (pendingLongMode === "manual") {
      void startLongCapture();
      return;
    }
    choosingScrollAnchor = true;
    void tick().then(() => stageElement?.focus());
  }

  async function startLongCapture(selectedAnchor?: Point): Promise<void> {
    const activeSession = session;
    const activeSelection = selection;
    if (!activeSession || !activeSelection || !api.startLongCapture) return;
    const roundedSelection = roundRect(activeSelection);
    const scrollAnchor = selectedAnchor
      ? { x: Math.round(selectedAnchor.x), y: Math.round(selectedAnchor.y) }
      : {
          x: roundedSelection.x + Math.floor(roundedSelection.width / 2),
          y: roundedSelection.y + Math.floor(roundedSelection.height / 2),
        };
    const generation = ++longStartGeneration;
    choosingScrollAnchor = false;
    longModeMenuOpen = false;
    longBusy = true;
    error = "";
    const request = api.startLongCapture({
      sessionId: activeSession.id,
      selection: roundedSelection,
      scrollAnchor,
      scope: "selection",
      mode: pendingLongMode,
    });
    void request.then((lateStatus) => {
      if (generation !== longStartGeneration) {
        void api.cancelLongCapture(lateStatus.jobId).catch(() => undefined);
      }
    }).catch(() => undefined);
    try {
      const status = await withTimeout(request, longStartTimeoutMs, "长截图启动超时，已恢复普通截图。");
      if (generation !== longStartGeneration) {
        return;
      }
      longActiveJobId = status.jobId;
      longBusy = false;
      applyLongCaptureStatus(status, generation);
    } catch (value) {
      if (generation !== longStartGeneration) return;
      longStartGeneration += 1;
      try {
        await withTimeout(
          api.cancelLongCaptureSession(activeSession.id),
          2_500,
          "恢复普通截图超时。",
        );
      } catch {
        // The regular screenshot session remains available even if no long job
        // had been registered yet. A late start response is canceled above.
      }
      resetLongCapture();
      reportError(value);
    }
  }

  async function cancelLongCaptureToEditor(): Promise<void> {
    const status = longStatus;
    const startPending = longBusy;
    const activeSessionId = session?.id;
    if (!status && !startPending) return resetLongCapture();
    error = "";
    // Recover the editor and unmount its controller before awaiting native IPC.
    // This invalidates pending start/control responses and also debounces a
    // second context-menu cancellation while the first request is in flight.
    resetLongCapture();
    try {
      if (status) {
        await withTimeout(api.cancelLongCapture(status.jobId), 5_000, "取消长截图超时。");
      } else if (startPending && activeSessionId) {
        await withTimeout(api.cancelLongCaptureSession(activeSessionId), 5_000, "取消长截图启动超时。");
      }
    } catch (value) {
      reportError(value);
    }
  }

  async function exportLongImage(action: "copy" | "save" | "pin"): Promise<void> {
    const status = longStatus;
    if (!status || status.state !== "ready" || longBusy) return;
    if (!api.exportLongCapture) {
      reportError("当前版本未提供长截图导出接口。");
      return;
    }
    longBusy = true;
    error = "";
    const generation = longStartGeneration;
    try {
      finishLongText(true);
      const result = longHistory.present.length > 0
        ? await exportAnnotatedLongCapture(
          api,
          status.jobId,
          action,
          status.width,
          status.height,
          longHistory.present,
        )
        : await api.exportLongCapture(status.jobId, action);
      if (generation === longStartGeneration && longActiveJobId === status.jobId && !result.canceled) oncomplete?.(result);
    } catch (value) {
      if (generation === longStartGeneration && longActiveJobId === status.jobId) reportError(value);
    } finally {
      if (generation === longStartGeneration && longActiveJobId === status.jobId) longBusy = false;
    }
  }

  async function loadLongTile(jobId: string, range: LongCaptureTileRange, generation: number): Promise<void> {
    if (!api.getLongCaptureTile || longTilePending.has(range.y)) return;
    longTilePending.add(range.y);
    try {
      const png = await api.getLongCaptureTile(jobId, range.y, range.height);
      const image = await decodePng(png);
      if (generation !== longTileGeneration
        || longStatus?.jobId !== jobId
        || longStatus.state !== "ready"
        || !longDesiredTileYs.has(range.y)) {
        if ("close" in image && typeof image.close === "function") image.close();
        return;
      }
      longTiles = { ...longTiles, [range.y]: { ...range, image } };
    } catch (value) {
      if (generation === longTileGeneration && longStatus?.jobId === jobId) reportError(value);
    } finally {
      longTilePending.delete(range.y);
    }
  }

  function syncLongTiles(status: LongCaptureStatus, ranges: LongCaptureTileRange[]): void {
    const keep = new Set(ranges.map((range) => range.y));
    longDesiredTileYs.clear();
    for (const y of keep) longDesiredTileYs.add(y);
    let changed = false;
    const retained: Record<number, LoadedLongTile> = {};
    for (const [key, tile] of Object.entries(longTiles)) {
      if (keep.has(Number(key))) retained[Number(key)] = tile;
      else {
        if ("close" in tile.image && typeof tile.image.close === "function") tile.image.close();
        changed = true;
      }
    }
    if (changed) longTiles = retained;
    const generation = longTileGeneration;
    for (const range of ranges) {
      if (!retained[range.y] && !longTiles[range.y]) void loadLongTile(status.jobId, range, generation);
    }
  }

  function renderLongTile(node: HTMLCanvasElement, initial: LongTileRenderParams) {
    let params = initial;
    let frame: number | undefined;
    const paint = (): void => {
      frame = undefined;
      const { tile, width, annotations: committed, draft: pending, selectedId: selected } = params;
      if (node.width !== width) node.width = width;
      if (node.height !== tile.height) node.height = tile.height;
      const context = node.getContext("2d", { willReadFrequently: true });
      if (!context) return;
      const candidates = pending ? [...committed, pending] : [...committed];
      const visible = candidates.filter((annotation) => {
        const bounds = annotationBounds(annotation);
        return bounds.y + bounds.height >= tile.y && bounds.y <= tile.y + tile.height;
      });
      const shifted = visible.map((annotation) => translateAnnotation(annotation, { x: 0, y: -tile.y }));
      renderComposite(context, tile.image, width, tile.height, shifted);
      const selectedAnnotation = selected ? visible.find((annotation) => annotation.id === selected) : null;
      if (selectedAnnotation) drawAnnotationSelection(
        context,
        translateAnnotation(selectedAnnotation, { x: 0, y: -tile.y }),
      );
    };
    const schedule = (): void => {
      if (frame !== undefined) cancelAnimationFrame(frame);
      frame = requestAnimationFrame(paint);
    };
    schedule();
    return {
      update(next: LongTileRenderParams) {
        params = next;
        schedule();
      },
      destroy() {
        if (frame !== undefined) cancelAnimationFrame(frame);
      },
    };
  }

  $effect(() => {
    const status = longStatus;
    const ranges = longVisibleTiles;
    if (status?.state === "ready") untrack(() => syncLongTiles(status, ranges));
  });

  function stagePoint(event: Pick<MouseEvent, "clientX" | "clientY">): Point {
    if (!session) return { x: 0, y: 0 };
    const rect = stageElement.getBoundingClientRect();
    return clampPoint({
      x: (event.clientX - rect.left) / Math.max(1, rect.width) * session.frameWidth,
      y: (event.clientY - rect.top) / Math.max(1, rect.height) * session.frameHeight,
    }, frameBounds);
  }

  function selectionPoint(point: Point): Point {
    return selection ? clampPoint(point, selection) : point;
  }

  function pointerCss(event: PointerEvent): Point {
    const rect = stageElement.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  function isUiTarget(target: EventTarget | null): boolean {
    return target instanceof Element
      && !!target.closest(".ui-layer, .text-editor, .resize-handle, .annotation-handle");
  }

  function clearDoubleClickSnapshot(): void {
    doubleClickSnapshot = null;
    if (doubleClickSnapshotTimer) clearTimeout(doubleClickSnapshotTimer);
    doubleClickSnapshotTimer = undefined;
  }

  function rememberDoubleClickSnapshot(event: PointerEvent, point: Point): void {
    if (!selection) {
      clearDoubleClickSnapshot();
      return;
    }
    const now = performance.now();
    const clientPoint = { x: event.clientX, y: event.clientY };
    const previous = doubleClickSnapshot;
    const continuesPreviousClick = !!previous
      && now - previous.capturedAt <= doubleClickWindowMs
      && Math.hypot(clientPoint.x - previous.clientPoint.x, clientPoint.y - previous.clientPoint.y) <= doubleClickDistance;

    if (!continuesPreviousClick) {
      doubleClickSnapshot = {
        selection: { ...selection },
        history,
        previewAnnotations,
        draft,
        textDraft,
        selectedId,
        activeTool,
        point,
        clientPoint,
        capturedAt: now,
      };
    }
    if (doubleClickSnapshotTimer) clearTimeout(doubleClickSnapshotTimer);
    doubleClickSnapshotTimer = setTimeout(clearDoubleClickSnapshot, doubleClickWindowMs);
  }

  function restoreDoubleClickSnapshot(snapshot: DoubleClickSnapshot): void {
    selection = { ...snapshot.selection };
    history = snapshot.history;
    previewAnnotations = snapshot.previewAnnotations;
    draft = snapshot.draft;
    textDraft = snapshot.textDraft;
    selectedId = snapshot.selectedId;
    activeTool = snapshot.activeTool;
    interaction = null;
  }

  function updateViewport(): void {
    if (!stageElement) return;
    viewportWidth = stageElement.clientWidth;
    viewportHeight = stageElement.clientHeight;
    if (toolbarElement) {
      toolbarWidth = toolbarElement.offsetWidth;
      toolbarHeight = toolbarElement.offsetHeight;
    }
  }

  function renderNow(): void {
    renderFrame = undefined;
    if (!displayCanvas || !sourceImage || !session) return;
    if (displayCanvas.width !== session.frameWidth) displayCanvas.width = session.frameWidth;
    if (displayCanvas.height !== session.frameHeight) displayCanvas.height = session.frameHeight;
    const context = displayCanvas.getContext("2d", { willReadFrequently: true });
    if (!context) return;
    renderComposite(
      context,
      sourceImage,
      session.frameWidth,
      session.frameHeight,
      draft ? [...annotations, draft] : annotations,
    );
  }

  function scheduleRender(): void {
    if (renderFrame !== undefined) cancelAnimationFrame(renderFrame);
    renderFrame = requestAnimationFrame(renderNow);
  }

  $effect(() => {
    annotations;
    draft;
    sourceImage;
    scheduleRender();
  });

  function commitAnnotations(next: Annotation[]): void {
    history = commitHistory(history, next);
    previewAnnotations = null;
  }

  function updateAnnotation(next: Annotation): Annotation[] {
    return history.present.map((annotation) => annotation.id === next.id ? next : annotation);
  }

  function undo(): void {
    if (busy) return;
    if (textDraft) {
      textDraft = null;
      draft = null;
      return;
    }
    history = undoHistory(history);
    if (history.present.length === 0) activeTool = null;
    selectedId = null;
    previewAnnotations = null;
  }

  function redo(): void {
    if (busy || textDraft) return;
    history = redoHistory(history);
    selectedId = null;
    previewAnnotations = null;
  }

  function removeSelected(): void {
    if (!selectedId || busy) return;
    commitAnnotations(history.present.filter((annotation) => annotation.id !== selectedId));
    selectedId = null;
    if (history.present.length === 0) activeTool = null;
  }

  function persistPreferences(): void {
    if (preferenceTimer) clearTimeout(preferenceTimer);
    preferenceTimer = setTimeout(() => {
      void api.saveToolSettings({ ...toolSettings }, colorFormat).catch(() => undefined);
    }, 350);
  }

  function styleSelected(): void {
    if (!selectedAnnotation) return;
    let next: Annotation = selectedAnnotation;
    if (next.kind === "shape") {
      next = {
        ...next,
        shape: toolSettings.shape,
        stroke: { color: toolSettings.strokeColor, width: toolSettings.strokeWidth, lineStyle: toolSettings.lineStyle },
        fill: toolSettings.fillColor,
      };
    } else if (next.kind === "line") {
      next = {
        ...next,
        line: toolSettings.line,
        stroke: { color: toolSettings.strokeColor, width: toolSettings.strokeWidth, lineStyle: toolSettings.lineStyle },
      };
    } else if (next.kind === "pencil") {
      next = { ...next, color: toolSettings.strokeColor, width: toolSettings.pencilWidth };
    } else if (next.kind === "marker") {
      next = { ...next, color: toolSettings.strokeColor, width: toolSettings.markerWidth, tip: toolSettings.markerTip };
    } else if (next.kind === "text") {
      next = {
        ...next,
        color: toolSettings.textColor,
        fontFamily: toolSettings.fontFamily,
        fontSize: toolSettings.fontSize,
        bold: toolSettings.textBold,
        italic: toolSettings.textItalic,
        underline: toolSettings.textUnderline,
      };
    } else if (next.kind === "effect") {
      next = { ...next, effect: toolSettings.effect, intensity: toolSettings.effectIntensity };
    }
    commitAnnotations(updateAnnotation(next));
  }

  function updateSettings(patch: Partial<ToolSettings>, applyToSelection = true): void {
    toolSettings = { ...toolSettings, ...patch };
    persistPreferences();
    if (applyToSelection) styleSelected();
  }

  function activateTool(tool: ScreenshotToolName): void {
    longModeMenuOpen = false;
    activeTool = tool;
    selectedId = null;
    void tick().then(updateViewport);
  }

  function createDraft(tool: ScreenshotToolName, point: Point): Annotation {
    const rect = { x: point.x, y: point.y, width: 0, height: 0 };
    if (tool === "shape") {
      return {
        id: id(), kind: "shape", shape: toolSettings.shape, rect,
        stroke: { color: toolSettings.strokeColor, width: toolSettings.strokeWidth, lineStyle: toolSettings.lineStyle },
        fill: toolSettings.fillColor,
      } satisfies ShapeAnnotation;
    }
    if (tool === "line") {
      return {
        id: id(), kind: "line", line: toolSettings.line, from: point, to: point,
        stroke: { color: toolSettings.strokeColor, width: toolSettings.strokeWidth, lineStyle: toolSettings.lineStyle },
      } satisfies LineAnnotation;
    }
    if (tool === "pencil" || tool === "marker") {
      return {
        id: id(), kind: tool, points: [point], color: toolSettings.strokeColor,
        width: tool === "pencil" ? toolSettings.pencilWidth : toolSettings.markerWidth,
        opacity: tool === "marker" ? 0.42 : 1,
        tip: tool === "marker" ? toolSettings.markerTip : "round",
      } satisfies PathAnnotation;
    }
    if (tool === "effect") {
      return {
        id: id(), kind: "effect", effect: toolSettings.effect, mode: toolSettings.effectMode,
        rect, points: [point], size: toolSettings.effectSize, intensity: toolSettings.effectIntensity,
      } satisfies EffectAnnotation;
    }
    if (tool === "eraser") {
      return {
        id: id(), kind: "eraser", mode: toolSettings.eraserMode,
        rect, points: [point], size: toolSettings.eraserSize,
      } satisfies EraserAnnotation;
    }
    return {
      id: id(), kind: "text", rect,
      text: "", color: toolSettings.textColor, fontFamily: toolSettings.fontFamily,
      fontSize: toolSettings.fontSize, bold: toolSettings.textBold,
      italic: toolSettings.textItalic, underline: toolSettings.textUnderline,
    } satisfies TextAnnotation;
  }

  function validDraft(annotation: Annotation): boolean {
    if (annotation.kind === "line") return Math.hypot(annotation.to.x - annotation.from.x, annotation.to.y - annotation.from.y) >= 2;
    if (annotation.kind === "pencil" || annotation.kind === "marker") return annotation.points.length > 0;
    if (annotation.kind === "effect" || annotation.kind === "eraser") {
      return annotation.mode === "brush" ? annotation.points.length > 0 : annotation.rect.width >= 2 && annotation.rect.height >= 2;
    }
    return "rect" in annotation && annotation.rect.width >= 2 && annotation.rect.height >= 2;
  }

  function hitAnnotation(point: Point): Annotation | null {
    for (let index = history.present.length - 1; index >= 0; index -= 1) {
      const annotation = history.present[index];
      if ((annotation.kind === "shape" || annotation.kind === "line" || annotation.kind === "pencil" || annotation.kind === "marker" || annotation.kind === "text")
        && hitTestAnnotation(annotation, point)) return annotation;
    }
    return null;
  }

  function commitLongAnnotations(next: Annotation[]): void {
    longHistory = commitHistory(longHistory, next);
    longPreviewAnnotations = null;
  }

  function activateLongTool(tool: ScreenshotToolName): void {
    finishLongText(true);
    longActiveTool = longActiveTool === tool ? null : tool;
    longSelectedId = null;
  }

  function longPreviewPoint(event: Pick<PointerEvent, "clientX" | "clientY" | "currentTarget">): Point {
    const status = longStatus;
    if (!status) return { x: 0, y: 0 };
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    return clampPoint({
      x: (event.clientX - rect.left) / Math.max(0.01, longPreviewScale),
      y: (event.clientY - rect.top) / Math.max(0.01, longPreviewScale),
    }, { x: 0, y: 0, width: status.width, height: status.height });
  }

  function hitLongAnnotation(point: Point): Annotation | null {
    for (let index = longHistory.present.length - 1; index >= 0; index -= 1) {
      const annotation = longHistory.present[index];
      if (hitTestAnnotation(annotation, point)) return annotation;
      const bounds = annotationBounds(annotation);
      if (point.x >= bounds.x - 5 && point.x <= bounds.x + bounds.width + 5
        && point.y >= bounds.y - 5 && point.y <= bounds.y + bounds.height + 5) return annotation;
    }
    return null;
  }

  function translateLongInside(annotation: Annotation, delta: Point): Annotation {
    const status = longStatus;
    if (!status) return annotation;
    const bounds = annotationBounds(annotation);
    return translateAnnotation(annotation, {
      x: clamp(delta.x, -bounds.x, status.width - bounds.x - bounds.width),
      y: clamp(delta.y, -bounds.y, status.height - bounds.y - bounds.height),
    });
  }

  function updateLongDraft(point: Point): void {
    if (!longDraft || longInteraction?.kind !== "draw") return;
    if (longDraft.kind === "shape" || longDraft.kind === "text") {
      longDraft = { ...longDraft, rect: roundRect(normalizeRect(longInteraction.origin, point)) };
    } else if (longDraft.kind === "line") {
      longDraft = { ...longDraft, to: point };
    } else if (longDraft.kind === "pencil" || longDraft.kind === "marker") {
      const last = longDraft.points.at(-1)!;
      if (Math.hypot(point.x - last.x, point.y - last.y) >= 1.5) longDraft = { ...longDraft, points: [...longDraft.points, point] };
    } else if ((longDraft.kind === "effect" || longDraft.kind === "eraser") && longDraft.mode === "rectangle") {
      longDraft = { ...longDraft, rect: roundRect(normalizeRect(longInteraction.origin, point)) };
    } else {
      const last = longDraft.points.at(-1)!;
      if (Math.hypot(point.x - last.x, point.y - last.y) >= 1.5) longDraft = { ...longDraft, points: [...longDraft.points, point] };
    }
  }

  function handleLongPointerDown(event: PointerEvent): void {
    if (!longStatus || longStatus.state !== "ready" || longBusy || longTextDraft || event.button !== 0) return;
    const point = longPreviewPoint(event);
    const element = event.currentTarget as HTMLElement;
    element.setPointerCapture(event.pointerId);
    if (longActiveTool) {
      longDraft = createDraft(longActiveTool, point);
      longInteraction = { kind: "draw", pointerId: event.pointerId, origin: point };
      return;
    }
    const hit = hitLongAnnotation(point);
    if (!hit) {
      longSelectedId = null;
      return;
    }
    longSelectedId = hit.id;
    longPreviewAnnotations = [...longHistory.present];
    longInteraction = { kind: "move", pointerId: event.pointerId, origin: point, initial: hit };
  }

  function handleLongPointerMove(event: PointerEvent): void {
    if (!longInteraction || longInteraction.pointerId !== event.pointerId) return;
    const point = longPreviewPoint(event);
    if (longInteraction.kind === "draw") updateLongDraft(point);
    else {
      const moved = translateLongInside(longInteraction.initial, {
        x: point.x - longInteraction.origin.x,
        y: point.y - longInteraction.origin.y,
      });
      longPreviewAnnotations = longHistory.present.map((annotation) => annotation.id === moved.id ? moved : annotation);
    }
  }

  function beginLongTextEditing(annotation: TextAnnotation): void {
    const status = longStatus;
    if (!status) return;
    let rect = annotation.rect;
    if (rect.width < 20 || rect.height < 20) {
      rect = clampRect({ x: rect.x, y: rect.y, width: 280, height: 110 }, {
        x: 0, y: 0, width: status.width, height: status.height,
      }, 20);
    }
    longTextDraft = { annotation: { ...annotation, rect }, value: "" };
    longDraft = null;
    void tick().then(() => longTextAreaElement?.focus());
  }

  function finishLongText(save = true): void {
    if (!longTextDraft) return;
    const value = longTextDraft.value.trimEnd();
    if (save && value.trim()) {
      const annotation = { ...longTextDraft.annotation, text: value };
      commitLongAnnotations([...longHistory.present, annotation]);
      longSelectedId = annotation.id;
    }
    longTextDraft = null;
    longDraft = null;
  }

  function handleLongPointerUp(event: PointerEvent): void {
    if (!longInteraction || longInteraction.pointerId !== event.pointerId) return;
    const element = event.currentTarget as HTMLElement;
    if (element.hasPointerCapture(event.pointerId)) element.releasePointerCapture(event.pointerId);
    if (longInteraction.kind === "draw" && longDraft) {
      if (longDraft.kind === "text") beginLongTextEditing(longDraft);
      else if (validDraft(longDraft)) {
        commitLongAnnotations([...longHistory.present, longDraft]);
        longSelectedId = longDraft.kind === "effect" || longDraft.kind === "eraser" ? null : longDraft.id;
        longDraft = null;
      } else longDraft = null;
    } else if (longInteraction.kind === "move" && longPreviewAnnotations) {
      commitLongAnnotations(longPreviewAnnotations);
    }
    longInteraction = null;
  }

  function handleLongPointerCancel(): void {
    longDraft = null;
    longPreviewAnnotations = null;
    longInteraction = null;
  }

  function undoLongAnnotation(): void {
    finishLongText(false);
    longHistory = undoHistory(longHistory);
    longSelectedId = null;
    longPreviewAnnotations = null;
  }

  function redoLongAnnotation(): void {
    finishLongText(false);
    longHistory = redoHistory(longHistory);
    longSelectedId = null;
    longPreviewAnnotations = null;
  }

  function removeLongSelected(): void {
    if (!longSelectedId) return;
    commitLongAnnotations(longHistory.present.filter((annotation) => annotation.id !== longSelectedId));
    longSelectedId = null;
  }

  function startSelectionResize(event: PointerEvent, handle: ResizeHandle): void {
    if (!selection || busy || textDraft) return;
    event.preventDefault();
    event.stopPropagation();
    stageElement.setPointerCapture(event.pointerId);
    interaction = { kind: "resize-selection", pointerId: event.pointerId, handle, initial: selection };
  }

  function startAnnotationResize(event: PointerEvent, handle: ResizeHandle): void {
    if (!selectedAnnotation || !selectedBounds || busy) return;
    event.preventDefault();
    event.stopPropagation();
    const bounds = {
      x: selectedBounds.x,
      y: selectedBounds.y,
      width: Math.max(4, selectedBounds.width),
      height: Math.max(4, selectedBounds.height),
    };
    stageElement.setPointerCapture(event.pointerId);
    interaction = { kind: "resize-annotation", pointerId: event.pointerId, handle, initial: selectedAnnotation, bounds };
  }

  function handlePointerDown(event: PointerEvent): void {
    if (loading || busy || !session || event.button !== 0 || isUiTarget(event.target)) return;
    const point = stagePoint(event);
    if (longModeMenuOpen) {
      event.preventDefault();
      dismissLongCapturePreparation();
      return;
    }
    if (choosingScrollAnchor) {
      event.preventDefault();
      if (hitTestSelection(point)) void startLongCapture(point);
      else showToast("请在选区内点击要自动滚动的正文区域");
      return;
    }
    if (longCaptureActive) return;
    rememberDoubleClickSnapshot(event, point);
    if (textDraft) return;
    hoverPoint = null;
    stageElement.setPointerCapture(event.pointerId);

    if (!selection) {
      selection = { x: point.x, y: point.y, width: 0, height: 0 };
      interaction = { kind: "create-selection", pointerId: event.pointerId, origin: point };
      return;
    }

    if (!selectionLocked && !activeTool && !hitTestSelection(point)) {
      selectedId = null;
      selection = { x: point.x, y: point.y, width: 0, height: 0 };
      interaction = { kind: "create-selection", pointerId: event.pointerId, origin: point };
      return;
    }

    const hit = hitAnnotation(point);
    if (hit) {
      selectedId = hit.id;
      activeTool = hit.kind;
      interaction = { kind: "move-annotation", pointerId: event.pointerId, origin: point, initial: hit };
      previewAnnotations = [...history.present];
      return;
    }

    if (!hitTestSelection(point)) return;
    selectedId = null;
    if (!activeTool) {
      if (!selectionLocked) interaction = { kind: "move-selection", pointerId: event.pointerId, origin: point, initial: selection };
      return;
    }
    const start = selectionPoint(point);
    draft = createDraft(activeTool, start);
    interaction = { kind: "draw", pointerId: event.pointerId, origin: start };
  }

  function hitTestSelection(point: Point): boolean {
    return !!selection
      && point.x >= selection.x && point.x <= selection.x + selection.width
      && point.y >= selection.y && point.y <= selection.y + selection.height;
  }

  function translatedInside(annotation: Annotation, delta: Point): Annotation {
    if (!selection) return translateAnnotation(annotation, delta);
    const bounds = annotationBounds(annotation);
    const corrected = {
      x: clamp(delta.x, selection.x - bounds.x, selection.x + selection.width - bounds.x - bounds.width),
      y: clamp(delta.y, selection.y - bounds.y, selection.y + selection.height - bounds.y - bounds.height),
    };
    return translateAnnotation(annotation, corrected);
  }

  function updateDraft(point: Point): void {
    if (!draft || !interaction || interaction.kind !== "draw") return;
    const current = selectionPoint(point);
    if (draft.kind === "shape" || draft.kind === "text") {
      draft = { ...draft, rect: roundRect(normalizeRect(interaction.origin, current)) };
    } else if (draft.kind === "line") {
      draft = { ...draft, to: current };
    } else if (draft.kind === "pencil" || draft.kind === "marker") {
      const last = draft.points.at(-1)!;
      if (Math.hypot(current.x - last.x, current.y - last.y) >= 1.5) draft = { ...draft, points: [...draft.points, current] };
    } else if ("mode" in draft && draft.mode === "rectangle") {
      draft = { ...draft, rect: roundRect(normalizeRect(interaction.origin, current)) };
    } else {
      const last = draft.points.at(-1)!;
      if (Math.hypot(current.x - last.x, current.y - last.y) >= 1.5) draft = { ...draft, points: [...draft.points, current] };
    }
  }

  function handlePointerMove(event: PointerEvent): void {
    if (!session || loading) return;
    const point = stagePoint(event);
    if (longCaptureActive) {
      hoverPoint = null;
      hoverCss = null;
      return;
    }
    if (doubleClickSnapshot
      && Math.hypot(event.clientX - doubleClickSnapshot.clientPoint.x, event.clientY - doubleClickSnapshot.clientPoint.y) > doubleClickDistance) {
      clearDoubleClickSnapshot();
    }
    if (!interaction) {
      hoverPoint = point;
      hoverCss = pointerCss(event);
      updateMagnifier(point);
      return;
    }
    if (interaction.pointerId !== event.pointerId) return;
    if (interaction.kind === "create-selection") {
      selection = roundRect(normalizeRect(interaction.origin, point));
    } else if (interaction.kind === "move-selection") {
      selection = moveRect(interaction.initial, { x: point.x - interaction.origin.x, y: point.y - interaction.origin.y }, frameBounds);
    } else if (interaction.kind === "resize-selection") {
      selection = resizeRect(interaction.initial, interaction.handle, point, frameBounds);
    } else if (interaction.kind === "draw") {
      updateDraft(point);
    } else if (interaction.kind === "move-annotation") {
      const next = translatedInside(interaction.initial, { x: point.x - interaction.origin.x, y: point.y - interaction.origin.y });
      previewAnnotations = updateAnnotation(next);
    } else if (interaction.kind === "resize-annotation" && selection) {
      const target = resizeRect(interaction.bounds, interaction.handle, point, selection, 4);
      previewAnnotations = updateAnnotation(scaleAnnotation(interaction.initial, interaction.bounds, target));
    }
  }

  function beginTextEditing(annotation: TextAnnotation): void {
    if (!selection) return;
    let rect = annotation.rect;
    if (rect.width < 20 || rect.height < 20) {
      rect = clampRect({ x: rect.x, y: rect.y, width: 280, height: 110 }, selection, 20);
    }
    textDraft = { annotation: { ...annotation, rect }, value: "" };
    draft = null;
    void tick().then(() => textAreaElement?.focus());
  }

  function finishText(save = true): void {
    if (!textDraft) return;
    const value = textDraft.value.trimEnd();
    if (save && value.trim()) {
      const annotation = { ...textDraft.annotation, text: value };
      commitAnnotations([...history.present, annotation]);
      selectedId = annotation.id;
    }
    textDraft = null;
    draft = null;
  }

  function handlePointerUp(event: PointerEvent): void {
    if (!interaction || interaction.pointerId !== event.pointerId) return;
    if (stageElement.hasPointerCapture(event.pointerId)) stageElement.releasePointerCapture(event.pointerId);
    if (interaction.kind === "create-selection" && selection) {
      selection = clampRect(roundRect(selection), frameBounds);
      activeTool = null;
    } else if (interaction.kind === "draw" && draft) {
      if (draft.kind === "text") beginTextEditing(draft);
      else if (validDraft(draft)) {
        commitAnnotations([...history.present, draft]);
        selectedId = draft.kind === "effect" || draft.kind === "eraser" ? null : draft.id;
        draft = null;
      } else {
        draft = null;
      }
    } else if ((interaction.kind === "move-annotation" || interaction.kind === "resize-annotation") && previewAnnotations) {
      commitAnnotations(previewAnnotations);
    }
    interaction = null;
    void tick().then(updateViewport);
  }

  function handlePointerCancel(event: PointerEvent): void {
    if (!interaction || interaction.pointerId !== event.pointerId) return;
    if (interaction.kind === "draw") draft = null;
    previewAnnotations = null;
    interaction = null;
  }

  function updateMagnifier(point: Point): void {
    if (!sourceCanvas || !magnifierCanvas || !session) return;
    const size = 11;
    const context = magnifierCanvas.getContext("2d");
    const sourceContext = sourceCanvas.getContext("2d", { willReadFrequently: true });
    if (!context || !sourceContext) return;
    const x = clamp(Math.round(point.x), 0, session.frameWidth - 1);
    const y = clamp(Math.round(point.y), 0, session.frameHeight - 1);
    const data = sourceContext.getImageData(x, y, 1, 1).data;
    sampledColor = { r: data[0], g: data[1], b: data[2] };
    context.imageSmoothingEnabled = false;
    context.clearRect(0, 0, magnifierCanvas.width, magnifierCanvas.height);
    context.drawImage(sourceCanvas, x - Math.floor(size / 2), y - Math.floor(size / 2), size, size, 0, 0, magnifierCanvas.width, magnifierCanvas.height);
  }

  function colorText(): string {
    if (colorFormat === "rgb") return `RGB(${sampledColor.r}, ${sampledColor.g}, ${sampledColor.b})`;
    return `#${[sampledColor.r, sampledColor.g, sampledColor.b].map((value) => value.toString(16).padStart(2, "0")).join("").toUpperCase()}`;
  }

  function toggleColorFormat(): void {
    colorFormat = colorFormat === "hex" ? "rgb" : "hex";
    persistPreferences();
  }

  async function copyColor(): Promise<void> {
    if (!hoverPoint) return;
    try {
      await navigator.clipboard.writeText(colorText());
      showToast(`已复制 ${colorText()}`);
    } catch {
      showToast("无法复制颜色");
    }
  }

  async function destroyCaptureWindow(): Promise<void> {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await withTimeout(getCurrentWindow().destroy(), 2_500, "关闭截图窗口超时。");
  }

  async function cancel(): Promise<void> {
    if (busy) return;
    const closingSessionId = session?.id ?? pendingSessionId ?? undefined;
    loadGeneration += 1;
    loadQueued = false;
    queuedSessionId = undefined;
    busy = true;
    try {
      await withTimeout(api.cancel(closingSessionId), 2_500, "取消截图超时。");
      oncancel?.();
    } catch (value) {
      try {
        await destroyCaptureWindow();
        oncancel?.();
      } catch {
        reportError(value);
      }
    } finally {
      busy = false;
    }
  }

  async function exportImage(action: "copy" | "save" | "pin"): Promise<void> {
    if (!session || !selection || !sourceImage || busy || selection.width < 1 || selection.height < 1) return;
    finishText(true);
    busy = true;
    error = "";
    try {
      const png = await exportSelectionPng(
        sourceImage,
        session.frameWidth,
        session.frameHeight,
        history.present,
        selection,
        { framePng: sourcePng },
      );
      const result = await api.exportPng({ sessionId: session.id, action }, png);
      if (result.canceled) return;
      if (preferenceTimer) clearTimeout(preferenceTimer);
      await api.saveToolSettings({ ...toolSettings }, colorFormat).catch(() => undefined);
      oncomplete?.(result);
    } catch (value) {
      reportError(value);
    } finally {
      busy = false;
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (loading) {
      if (event.key === "Escape") {
        event.preventDefault();
        void cancel();
      }
      return;
    }
    if (longConfirmOpen) {
      if (event.key === "Escape") {
        event.preventDefault();
        dismissLongCapturePreparation();
      }
      return;
    }
    if (longModeMenuOpen && event.key === "Escape") {
      event.preventDefault();
      dismissLongCapturePreparation();
      return;
    }
    if (choosingScrollAnchor) {
      if (event.key === "Escape") {
        event.preventDefault();
        dismissLongCapturePreparation();
      } else if (event.key === "Enter") {
        event.preventDefault();
        void startLongCapture();
      }
      return;
    }
    if (longStatus && longStatus.state !== "ready") {
      // The embedded control owns capture-phase shortcuts so keyboard and
      // button actions share one busy/action generation.
      return;
    }
    if (longStatus || longBusy) {
      if (longTextDraft) {
        if (event.key === "Escape") {
          event.preventDefault();
          finishLongText(false);
        } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
          event.preventDefault();
          finishLongText(true);
        }
      } else if (longStatus?.state === "ready" && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
        event.preventDefault();
        if (event.shiftKey) redoLongAnnotation(); else undoLongAnnotation();
      } else if (longStatus?.state === "ready" && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
        event.preventDefault();
        redoLongAnnotation();
      } else if (longStatus?.state === "ready" && (event.key === "Delete" || event.key === "Backspace") && longSelectedId) {
        event.preventDefault();
        removeLongSelected();
      } else if (event.key === "Escape") {
        event.preventDefault();
        void cancelLongCaptureToEditor();
      } else if (longStatus?.state === "ready" && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
        event.preventDefault();
        void exportLongImage("copy");
      } else if (longStatus?.state === "ready" && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void exportLongImage("save");
      }
      return;
    }
    if (busy) return;
    if (textDraft) {
      if (event.key === "Escape") {
        event.preventDefault();
        finishText(false);
      } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        finishText(true);
      }
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      if (selectedId) selectedId = null;
      else void cancel();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
      event.preventDefault();
      if (event.shiftKey) redo(); else undo();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
      event.preventDefault();
      redo();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
      event.preventDefault();
      if (selection) void exportImage("copy");
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void exportImage("save");
    } else if (event.key === "Delete" || event.key === "Backspace") {
      if (selectedId) {
        event.preventDefault();
        removeSelected();
      }
    } else if (event.key === "Shift" && hoverPoint && !event.repeat) {
      event.preventDefault();
      toggleColorFormat();
    } else if (event.key.toLowerCase() === "c" && hoverPoint && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      void copyColor();
    }
  }

  function handleDoubleClick(event: MouseEvent): void {
    if (loading || busy || longCaptureActive || !session || isUiTarget(event.target)) return;
    const snapshot = doubleClickSnapshot;
    clearDoubleClickSnapshot();
    if (snapshot) restoreDoubleClickSnapshot(snapshot);
    if (!selection || (snapshot && !hitTestSelection(snapshot.point)) || !hitTestSelection(stagePoint(event))) return;
    event.preventDefault();
    event.stopPropagation();
    void exportImage("copy");
  }

  function handleContextMenu(event: MouseEvent): void {
    if (longConfirmOpen || choosingScrollAnchor) {
      event.preventDefault();
      event.stopPropagation();
      dismissLongCapturePreparation();
      return;
    }
    if (longStatus || longBusy) {
      event.preventDefault();
      event.stopPropagation();
      void cancelLongCaptureToEditor();
      return;
    }
    const hasUsableSelection = !!selection && selection.width >= 1 && selection.height >= 1;
    const pointIsInsideSelection = hasUsableSelection && !!session && hitTestSelection(stagePoint(event));
    if (pointIsInsideSelection) return;

    event.preventDefault();
    event.stopPropagation();
    void cancel();
  }

  function selectionPath(): string {
    if (!session || !selection) return "";
    const { frameWidth: width, frameHeight: height } = session;
    return `M0 0H${width}V${height}H0Z M${selection.x} ${selection.y}V${selection.y + selection.height}H${selection.x + selection.width}V${selection.y}Z`;
  }

  function toolbarStyle(): string {
    if (!selection || !session || viewportWidth <= 0 || viewportHeight <= 0) return "visibility:hidden";
    const sx = viewportWidth / session.frameWidth;
    const sy = viewportHeight / session.frameHeight;
    const cssSelection = { x: selection.x * sx, y: selection.y * sy, width: selection.width * sx, height: selection.height * sy };
    const position = placeToolbar(cssSelection, { x: 0, y: 0, width: viewportWidth, height: viewportHeight }, Math.min(toolbarWidth, viewportWidth - 16), toolbarHeight, 8);
    return `left:${position.left}px;top:${position.top}px;max-width:${Math.max(120, viewportWidth - 16)}px`;
  }

  function magnifierStyle(): string {
    if (!hoverCss) return "visibility:hidden";
    const width = 152;
    const height = 175;
    const left = hoverCss.x + 22 + width <= viewportWidth ? hoverCss.x + 22 : hoverCss.x - width - 22;
    const top = hoverCss.y + 22 + height <= viewportHeight ? hoverCss.y + 22 : hoverCss.y - height - 22;
    return `left:${clamp(left, 8, Math.max(8, viewportWidth - width - 8))}px;top:${clamp(top, 8, Math.max(8, viewportHeight - height - 8))}px`;
  }

  function rectStyle(rect: Rect): string {
    if (!session) return "";
    return `left:${rect.x / session.frameWidth * 100}%;top:${rect.y / session.frameHeight * 100}%;width:${rect.width / session.frameWidth * 100}%;height:${rect.height / session.frameHeight * 100}%`;
  }

  function resetEditor(): void {
    resetLongCapture();
    selection = null;
    history = createHistory([]);
    previewAnnotations = null;
    draft = null;
    textDraft = null;
    selectedId = null;
    activeTool = null;
    interaction = null;
    hoverPoint = null;
    hoverCss = null;
    error = "";
  }

  function releaseSourceImage(): void {
    if (sourceImage && "close" in sourceImage && typeof sourceImage.close === "function") sourceImage.close();
    sourceImage = null;
  }

  function returnToIdle(closedSessionId?: string): void {
    if (closedSessionId && session && session.id !== closedSessionId) return;
    loadGeneration += 1;
    releaseSourceImage();
    sourcePng = null;
    session = null;
    pendingSessionId = null;
    loading = false;
    busy = false;
    resetEditor();
    invalidatingSessionId = null;
  }

  async function invalidateCapture(message: string): Promise<void> {
    const current = session;
    if (!current || invalidatingSessionId === current.id) return;
    invalidatingSessionId = current.id;
    busy = true;
    error = message;
    onerror?.(message);
    if (typeof window !== "undefined" && typeof window.alert === "function") window.alert(message);
    try {
      await api.cancel(current.id);
      oncancel?.();
    } catch (value) {
      reportError(value);
      busy = false;
      invalidatingSessionId = null;
    }
  }

  async function loadFrame(sessionId: string): Promise<Uint8Array> {
    const attemptTimeout = Math.max(10, Math.floor(loadTimeoutMs / 2));
    try {
      return await withTimeout(
        api.getFrame(sessionId),
        attemptTimeout,
        "读取截图画面超时，正在自动重试。",
      );
    } catch (value) {
      if (!(value instanceof ScreenshotLoadTimeoutError)) throw value;
      return withTimeout(
        api.getFrame(sessionId),
        attemptTimeout,
        "读取截图画面超时，请重试截图。",
      );
    }
  }

  async function loadCapture(requestedSessionId?: string): Promise<void> {
    if (session && sourceImage && (!requestedSessionId || requestedSessionId === session.id)) {
      loading = false;
      error = "";
      if (requestedSessionId) {
        try {
          await tick();
          updateViewport();
          renderNow();
          await api.present(session.id);
        } catch (value) {
          reportError(value);
        }
      }
      stageElement?.focus();
      return;
    }
    const generation = ++loadGeneration;
    loading = true;
    busy = false;
    error = "";
    if (requestedSessionId) pendingSessionId = requestedSessionId;
    let frameSessionId = requestedSessionId;
    try {
      const loadedSession = await withTimeout(
        api.getSession(requestedSessionId),
        loadTimeoutMs,
        "读取截图会话超时，请重试截图。",
      );
      if (componentDisposed || generation !== loadGeneration) return;
      if (!loadedSession) {
        returnToIdle();
        return;
      }
      pendingSessionId = loadedSession.id;
      frameSessionId = loadedSession.id;
      const settingsPromise = withTimeout(
        api.getSettings(),
        Math.min(2_000, loadTimeoutMs),
        "读取截图偏好设置超时。",
      ).catch(() => null);
      const frame = await loadFrame(loadedSession.id);
      const image = await withTimeout(
        decodePng(frame),
        loadTimeoutMs,
        "解码截图画面超时，请重试截图。",
      );
      validateFrameDimensions(loadedSession, image);
      if (componentDisposed || generation !== loadGeneration) {
        if ("close" in image && typeof image.close === "function") image.close();
        return;
      }
      const settings = await settingsPromise;
      releaseSourceImage();
      resetEditor();
      session = loadedSession;
      sourceImage = image;
      sourcePng = frame;
      toolSettings = { ...DEFAULT_TOOL_SETTINGS, ...(settings?.toolParameters ?? {}) };
      colorFormat = settings?.colorFormat === "rgb" ? "rgb" : "hex";
      sourceCanvas.width = loadedSession.frameWidth;
      sourceCanvas.height = loadedSession.frameHeight;
      sourceCanvas.getContext("2d", { willReadFrequently: true })?.drawImage(
        image,
        0,
        0,
        loadedSession.frameWidth,
        loadedSession.frameHeight,
      );
      displayCanvas.width = loadedSession.frameWidth;
      displayCanvas.height = loadedSession.frameHeight;
      resizeObserver?.observe(stageElement);
      loading = false;
      await tick();
      updateViewport();
      renderNow();
      await api.present(loadedSession.id);
      if (componentDisposed || generation !== loadGeneration) return;
      pendingSessionId = null;
      stageElement.focus();
      scheduleWindowGeometryValidation?.();
    } catch (value) {
      if (componentDisposed || generation !== loadGeneration) return;
      loading = false;
      reportError(value);
      await tick();
      if (frameSessionId && !componentDisposed && generation === loadGeneration) {
        try {
          renderNow();
          await api.present(frameSessionId);
        } catch {
          // The original error remains the actionable message for the user.
        }
      }
    }
  }

  function requestLoad(requestedSessionId?: string): void {
    if (session && sourceImage && !requestedSessionId) {
      loading = false;
      error = "";
      stageElement?.focus();
      return;
    }
    if (requestedSessionId) queuedSessionId = requestedSessionId;
    loadQueued = true;
    if (loadInFlight) return;

    loadInFlight = (async () => {
      try {
        while (loadQueued && !componentDisposed) {
          loadQueued = false;
          const nextSessionId = queuedSessionId;
          queuedSessionId = undefined;
          await loadCapture(nextSessionId);
          if (queuedSessionId && session?.id === queuedSessionId && sourceImage) {
            queuedSessionId = undefined;
            loadQueued = false;
          }
        }
      } finally {
        loadInFlight = null;
        if (loadQueued && !componentDisposed) requestLoad(queuedSessionId);
      }
    })();
  }

  onMount(() => {
    componentDisposed = false;
    const eventCleanups: Array<() => void> = [];
    document.documentElement.classList.add("screenshot-tool-page");
    document.body.classList.add("screenshot-tool-page");
    resizeObserver = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(updateViewport);
    void loadLongCaptureCapability();

    const recoverOnFocus = () => requestLoad();
    const recoverOnVisibility = () => {
      if (!document.hidden) requestLoad();
    };
    window.addEventListener("focus", recoverOnFocus);
    document.addEventListener("visibilitychange", recoverOnVisibility);
    requestLoad(sessionId);

    void (async () => {
      if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
        const { listen } = await import("@tauri-apps/api/event");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const readyCleanup = await listen<{ id?: string } | string>("screenshot_session_ready", (event) => {
          const nextId = typeof event.payload === "string" ? event.payload : event.payload?.id;
          requestLoad(nextId);
        });
        const closedCleanup = await listen<{ id?: string } | string>("screenshot_session_closed", (event) => {
          const closedId = typeof event.payload === "string" ? event.payload : event.payload?.id;
          returnToIdle(closedId);
        });
        const errorCleanup = await listen<{ message?: string } | string>("screenshot_capture_error", (event) => {
          const message = typeof event.payload === "string"
            ? event.payload
            : event.payload?.message ?? "截图准备失败，请重试。";
          loadGeneration += 1;
          loading = false;
          reportError(message);
        });
        const [longProgressCleanup, longPausedCleanup, longAttentionCleanup, longReadyCleanup, longFailedCleanup] = await Promise.all([
          listen<unknown>("long_capture_progress", (event) => handleLongCaptureEvent(event.payload, "capturing")),
          listen<unknown>("long_capture_paused", (event) => handleLongCaptureEvent(event.payload, "paused")),
          listen<unknown>("long_capture_attention_required", (event) => handleLongCaptureEvent(event.payload, "paused")),
          listen<unknown>("long_capture_ready", (event) => handleLongCaptureEvent(event.payload, "ready")),
          listen<unknown>("long_capture_failed", (event) => handleLongCaptureEvent(event.payload, "failed")),
        ]);
        if (componentDisposed) {
          readyCleanup();
          closedCleanup();
          errorCleanup();
          longProgressCleanup();
          longPausedCleanup();
          longAttentionCleanup();
          longReadyCleanup();
          longFailedCleanup();
          return;
        }
        eventCleanups.push(
          readyCleanup,
          closedCleanup,
          errorCleanup,
          longProgressCleanup,
          longPausedCleanup,
          longAttentionCleanup,
          longReadyCleanup,
          longFailedCleanup,
        );
        requestLoad(sessionId);
        const currentWindow = getCurrentWindow();
        scheduleWindowGeometryValidation = () => {
          if (geometryValidationTimer) clearTimeout(geometryValidationTimer);
          geometryValidationTimer = setTimeout(() => {
            void (async () => {
              const expected = session;
              if (!expected || invalidatingSessionId === expected.id) return;
              try {
                const [position, size, scaleFactor] = await Promise.all([
                  currentWindow.outerPosition(),
                  currentWindow.innerSize(),
                  currentWindow.scaleFactor(),
                ]);
                if (session?.id === expected.id && !matchesScreenshotWindow(expected, {
                  position,
                  size,
                  scaleFactor,
                })) {
                  await invalidateCapture("显示器布局、分辨率或缩放已发生变化，本次截图已取消，请重新截图。");
                }
              } catch {
                if (session?.id === expected.id) {
                  await invalidateCapture("无法确认当前显示器状态，本次截图已取消，请重新截图。");
                }
              }
            })();
          }, 400);
        };
        const [resizedCleanup, movedCleanup, scaleCleanup] = await Promise.all([
          currentWindow.onResized(() => scheduleWindowGeometryValidation?.()),
          currentWindow.onMoved(() => scheduleWindowGeometryValidation?.()),
          currentWindow.onScaleChanged(() => scheduleWindowGeometryValidation?.()),
        ]);
        if (componentDisposed) {
          resizedCleanup();
          movedCleanup();
          scaleCleanup();
          return;
        }
        eventCleanups.push(resizedCleanup, movedCleanup, scaleCleanup);
      }
    })().catch((value) => {
      if (componentDisposed) return;
      console.error("初始化截图窗口监听失败", value);
      requestLoad(sessionId);
    });

    return () => {
      componentDisposed = true;
      loadGeneration += 1;
      loadQueued = false;
      queuedSessionId = undefined;
      for (const cleanup of eventCleanups) cleanup();
      window.removeEventListener("focus", recoverOnFocus);
      document.removeEventListener("visibilitychange", recoverOnVisibility);
      resizeObserver?.disconnect();
      if (renderFrame !== undefined) cancelAnimationFrame(renderFrame);
      if (preferenceTimer) clearTimeout(preferenceTimer);
      if (toastTimer) clearTimeout(toastTimer);
      if (longPollTimer) clearTimeout(longPollTimer);
      if (geometryValidationTimer) clearTimeout(geometryValidationTimer);
      clearDoubleClickSnapshot();
      clearLongTiles();
      scheduleWindowGeometryValidation = undefined;
      releaseSourceImage();
      document.documentElement.classList.remove("screenshot-tool-page");
      document.body.classList.remove("screenshot-tool-page");
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} onresize={updateViewport} />

<div
  class="screenshot-tool"
  class:selection-movable={selectionMoveCursor}
  data-testid="screenshot-tool"
  bind:this={stageElement}
  role="application"
  aria-label="飞花 - PetalDesk 截图编辑器"
  tabindex="-1"
  onpointerdown={handlePointerDown}
  onpointermove={handlePointerMove}
  onpointerup={handlePointerUp}
  onpointercancel={handlePointerCancel}
  ondblclick={handleDoubleClick}
  oncontextmenu={handleContextMenu}
  onpointerleave={() => { if (!interaction) { hoverPoint = null; hoverCss = null; } }}
>
  <canvas class="source-canvas" bind:this={sourceCanvas} aria-hidden="true"></canvas>
  <canvas class="capture-canvas" bind:this={displayCanvas} aria-label="截图画面"></canvas>

  {#if session && selection}
    <svg class="selection-layer" viewBox={`0 0 ${session.frameWidth} ${session.frameHeight}`} preserveAspectRatio="none" aria-hidden="true">
      <path class="dim-mask" d={selectionPath()} fill-rule="evenodd"></path>
      <rect class="selection-border" x={selection.x} y={selection.y} width={selection.width} height={selection.height}></rect>
      {#if !busy && !textDraft && !longCaptureActive}
        {#each Object.entries(selectionHandlePoints(selection)) as [handle, point]}
          <rect
            class={`resize-handle handle-${handle}`}
            x={point.x - 5}
            y={point.y - 5}
            width="10"
            height="10"
            role="button"
            aria-label={`调整选区 ${handle}`}
            tabindex="-1"
            onpointerdown={(event) => startSelectionResize(event, handle as ResizeHandle)}
          ></rect>
        {/each}
      {/if}
      {#if selectedBounds && selectedAnnotation && !longCaptureActive}
        <rect
          class="annotation-border"
          x={selectedBounds.x - 4}
          y={selectedBounds.y - 4}
          width={selectedBounds.width + 8}
          height={selectedBounds.height + 8}
        ></rect>
        {#each Object.entries(selectionHandlePoints(selectedBounds)) as [handle, point]}
          <rect
            class={`annotation-handle handle-${handle}`}
            x={point.x - 5}
            y={point.y - 5}
            width="10"
            height="10"
            role="button"
            aria-label={`调整标注 ${handle}`}
            tabindex="-1"
            onpointerdown={(event) => startAnnotationResize(event, handle as ResizeHandle)}
          ></rect>
        {/each}
      {/if}
    </svg>

    <div class="selection-size" style={rectStyle({ x: selection.x, y: Math.max(0, selection.y - 28 / Math.max(0.1, viewportHeight / session.frameHeight)), width: 1, height: 1 })}>
      {Math.round(selection.width)} × {Math.round(selection.height)} px
    </div>

    {#if textDraft}
      <textarea
        class="text-editor"
        bind:this={textAreaElement}
        bind:value={textDraft.value}
        style={`${rectStyle(textDraft.annotation.rect)};font-family:${textDraft.annotation.fontFamily};font-size:${textDraft.annotation.fontSize * viewportWidth / Math.max(1, session.frameWidth)}px;color:${textDraft.annotation.color};font-weight:${textDraft.annotation.bold ? 700 : 400};font-style:${textDraft.annotation.italic ? "italic" : "normal"};text-decoration:${textDraft.annotation.underline ? "underline" : "none"}`}
        aria-label="输入标注文字"
        placeholder="输入文字"
        onblur={() => finishText(true)}
        onpointerdown={(event) => event.stopPropagation()}
      ></textarea>
    {/if}

    {#if !longCaptureActive}
      <div
        class="toolbar-dock ui-layer"
        bind:this={toolbarElement}
        style={toolbarStyle()}
        onpointerdown={(event) => event.stopPropagation()}
        onpointermove={(event) => event.stopPropagation()}
        role="toolbar"
        tabindex="-1"
        aria-label="截图标注工具"
      >
      <div class="primary-tools">
        <button class:active={activeTool === "shape"} type="button" title="矩形和椭圆" aria-label="形状" onclick={() => activateTool("shape")}><Shapes size={19} /></button>
        <button class:active={activeTool === "line"} type="button" title="线条和箭头" aria-label="线条和箭头" onclick={() => activateTool("line")}><ArrowRight size={19} /></button>
        <button class:active={activeTool === "pencil"} type="button" title="铅笔" aria-label="铅笔" onclick={() => activateTool("pencil")}><Pencil size={19} /></button>
        <button class:active={activeTool === "marker"} type="button" title="马克笔" aria-label="马克笔" onclick={() => activateTool("marker")}><Highlighter size={19} /></button>
        <button class:active={activeTool === "effect"} type="button" title="马赛克和模糊" aria-label="马赛克和模糊" onclick={() => activateTool("effect")}><ScanLine size={19} /></button>
        <button class:active={activeTool === "text"} type="button" title="文字" aria-label="文字" onclick={() => activateTool("text")}><Type size={19} /></button>
        <button class:active={activeTool === "eraser"} type="button" title="橡皮擦" aria-label="橡皮擦" onclick={() => activateTool("eraser")}><Eraser size={19} /></button>
        <button
          type="button"
          title={longCapability?.reason || "长截图（默认手动滚动）"}
          aria-label="长截图"
          disabled={!longCapabilityLoaded || !longCapability?.available}
          onclick={() => requestLongCapture("manual")}
        ><GalleryVerticalEnd size={19} /></button>
        <button
          class:active={longModeMenuOpen}
          class="long-mode-toggle"
          type="button"
          title="选择长截图模式"
          aria-label="选择长截图模式"
          aria-expanded={longModeMenuOpen}
          aria-controls="long-mode-options"
          disabled={!longCapabilityLoaded || !longCapability?.available}
          onclick={toggleLongModeMenu}
        ><ChevronDown size={15} /></button>
        <span class="separator"></span>
        <button type="button" title={`撤销 ${formatShortcut("Ctrl+Z")}`} aria-label="撤销" disabled={history.past.length === 0} onclick={undo}><Undo2 size={19} /></button>
        <button type="button" title={`重做 ${formatShortcut("Ctrl+Y")}`} aria-label="重做" disabled={history.future.length === 0} onclick={redo}><Redo2 size={19} /></button>
        <span class="separator"></span>
        <button type="button" title="取消 Esc" aria-label="取消截图" onclick={() => void cancel()}><X size={19} /></button>
        <button type="button" title="置顶贴图" aria-label="置顶贴图" onclick={() => void exportImage("pin")}><Pin size={19} /></button>
        <button type="button" title={`保存 ${formatShortcut("Ctrl+S")}`} aria-label="保存截图" onclick={() => void exportImage("save")}><Save size={19} /></button>
        <button class="primary-action" type="button" title={`复制 ${formatShortcut("Ctrl+C")}`} aria-label="复制截图" onclick={() => void exportImage("copy")}><Copy size={19} /></button>
      </div>

      {#if longModeMenuOpen}
        <div id="long-mode-options" class="tool-options long-mode-options" role="toolbar" aria-label="长截图模式">
          <strong>长截图模式</strong>
          <div class="segmented">
            <button bind:this={longModeFirstButton} class:active={pendingLongMode === "manual"} type="button" onclick={() => requestLongCapture("manual")}>手动滚动（默认）</button>
            <button class:active={pendingLongMode === "current"} type="button" onclick={() => requestLongCapture("current")}>当前位置自动</button>
            <button class:active={pendingLongMode === "top"} type="button" onclick={() => requestLongCapture("top")}>从顶部自动</button>
          </div>
        </div>
      {:else if activeTool}
        <div class="tool-options" aria-label="工具参数">
          {#if activeTool === "shape"}
            <div class="segmented" aria-label="形状类型">
              <button class:active={toolSettings.shape === "rectangle"} type="button" onclick={() => updateSettings({ shape: "rectangle" })}>矩形</button>
              <button class:active={toolSettings.shape === "ellipse"} type="button" onclick={() => updateSettings({ shape: "ellipse" })}>椭圆</button>
            </div>
            <label><span>填充</span><input type="checkbox" checked={toolSettings.fillColor !== null} onchange={(event) => updateSettings({ fillColor: event.currentTarget.checked ? `${toolSettings.strokeColor}55` : null })} /></label>
          {:else if activeTool === "line"}
            <div class="segmented" aria-label="线条类型">
              <button class:active={toolSettings.line === "line"} type="button" onclick={() => updateSettings({ line: "line" })}>直线</button>
              <button class:active={toolSettings.line === "arrow"} type="button" onclick={() => updateSettings({ line: "arrow" })}>箭头</button>
              <button class:active={toolSettings.line === "double-arrow"} type="button" onclick={() => updateSettings({ line: "double-arrow" })}>双箭头</button>
            </div>
            <div class="segmented" aria-label="线型">
              <button class:active={toolSettings.lineStyle === "solid"} type="button" onclick={() => updateSettings({ lineStyle: "solid" })}>实线</button>
              <button class:active={toolSettings.lineStyle === "dashed"} type="button" onclick={() => updateSettings({ lineStyle: "dashed" })}>虚线</button>
            </div>
          {:else if activeTool === "marker"}
            <div class="segmented" aria-label="笔头">
              <button class:active={toolSettings.markerTip === "round"} type="button" onclick={() => updateSettings({ markerTip: "round" })}>圆头</button>
              <button class:active={toolSettings.markerTip === "square"} type="button" onclick={() => updateSettings({ markerTip: "square" })}>方头</button>
            </div>
          {:else if activeTool === "effect"}
            <div class="segmented" aria-label="效果类型">
              <button class:active={toolSettings.effect === "mosaic"} type="button" onclick={() => updateSettings({ effect: "mosaic" })}>马赛克</button>
              <button class:active={toolSettings.effect === "blur"} type="button" onclick={() => updateSettings({ effect: "blur" })}>模糊</button>
            </div>
            <div class="segmented" aria-label="效果范围">
              <button class:active={toolSettings.effectMode === "brush"} type="button" onclick={() => updateSettings({ effectMode: "brush" })}>画笔</button>
              <button class:active={toolSettings.effectMode === "rectangle"} type="button" onclick={() => updateSettings({ effectMode: "rectangle" })}>区域</button>
            </div>
          {:else if activeTool === "eraser"}
            <div class="segmented" aria-label="橡皮范围">
              <button class:active={toolSettings.eraserMode === "brush"} type="button" onclick={() => updateSettings({ eraserMode: "brush" })}>画笔</button>
              <button class:active={toolSettings.eraserMode === "rectangle"} type="button" onclick={() => updateSettings({ eraserMode: "rectangle" })}>区域</button>
            </div>
          {:else if activeTool === "text"}
            <select aria-label="字体" value={toolSettings.fontFamily} onchange={(event) => updateSettings({ fontFamily: event.currentTarget.value })}>
              <option>Microsoft YaHei UI</option><option>SimSun</option><option>SimHei</option><option>Segoe UI</option>
            </select>
            <div class="text-toggles">
              <button class:active={toolSettings.textBold} type="button" aria-label="粗体" onclick={() => updateSettings({ textBold: !toolSettings.textBold })}><Bold size={16} /></button>
              <button class:active={toolSettings.textItalic} type="button" aria-label="斜体" onclick={() => updateSettings({ textItalic: !toolSettings.textItalic })}><Italic size={16} /></button>
              <button class:active={toolSettings.textUnderline} type="button" aria-label="下划线" onclick={() => updateSettings({ textUnderline: !toolSettings.textUnderline })}><Underline size={16} /></button>
            </div>
          {/if}

          {#if activeTool !== "effect" && activeTool !== "eraser"}
            <div class="palette" aria-label="颜色">
              {#each colors as color}
                <button
                  class:selected={(activeTool === "text" ? toolSettings.textColor : toolSettings.strokeColor) === color}
                  type="button"
                  style:background={color}
                  aria-label={`颜色 ${color}`}
                  onclick={() => updateSettings(activeTool === "text" ? { textColor: color } : { strokeColor: color })}
                ></button>
              {/each}
            </div>
          {/if}

          {#if activeTool === "shape" || activeTool === "line"}
            <label class="range"><span>线宽</span><input type="range" min="1" max="20" value={toolSettings.strokeWidth} oninput={(event) => updateSettings({ strokeWidth: Number(event.currentTarget.value) }, false)} onchange={styleSelected} /></label>
          {:else if activeTool === "pencil"}
            <label class="range"><span>粗细</span><input type="range" min="1" max="30" value={toolSettings.pencilWidth} oninput={(event) => updateSettings({ pencilWidth: Number(event.currentTarget.value) }, false)} onchange={styleSelected} /></label>
          {:else if activeTool === "marker"}
            <label class="range"><span>粗细</span><input type="range" min="6" max="64" value={toolSettings.markerWidth} oninput={(event) => updateSettings({ markerWidth: Number(event.currentTarget.value) }, false)} onchange={styleSelected} /></label>
          {:else if activeTool === "effect"}
            <label class="range"><span>范围</span><input type="range" min="8" max="100" value={toolSettings.effectSize} oninput={(event) => updateSettings({ effectSize: Number(event.currentTarget.value) }, false)} /></label>
            <label class="range"><span>强度</span><input type="range" min="2" max="30" value={toolSettings.effectIntensity} oninput={(event) => updateSettings({ effectIntensity: Number(event.currentTarget.value) }, false)} onchange={styleSelected} /></label>
          {:else if activeTool === "eraser"}
            <label class="range"><span>大小</span><input type="range" min="8" max="100" value={toolSettings.eraserSize} oninput={(event) => updateSettings({ eraserSize: Number(event.currentTarget.value) }, false)} /></label>
          {:else if activeTool === "text"}
            <label class="range"><span>字号</span><input type="range" min="12" max="72" value={toolSettings.fontSize} oninput={(event) => updateSettings({ fontSize: Number(event.currentTarget.value) }, false)} onchange={styleSelected} /></label>
          {/if}
        </div>
      {/if}
      </div>
    {/if}
  {/if}

  {#if choosingScrollAnchor}
    <div class="long-anchor-bar ui-layer" role="toolbar" aria-label="选择自动滚动区域">
      <strong>点击选区内要自动滚动的正文</strong>
      <span>{pendingLongMode === "top" ? "将先回到顶部" : "从当前位置开始"}</span>
      <button class="anchor-center-action" type="button" title="使用选区中心 Enter" onclick={() => void startLongCapture()}>选区中心</button>
      <button class="icon-control" type="button" title="返回截图 Esc" aria-label="取消选择自动滚动区域" onclick={dismissLongCapturePreparation}><X size={17} /></button>
    </div>
  {/if}

  {#if longStatus && longStatus.state !== "ready"}
    <LongCaptureControl
      jobId={longStatus.jobId}
      initialStatus={longStatus}
      {api}
      floating
      monitor={false}
      cancelLabel="返回普通截图"
      onstatus={applyLongCaptureStatus}
      oncancel={resetLongCapture}
      onerror={reportError}
      onbusychange={(value) => { longBusy = value; }}
    />
  {:else if longBusy && !longStatus}
    <div class="long-progress ui-layer" role="status"><LoaderCircle class="spin" size={17} /><strong>长截图</strong><span class="long-stats">正在启动</span><button class="icon-control" type="button" title="取消 Esc" aria-label="取消长截图" onclick={() => void cancelLongCaptureToEditor()}><X size={17} /></button></div>
  {/if}

  {#if longConfirmOpen}
    <div class="long-confirm-backdrop ui-layer">
      <div class="long-confirm" role="alertdialog" aria-modal="true" aria-labelledby="long-confirm-title">
        <strong id="long-confirm-title">清除标注并开始长截图？</strong>
        <p>长截图开始前需要清除当前选区中的标注。</p>
        <div>
          <button bind:this={longConfirmKeepButton} type="button" onclick={dismissLongCapturePreparation}>保留标注</button>
          <button class="confirm-action" type="button" onclick={confirmLongCapture}>清除并继续</button>
        </div>
      </div>
    </div>
  {/if}

  {#if longStatus?.state === "ready"}
    <section class="long-preview ui-layer" aria-label="长截图预览">
      <header>
        <button class="icon-control" type="button" title="返回普通截图" aria-label="返回普通截图" disabled={longBusy} onclick={() => void cancelLongCaptureToEditor()}><ArrowLeft size={18} /></button>
        <strong>长截图预览</strong>
        <span>{longStatus.width.toLocaleString()} × {longStatus.height.toLocaleString()} px</span>
        <div class="long-preview-actions">
          <button type="button" title="置顶贴图" aria-label="置顶长截图" disabled={longBusy} onclick={() => void exportLongImage("pin")}><Pin size={18} /></button>
          <button type="button" title={`保存 ${formatShortcut("Ctrl+S")}`} aria-label="保存长截图" disabled={longBusy} onclick={() => void exportLongImage("save")}><Save size={18} /></button>
          <button class="primary-action" type="button" title={`复制 ${formatShortcut("Ctrl+C")}`} aria-label="复制长截图" disabled={longBusy} onclick={() => void exportLongImage("copy")}><Copy size={18} /></button>
        </div>
      </header>
      <div class="long-annotation-tools" role="toolbar" aria-label="长截图标注工具">
        <div class="long-tool-buttons">
          <button class:active={longActiveTool === "shape"} type="button" title="形状" aria-label="长截图形状" onclick={() => activateLongTool("shape")}><Shapes size={18} /></button>
          <button class:active={longActiveTool === "line"} type="button" title="线条和箭头" aria-label="长截图线条和箭头" onclick={() => activateLongTool("line")}><ArrowRight size={18} /></button>
          <button class:active={longActiveTool === "pencil"} type="button" title="铅笔" aria-label="长截图铅笔" onclick={() => activateLongTool("pencil")}><Pencil size={18} /></button>
          <button class:active={longActiveTool === "marker"} type="button" title="马克笔" aria-label="长截图马克笔" onclick={() => activateLongTool("marker")}><Highlighter size={18} /></button>
          <button class:active={longActiveTool === "effect"} type="button" title="马赛克和模糊" aria-label="长截图马赛克和模糊" onclick={() => activateLongTool("effect")}><ScanLine size={18} /></button>
          <button class:active={longActiveTool === "text"} type="button" title="文字" aria-label="长截图文字" onclick={() => activateLongTool("text")}><Type size={18} /></button>
          <button class:active={longActiveTool === "eraser"} type="button" title="橡皮擦" aria-label="长截图橡皮擦" onclick={() => activateLongTool("eraser")}><Eraser size={18} /></button>
          <span class="long-tool-separator"></span>
          <button type="button" title={`撤销 ${formatShortcut("Ctrl+Z")}`} aria-label="撤销长截图标注" disabled={longHistory.past.length === 0} onclick={undoLongAnnotation}><Undo2 size={18} /></button>
          <button type="button" title={`重做 ${formatShortcut("Ctrl+Y")}`} aria-label="重做长截图标注" disabled={longHistory.future.length === 0} onclick={redoLongAnnotation}><Redo2 size={18} /></button>
          {#if longSelectedId}
            <button type="button" title="删除 Delete" aria-label="删除长截图标注" onclick={removeLongSelected}><Trash2 size={18} /></button>
          {/if}
        </div>

        {#if longActiveTool === "shape"}
          <div class="segmented" aria-label="长截图形状类型">
            <button class:active={toolSettings.shape === "rectangle"} type="button" onclick={() => updateSettings({ shape: "rectangle" })}>矩形</button>
            <button class:active={toolSettings.shape === "ellipse"} type="button" onclick={() => updateSettings({ shape: "ellipse" })}>椭圆</button>
          </div>
        {:else if longActiveTool === "line"}
          <div class="segmented" aria-label="长截图线条类型">
            <button class:active={toolSettings.line === "line"} type="button" onclick={() => updateSettings({ line: "line" })}>直线</button>
            <button class:active={toolSettings.line === "arrow"} type="button" onclick={() => updateSettings({ line: "arrow" })}>箭头</button>
            <button class:active={toolSettings.line === "double-arrow"} type="button" onclick={() => updateSettings({ line: "double-arrow" })}>双箭头</button>
          </div>
        {:else if longActiveTool === "marker"}
          <div class="segmented" aria-label="长截图笔头">
            <button class:active={toolSettings.markerTip === "round"} type="button" onclick={() => updateSettings({ markerTip: "round" })}>圆头</button>
            <button class:active={toolSettings.markerTip === "square"} type="button" onclick={() => updateSettings({ markerTip: "square" })}>方头</button>
          </div>
        {:else if longActiveTool === "effect"}
          <div class="segmented" aria-label="长截图效果类型">
            <button class:active={toolSettings.effect === "mosaic"} type="button" onclick={() => updateSettings({ effect: "mosaic" })}>马赛克</button>
            <button class:active={toolSettings.effect === "blur"} type="button" onclick={() => updateSettings({ effect: "blur" })}>模糊</button>
          </div>
          <div class="segmented" aria-label="长截图效果范围">
            <button class:active={toolSettings.effectMode === "brush"} type="button" onclick={() => updateSettings({ effectMode: "brush" })}>画笔</button>
            <button class:active={toolSettings.effectMode === "rectangle"} type="button" onclick={() => updateSettings({ effectMode: "rectangle" })}>区域</button>
          </div>
        {:else if longActiveTool === "eraser"}
          <div class="segmented" aria-label="长截图橡皮范围">
            <button class:active={toolSettings.eraserMode === "brush"} type="button" onclick={() => updateSettings({ eraserMode: "brush" })}>画笔</button>
            <button class:active={toolSettings.eraserMode === "rectangle"} type="button" onclick={() => updateSettings({ eraserMode: "rectangle" })}>区域</button>
          </div>
        {:else if longActiveTool === "text"}
          <select aria-label="长截图字体" value={toolSettings.fontFamily} onchange={(event) => updateSettings({ fontFamily: event.currentTarget.value })}>
            <option>Microsoft YaHei UI</option><option>SimSun</option><option>SimHei</option><option>Segoe UI</option>
          </select>
          <div class="long-text-toggles">
            <button class:active={toolSettings.textBold} type="button" aria-label="长截图粗体" onclick={() => updateSettings({ textBold: !toolSettings.textBold })}><Bold size={15} /></button>
            <button class:active={toolSettings.textItalic} type="button" aria-label="长截图斜体" onclick={() => updateSettings({ textItalic: !toolSettings.textItalic })}><Italic size={15} /></button>
            <button class:active={toolSettings.textUnderline} type="button" aria-label="长截图下划线" onclick={() => updateSettings({ textUnderline: !toolSettings.textUnderline })}><Underline size={15} /></button>
          </div>
        {/if}

        {#if longActiveTool && longActiveTool !== "effect" && longActiveTool !== "eraser"}
          <div class="long-palette" aria-label="长截图颜色">
            {#each colors as color}
              <button
                class:selected={(longActiveTool === "text" ? toolSettings.textColor : toolSettings.strokeColor) === color}
                type="button"
                style:background={color}
                aria-label={`长截图颜色 ${color}`}
                onclick={() => updateSettings(longActiveTool === "text" ? { textColor: color } : { strokeColor: color })}
              ></button>
            {/each}
          </div>
        {/if}

        {#if longActiveTool === "shape" || longActiveTool === "line"}
          <label><span>线宽</span><input type="range" min="1" max="20" value={toolSettings.strokeWidth} oninput={(event) => updateSettings({ strokeWidth: Number(event.currentTarget.value) }, false)} /></label>
        {:else if longActiveTool === "pencil"}
          <label><span>粗细</span><input type="range" min="1" max="30" value={toolSettings.pencilWidth} oninput={(event) => updateSettings({ pencilWidth: Number(event.currentTarget.value) }, false)} /></label>
        {:else if longActiveTool === "marker"}
          <label><span>粗细</span><input type="range" min="6" max="64" value={toolSettings.markerWidth} oninput={(event) => updateSettings({ markerWidth: Number(event.currentTarget.value) }, false)} /></label>
        {:else if longActiveTool === "effect"}
          <label><span>范围</span><input type="range" min="8" max="100" value={toolSettings.effectSize} oninput={(event) => updateSettings({ effectSize: Number(event.currentTarget.value) }, false)} /></label>
          <label><span>强度</span><input type="range" min="2" max="30" value={toolSettings.effectIntensity} oninput={(event) => updateSettings({ effectIntensity: Number(event.currentTarget.value) }, false)} /></label>
        {:else if longActiveTool === "eraser"}
          <label><span>大小</span><input type="range" min="8" max="100" value={toolSettings.eraserSize} oninput={(event) => updateSettings({ eraserSize: Number(event.currentTarget.value) }, false)} /></label>
        {:else if longActiveTool === "text"}
          <label><span>字号</span><input type="range" min="12" max="72" value={toolSettings.fontSize} oninput={(event) => updateSettings({ fontSize: Number(event.currentTarget.value) }, false)} /></label>
        {/if}
      </div>
      <div
        class="long-preview-scroll"
        bind:this={longPreviewElement}
        onscroll={(event) => (longPreviewScrollTop = event.currentTarget.scrollTop)}
      >
        <div
          class="long-preview-content"
          class:drawing={!!longActiveTool}
          role="application"
          aria-label="长截图标注画布"
          tabindex="-1"
          style={`width:${Math.max(1, longStatus.width * longPreviewScale)}px;height:${Math.max(1, longStatus.height * longPreviewScale)}px`}
          onpointerdown={handleLongPointerDown}
          onpointermove={handleLongPointerMove}
          onpointerup={handleLongPointerUp}
          onpointercancel={handleLongPointerCancel}
        >
          {#each longVisibleTiles as range (range.y)}
            {#if longTiles[range.y]}
              <canvas
                use:renderLongTile={{
                  tile: longTiles[range.y],
                  width: longStatus.width,
                  annotations: longAnnotations,
                  draft: longDraft,
                  selectedId: longSelectedId,
                }}
                aria-label={`长截图瓦片 ${range.y}`}
                style={`top:${range.y * longPreviewScale}px;width:${longStatus.width * longPreviewScale}px;height:${range.height * longPreviewScale}px`}
              ></canvas>
            {:else}
              <div class="long-tile-loading" style={`top:${range.y * longPreviewScale}px;height:${range.height * longPreviewScale}px`}><LoaderCircle class="spin" size={18} /></div>
            {/if}
          {/each}
          {#if longTextDraft}
            <textarea
              class="long-text-editor"
              bind:this={longTextAreaElement}
              bind:value={longTextDraft.value}
              style={`left:${longTextDraft.annotation.rect.x * longPreviewScale}px;top:${longTextDraft.annotation.rect.y * longPreviewScale}px;width:${longTextDraft.annotation.rect.width * longPreviewScale}px;height:${longTextDraft.annotation.rect.height * longPreviewScale}px;font-family:${longTextDraft.annotation.fontFamily};font-size:${Math.max(12, longTextDraft.annotation.fontSize * longPreviewScale)}px;color:${longTextDraft.annotation.color};font-weight:${longTextDraft.annotation.bold ? 700 : 400};font-style:${longTextDraft.annotation.italic ? "italic" : "normal"};text-decoration:${longTextDraft.annotation.underline ? "underline" : "none"}`}
              aria-label="输入长截图标注文字"
              placeholder="输入文字"
              onblur={() => finishLongText(true)}
              onpointerdown={(event) => event.stopPropagation()}
            ></textarea>
          {/if}
        </div>
      </div>
      {#if longBusy}<div class="long-preview-busy"><LoaderCircle class="spin" size={17} />正在处理…</div>{/if}
    </section>
  {/if}

  {#if hoverPoint && !interaction && !busy && !textDraft && !longCaptureActive}
    <div class="magnifier ui-layer" style={magnifierStyle()} aria-live="polite">
      <div class="magnified-pixels"><canvas bind:this={magnifierCanvas} width="132" height="108"></canvas><span></span></div>
      <div class="pixel-data">
        <strong>{colorText()}</strong>
        <span>{session ? session.monitor.x + Math.round(hoverPoint.x) : 0}, {session ? session.monitor.y + Math.round(hoverPoint.y) : 0}</span>
      </div>
      <small>C 复制 · {formatShortcut("Shift")} 切换</small>
    </div>
  {/if}

  {#if loading}
    <div class="status-overlay ui-layer"><LoaderCircle class="spin" size={28} /><span>正在准备截图…</span></div>
  {:else if error && !session}
    <div class="status-overlay error-state ui-layer">
      <strong>无法开始截图</strong>
      <span>{error}</span>
      <div class="error-actions">
        <button type="button" onclick={() => requestLoad(pendingSessionId ?? undefined)}>重试</button>
        <button type="button" onclick={() => void cancel()}>关闭</button>
      </div>
    </div>
  {:else if !session}
    <div class="status-overlay idle-state ui-layer"><span>截图工具已就绪</span></div>
  {/if}

  {#if busy}
    <div class="busy-indicator ui-layer"><LoaderCircle class="spin" size={18} /><span>正在处理…</span></div>
  {/if}
  {#if error && session}<div class="error-toast ui-layer" role="alert">{error}</div>{/if}
  {#if toast}<div class="toast ui-layer" role="status">{toast}</div>{/if}
</div>

<style>
  :global(html.screenshot-tool-page), :global(body.screenshot-tool-page) { width: 100%; height: 100%; padding: 0; margin: 0; overflow: hidden; background: #000; }
  :global(body.screenshot-tool-page) { user-select: none; }
  .screenshot-tool { position: fixed; z-index: 0; inset: 0; width: 100%; height: 100%; overflow: hidden; color: #f8f8f8; background: #000; outline: none; cursor: crosshair; touch-action: none; }
  .screenshot-tool.selection-movable { cursor: move; }
  .capture-canvas { position: absolute; z-index: 0; inset: 0; display: block; width: 100%; height: 100%; }
  .source-canvas { position: fixed; width: 1px; height: 1px; opacity: 0; pointer-events: none; }
  .selection-layer { position: absolute; z-index: 2; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
  .dim-mask { fill: rgb(0 0 0 / 52%); pointer-events: none; }
  .selection-border { fill: none; stroke: #fff; stroke-width: 1.5; vector-effect: non-scaling-stroke; filter: drop-shadow(0 0 1px rgb(0 0 0 / 80%)); }
  .resize-handle, .annotation-handle { fill: #fff; stroke: #0a84ff; stroke-width: 1.5; vector-effect: non-scaling-stroke; pointer-events: auto; }
  .annotation-handle { fill: #0a84ff; stroke: #fff; }
  .annotation-border { fill: none; stroke: #0a84ff; stroke-width: 1; stroke-dasharray: 6 4; vector-effect: non-scaling-stroke; pointer-events: none; }
  .handle-n, .handle-s { cursor: ns-resize; } .handle-e, .handle-w { cursor: ew-resize; } .handle-ne, .handle-sw { cursor: nesw-resize; } .handle-nw, .handle-se { cursor: nwse-resize; }
  .selection-size { position: absolute; z-index: 4; width: max-content !important; height: auto !important; padding: 4px 7px; color: #fff; background: rgb(17 17 17 / 88%); border-radius: 3px; font: 600 12px/1.2 "Segoe UI", sans-serif; pointer-events: none; transform: translateY(-100%); }
  .toolbar-dock { position: absolute; z-index: 20; display: grid; width: max-content; max-height: min(170px, calc(100vh - 16px)); color: #242424; background: rgb(250 250 250 / 98%); border: 1px solid rgb(0 0 0 / 22%); border-radius: 6px; box-shadow: 0 6px 20px rgb(0 0 0 / 28%); cursor: default; overflow: auto; }
  .primary-tools { display: flex; min-width: max-content; height: 46px; padding: 5px; align-items: center; gap: 2px; }
  .primary-tools > button, .text-toggles button { display: grid; width: 35px; height: 35px; padding: 0; place-items: center; color: #2d2d2d; background: transparent; border: 1px solid transparent; border-radius: 4px; cursor: default; }
  .primary-tools > button:hover:not(:disabled), .text-toggles button:hover { background: #e7e7e7; border-color: #d0d0d0; }
  .primary-tools > button.active, .text-toggles button.active { color: #fff; background: #0067c0; border-color: #005a9e; }
  .primary-tools > button.primary-action { color: #fff; background: #0067c0; border-color: #005a9e; }
  .primary-tools > button.long-mode-toggle { width: 25px; margin-left: -2px; }
  .primary-tools > button:disabled { opacity: .38; }
  .separator { width: 1px; height: 25px; margin: 0 3px; background: #d2d2d2; }
  .tool-options { display: flex; min-width: max-content; min-height: 43px; padding: 5px 8px; align-items: center; gap: 9px; border-top: 1px solid #d8d8d8; }
  .segmented { display: inline-flex; align-items: center; }
  .segmented button { min-height: 28px; padding: 4px 8px; color: #333; background: #fff; border: 1px solid #bbb; border-right-width: 0; font-size: 12px; }
  .segmented button:first-child { border-radius: 4px 0 0 4px; } .segmented button:last-child { border-right-width: 1px; border-radius: 0 4px 4px 0; }
  .segmented button.active { color: #fff; background: #0067c0; border-color: #005a9e; }
  .tool-options label { display: inline-flex; align-items: center; gap: 5px; color: #4c4c4c; font-size: 12px; white-space: nowrap; }
  .tool-options select { height: 29px; max-width: 155px; border: 1px solid #bbb; border-radius: 4px; background: #fff; }
  .long-mode-options strong { font-size: 12px; white-space: nowrap; }
  .palette { display: flex; gap: 4px; }
  .palette button { width: 22px; height: 22px; padding: 0; border: 1px solid rgb(0 0 0 / 28%); border-radius: 50%; box-shadow: inset 0 0 0 1px rgb(255 255 255 / 45%); }
  .palette button.selected { box-shadow: 0 0 0 2px #fafafa, 0 0 0 4px #0067c0; }
  .range input { width: 92px; accent-color: #0067c0; }
  .text-toggles { display: inline-flex; }
  .text-toggles button { width: 29px; height: 29px; }
  .text-editor { position: absolute; z-index: 15; box-sizing: border-box; min-width: 36px; min-height: 26px; padding: 4px 6px; resize: none; color: #ff3b30; background: rgb(255 255 255 / 92%); border: 1px solid #0a84ff; outline: 1px solid #fff; overflow: hidden; user-select: text; }
  .long-anchor-bar, .long-progress { position: absolute; z-index: 40; top: 12px; left: 50%; display: flex; box-sizing: border-box; min-height: 44px; max-width: calc(100vw - 24px); padding: 5px 7px 5px 11px; align-items: center; gap: 8px; color: #242424; background: rgb(250 250 250 / 98%); border: 1px solid rgb(0 0 0 / 25%); border-radius: 6px; box-shadow: 0 6px 20px rgb(0 0 0 / 28%); transform: translateX(-50%); cursor: default; font: 12px "Segoe UI", sans-serif; }
  .long-anchor-bar { min-width: min(430px, calc(100vw - 24px)); }
  .long-anchor-bar strong, .long-progress strong { flex: 0 0 auto; font-size: 12px; }
  .long-anchor-bar > span { min-width: 0; margin-left: auto; color: #666; white-space: nowrap; }
  .anchor-center-action { flex: 0 0 auto; height: 30px; padding: 0 10px; color: #2d2d2d; background: #fff; border: 1px solid #c8c8c8; border-radius: 4px; white-space: nowrap; }
  .anchor-center-action:hover { background: #e7e7e7; border-color: #aaa; }
  .long-progress { min-width: min(540px, calc(100vw - 24px)); }
  .long-stats { flex: 0 0 auto; color: #555; white-space: nowrap; }
  .long-message { min-width: 0; max-width: 220px; color: #6b4d18; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .long-control-separator { width: 1px; height: 24px; margin-left: auto; background: #d2d2d2; }
  .icon-control, .long-preview-actions button { display: grid; flex: 0 0 auto; width: 32px; height: 32px; padding: 0; place-items: center; color: #2d2d2d; background: transparent; border: 1px solid transparent; border-radius: 4px; }
  .icon-control:hover:not(:disabled), .long-preview-actions button:hover:not(:disabled) { background: #e7e7e7; border-color: #d0d0d0; }
  .icon-control:disabled, .long-preview-actions button:disabled { opacity: .42; }
  .icon-control.finish-control, .long-preview-actions button.primary-action { color: #fff; background: #0067c0; border-color: #005a9e; }
  .long-confirm-backdrop { position: absolute; z-index: 90; inset: 0; display: grid; padding: 16px; place-items: center; background: rgb(0 0 0 / 42%); cursor: default; }
  .long-confirm { width: min(360px, calc(100vw - 32px)); box-sizing: border-box; padding: 18px; color: #242424; background: #fff; border: 1px solid #c8c8c8; border-radius: 7px; box-shadow: 0 10px 30px rgb(0 0 0 / 30%); font: 13px/1.45 "Segoe UI", sans-serif; }
  .long-confirm > strong { display: block; font-size: 16px; }
  .long-confirm p { margin: 8px 0 18px; color: #666; }
  .long-confirm > div { display: flex; justify-content: flex-end; gap: 8px; }
  .long-confirm button { min-height: 32px; padding: 5px 11px; color: #242424; background: #fff; border: 1px solid #b9b9b9; border-radius: 4px; }
  .long-confirm button.confirm-action { color: #fff; background: #0067c0; border-color: #005a9e; }
  .long-preview { position: absolute; z-index: 70; inset: 0; display: grid; grid-template-rows: 50px 46px minmax(0, 1fr); color: #242424; background: #1d1d1d; cursor: default; }
  .long-preview > header { display: flex; min-width: 0; padding: 0 10px; align-items: center; gap: 9px; background: #fafafa; border-bottom: 1px solid #c8c8c8; box-shadow: 0 2px 8px rgb(0 0 0 / 22%); }
  .long-preview > header > strong { font-size: 13px; }
  .long-preview > header > span { color: #666; font: 12px "Segoe UI", sans-serif; white-space: nowrap; }
  .long-preview-actions { display: flex; margin-left: auto; gap: 3px; }
  .long-annotation-tools { display: flex; min-width: 0; padding: 5px 8px; align-items: center; gap: 8px; background: #f4f4f4; border-bottom: 1px solid #cfcfcf; overflow-x: auto; overflow-y: hidden; scrollbar-width: thin; }
  .long-tool-buttons, .long-text-toggles { display: flex; flex: 0 0 auto; align-items: center; gap: 2px; }
  .long-tool-buttons > button, .long-text-toggles button { display: grid; width: 32px; height: 32px; padding: 0; place-items: center; color: #2d2d2d; background: transparent; border: 1px solid transparent; border-radius: 4px; }
  .long-tool-buttons > button:hover:not(:disabled), .long-text-toggles button:hover { background: #e4e4e4; border-color: #ccc; }
  .long-tool-buttons > button.active, .long-text-toggles button.active { color: #fff; background: #0067c0; border-color: #005a9e; }
  .long-tool-buttons > button:disabled { opacity: .38; }
  .long-tool-separator { width: 1px; height: 24px; margin: 0 3px; background: #ccc; }
  .long-annotation-tools select { flex: 0 0 auto; height: 29px; max-width: 150px; background: #fff; border: 1px solid #bbb; border-radius: 4px; }
  .long-annotation-tools label { display: flex; flex: 0 0 auto; align-items: center; gap: 5px; color: #4c4c4c; font-size: 12px; white-space: nowrap; }
  .long-annotation-tools label input { width: 82px; accent-color: #0067c0; }
  .long-palette { display: flex; flex: 0 0 auto; gap: 4px; }
  .long-palette button { width: 19px; height: 19px; padding: 0; border: 1px solid rgb(0 0 0 / 28%); border-radius: 50%; box-shadow: inset 0 0 0 1px rgb(255 255 255 / 45%); }
  .long-palette button.selected { box-shadow: 0 0 0 2px #f4f4f4, 0 0 0 4px #0067c0; }
  .long-preview-scroll { min-width: 0; min-height: 0; padding: 16px 0 28px; overflow: auto; scrollbar-gutter: stable; }
  .long-preview-content { position: relative; margin: 0 auto; overflow: hidden; background: #f4f4f4; box-shadow: 0 4px 20px rgb(0 0 0 / 45%); touch-action: none; }
  .long-preview-content.drawing { cursor: crosshair; }
  .long-preview-content canvas, .long-tile-loading { position: absolute; left: 0; display: block; }
  .long-tile-loading { display: grid; width: 100%; place-items: center; color: #777; background: #ededed; }
  .long-text-editor { position: absolute; z-index: 3; box-sizing: border-box; min-width: 36px; min-height: 26px; padding: 4px 6px; resize: none; color: #ff3b30; background: rgb(255 255 255 / 94%); border: 1px solid #0a84ff; outline: 1px solid #fff; overflow: hidden; user-select: text; }
  .long-preview-busy { position: absolute; z-index: 2; top: 62px; left: 50%; display: flex; padding: 7px 11px; align-items: center; gap: 7px; color: #fff; background: rgb(22 22 22 / 92%); border-radius: 4px; transform: translateX(-50%); font: 12px "Segoe UI", sans-serif; }
  .magnifier { position: absolute; z-index: 30; width: 152px; padding: 7px; color: #f7f7f7; background: rgb(20 20 20 / 94%); border: 1px solid rgb(255 255 255 / 35%); border-radius: 5px; box-shadow: 0 5px 18px rgb(0 0 0 / 35%); font: 12px/1.35 "Segoe UI", sans-serif; pointer-events: none; }
  .magnified-pixels { position: relative; width: 132px; height: 108px; margin: 0 auto; overflow: hidden; border: 1px solid #777; }
  .magnified-pixels canvas { display: block; width: 132px; height: 108px; }
  .magnified-pixels span::before, .magnified-pixels span::after { position: absolute; content: ""; background: #f22; box-shadow: 0 0 0 1px rgb(255 255 255 / 70%); }
  .magnified-pixels span::before { top: 53px; left: 0; width: 100%; height: 1px; } .magnified-pixels span::after { top: 0; left: 65px; width: 1px; height: 100%; }
  .pixel-data { display: flex; margin-top: 6px; justify-content: space-between; gap: 6px; } .pixel-data strong { font-size: 12px; } .pixel-data span { color: #c9c9c9; }
  .magnifier small { display: block; margin-top: 3px; color: #aaa; font-size: 10px; }
  .status-overlay { position: absolute; z-index: 50; inset: 0; display: grid; place-content: center; justify-items: center; gap: 10px; color: #fff; background: #161616; font: 13px "Segoe UI", sans-serif; }
  .status-overlay.error-state { text-align: center; } .status-overlay.error-state strong { font-size: 17px; } .status-overlay.error-state span { max-width: 420px; color: #ccc; }
  .error-actions { display: flex; gap: 8px; }
  .status-overlay button { min-width: 76px; padding: 7px 13px; color: #222; background: #fff; border: 0; border-radius: 4px; }
  .busy-indicator, .toast, .error-toast { position: absolute; z-index: 110; left: 50%; display: flex; padding: 8px 12px; align-items: center; gap: 7px; color: #fff; background: rgb(22 22 22 / 92%); border: 1px solid rgb(255 255 255 / 25%); border-radius: 4px; box-shadow: 0 5px 16px rgb(0 0 0 / 28%); transform: translateX(-50%); font: 12px "Segoe UI", sans-serif; }
  .busy-indicator { top: 18px; } .toast { bottom: 24px; } .error-toast { bottom: 24px; max-width: min(540px, calc(100vw - 32px)); background: rgb(150 34 27 / 96%); }
  .spin { animation: spin .8s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 800px) { .toolbar-dock { width: calc(100vw - 16px); } .primary-tools, .tool-options { overflow-x: auto; } .long-progress, .long-anchor-bar { width: calc(100vw - 24px); min-width: 0; overflow-x: auto; } .long-message { display: none; } }
  @media (max-width: 520px) { .long-preview > header > span { display: none; } }
</style>

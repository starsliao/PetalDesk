import type { LongCaptureCapability, LongCaptureEngine, LongCaptureState, LongCaptureStatus } from "./types";

export const LONG_CAPTURE_TILE_HEIGHT = 1024;

export interface LongCaptureTileRange {
  y: number;
  height: number;
}

const captureStates = new Set<LongCaptureState>(["preparing", "capturing", "paused", "ready", "failed", "canceled"]);
const captureEngines = new Set<LongCaptureEngine>(["browserEnhanced", "uiAutomation", "wheel", "manual"]);

export function normalizeLongCaptureCapability(value: LongCaptureCapability & { supported?: boolean }): LongCaptureCapability {
  return {
    ...value,
    available: typeof value.available === "boolean" ? value.available : value.supported === true,
  };
}

export function normalizeLongCaptureStatus(value: LongCaptureStatus): LongCaptureStatus {
  const rawState = String(value.state);
  const state = rawState === "reviewing" ? "ready" : rawState;
  if (!captureStates.has(state as LongCaptureState)) throw new Error("长截图状态无效，请重试。");
  if (!captureEngines.has(value.engine)) throw new Error("长截图采集引擎无效，请重试。");
  return {
    ...value,
    state: state as LongCaptureState,
    frameCount: numeric(value.frameCount),
    width: numeric(value.width),
    height: numeric(value.height),
    canUndo: value.canUndo === true,
  };
}

interface VisibleTileOptions {
  totalHeight: number;
  scrollTop: number;
  viewportHeight: number;
  scale: number;
  tileHeight?: number;
  overscan?: number;
}

export function visibleLongCaptureTiles({
  totalHeight,
  scrollTop,
  viewportHeight,
  scale,
  tileHeight = LONG_CAPTURE_TILE_HEIGHT,
  overscan = 1,
}: VisibleTileOptions): LongCaptureTileRange[] {
  if (totalHeight <= 0 || scale <= 0 || tileHeight <= 0) return [];
  const safeScrollTop = Math.max(0, scrollTop);
  const firstVisible = Math.floor(safeScrollTop / scale / tileHeight);
  const lastVisible = Math.floor(Math.max(0, safeScrollTop + Math.max(1, viewportHeight) - 1) / scale / tileHeight);
  const first = Math.max(0, firstVisible - Math.max(0, overscan));
  const lastTile = Math.max(0, Math.ceil(totalHeight / tileHeight) - 1);
  const last = Math.min(lastTile, lastVisible + Math.max(0, overscan));
  const ranges: LongCaptureTileRange[] = [];
  for (let index = first; index <= last; index += 1) {
    const y = index * tileHeight;
    ranges.push({ y, height: Math.min(tileHeight, totalHeight - y) });
  }
  return ranges;
}

export function longCaptureStatusFromEvent(
  payload: unknown,
  previous: LongCaptureStatus | null,
  fallbackState?: LongCaptureStatus["state"],
): LongCaptureStatus | null {
  if (!payload || typeof payload !== "object") return previous;
  const value = "status" in payload && payload.status && typeof payload.status === "object"
    ? payload.status as Record<string, unknown>
    : payload as Record<string, unknown>;
  const jobId = typeof value.jobId === "string" ? value.jobId : previous?.jobId;
  const sessionId = typeof value.sessionId === "string" ? value.sessionId : previous?.sessionId;
  if (!jobId || !sessionId) return previous;
  const rawState = typeof value.state === "string" ? value.state : fallbackState ?? previous?.state;
  const state = rawState === "reviewing" ? "ready" : rawState;
  const engine = typeof value.engine === "string" ? value.engine : previous?.engine;
  if (!state || !engine || !captureStates.has(state as LongCaptureState) || !captureEngines.has(engine as LongCaptureEngine)) return previous;
  return {
    jobId,
    sessionId,
    state: state as LongCaptureStatus["state"],
    engine: engine as LongCaptureStatus["engine"],
    frameCount: numeric(value.frameCount, previous?.frameCount),
    width: numeric(value.width, previous?.width),
    height: numeric(value.height, previous?.height),
    message: typeof value.message === "string" || value.message === null ? value.message : previous?.message,
    canUndo: typeof value.canUndo === "boolean" ? value.canUndo : previous?.canUndo ?? false,
  };
}

function numeric(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.max(0, value) : fallback;
}

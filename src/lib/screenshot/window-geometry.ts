import type { ScreenshotSession } from "./types";

export interface ScreenshotWindowGeometry {
  position?: { x: number; y: number };
  size?: { width: number; height: number };
  scaleFactor?: number;
}

export function matchesScreenshotWindow(
  session: ScreenshotSession,
  geometry: ScreenshotWindowGeometry,
): boolean {
  if (geometry.position
    && (geometry.position.x !== session.monitor.x || geometry.position.y !== session.monitor.y)) return false;
  if (geometry.size
    && (geometry.size.width !== session.monitor.width || geometry.size.height !== session.monitor.height)) return false;
  if (geometry.scaleFactor !== undefined
    && Math.abs(geometry.scaleFactor - session.monitor.scaleFactor) > 0.001) return false;
  return true;
}

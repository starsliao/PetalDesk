import type { Annotation, Point, Rect, ResizeHandle } from "./types";

export const MIN_SELECTION_SIZE = 8;

export function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

export function normalizeRect(from: Point, to: Point): Rect {
  const left = Math.min(from.x, to.x);
  const top = Math.min(from.y, to.y);
  return {
    x: left,
    y: top,
    width: Math.abs(to.x - from.x),
    height: Math.abs(to.y - from.y),
  };
}

export function roundRect(rect: Rect): Rect {
  const x = Math.round(rect.x);
  const y = Math.round(rect.y);
  return {
    x,
    y,
    width: Math.max(0, Math.round(rect.x + rect.width) - x),
    height: Math.max(0, Math.round(rect.y + rect.height) - y),
  };
}

export function clampPoint(point: Point, bounds: Rect): Point {
  return {
    x: clamp(point.x, bounds.x, bounds.x + bounds.width),
    y: clamp(point.y, bounds.y, bounds.y + bounds.height),
  };
}

export function clampRect(rect: Rect, bounds: Rect, minSize = MIN_SELECTION_SIZE): Rect {
  const width = clamp(rect.width, Math.min(minSize, bounds.width), bounds.width);
  const height = clamp(rect.height, Math.min(minSize, bounds.height), bounds.height);
  return {
    x: clamp(rect.x, bounds.x, bounds.x + bounds.width - width),
    y: clamp(rect.y, bounds.y, bounds.y + bounds.height - height),
    width,
    height,
  };
}

export function moveRect(rect: Rect, delta: Point, bounds: Rect): Rect {
  return clampRect({ ...rect, x: rect.x + delta.x, y: rect.y + delta.y }, bounds, 1);
}

export function resizeRect(
  initial: Rect,
  handle: ResizeHandle,
  point: Point,
  bounds: Rect,
  minSize = MIN_SELECTION_SIZE,
): Rect {
  let left = initial.x;
  let top = initial.y;
  let right = initial.x + initial.width;
  let bottom = initial.y + initial.height;
  const clamped = clampPoint(point, bounds);

  if (handle.includes("w")) left = Math.min(clamped.x, right - minSize);
  if (handle.includes("e")) right = Math.max(clamped.x, left + minSize);
  if (handle.includes("n")) top = Math.min(clamped.y, bottom - minSize);
  if (handle.includes("s")) bottom = Math.max(clamped.y, top + minSize);

  return roundRect(clampRect({ x: left, y: top, width: right - left, height: bottom - top }, bounds, minSize));
}

export function pointInRect(point: Point, rect: Rect, padding = 0): boolean {
  return point.x >= rect.x - padding
    && point.x <= rect.x + rect.width + padding
    && point.y >= rect.y - padding
    && point.y <= rect.y + rect.height + padding;
}

export function distanceToSegment(point: Point, from: Point, to: Point): number {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  if (dx === 0 && dy === 0) return Math.hypot(point.x - from.x, point.y - from.y);
  const t = clamp(((point.x - from.x) * dx + (point.y - from.y) * dy) / (dx * dx + dy * dy), 0, 1);
  return Math.hypot(point.x - (from.x + t * dx), point.y - (from.y + t * dy));
}

export function annotationBounds(annotation: Annotation): Rect {
  if (annotation.kind === "shape" || annotation.kind === "text") return { ...annotation.rect };
  if (annotation.kind === "line") return normalizeRect(annotation.from, annotation.to);
  if ("width" in annotation) {
    const points = annotation.points;
    if (points.length === 0) return { x: 0, y: 0, width: 0, height: 0 };
    let minX = points[0].x;
    let maxX = minX;
    let minY = points[0].y;
    let maxY = minY;
    for (const point of points.slice(1)) {
      minX = Math.min(minX, point.x);
      maxX = Math.max(maxX, point.x);
      minY = Math.min(minY, point.y);
      maxY = Math.max(maxY, point.y);
    }
    const radius = annotation.width / 2;
    return { x: minX - radius, y: minY - radius, width: maxX - minX + radius * 2, height: maxY - minY + radius * 2 };
  }
  if ((annotation.kind === "effect" || annotation.kind === "eraser") && annotation.mode === "rectangle") {
    return { ...annotation.rect };
  }
  const points = annotation.points;
  if (points.length === 0) return { ...annotation.rect };
  let minX = points[0].x;
  let maxX = minX;
  let minY = points[0].y;
  let maxY = minY;
  for (const point of points.slice(1)) {
    minX = Math.min(minX, point.x);
    maxX = Math.max(maxX, point.x);
    minY = Math.min(minY, point.y);
    maxY = Math.max(maxY, point.y);
  }
  const radius = annotation.size / 2;
  return { x: minX - radius, y: minY - radius, width: maxX - minX + radius * 2, height: maxY - minY + radius * 2 };
}

export function hitTestAnnotation(annotation: Annotation, point: Point, tolerance = 7): boolean {
  if (annotation.kind === "shape") {
    if (annotation.fill && pointInRect(point, annotation.rect)) return true;
    if (!pointInRect(point, annotation.rect, tolerance)) return false;
    if (annotation.shape === "ellipse") {
      const rx = Math.max(1, annotation.rect.width / 2);
      const ry = Math.max(1, annotation.rect.height / 2);
      const nx = (point.x - annotation.rect.x - rx) / rx;
      const ny = (point.y - annotation.rect.y - ry) / ry;
      return annotation.fill !== null || Math.abs(nx * nx + ny * ny - 1) <= tolerance / Math.min(rx, ry);
    }
    const inner = {
      x: annotation.rect.x + tolerance,
      y: annotation.rect.y + tolerance,
      width: Math.max(0, annotation.rect.width - tolerance * 2),
      height: Math.max(0, annotation.rect.height - tolerance * 2),
    };
    return !pointInRect(point, inner);
  }
  if (annotation.kind === "line") {
    return distanceToSegment(point, annotation.from, annotation.to) <= Math.max(tolerance, annotation.stroke.width / 2);
  }
  if (annotation.kind === "text") return pointInRect(point, annotation.rect, tolerance);
  if ("width" in annotation) {
    const radius = annotation.width / 2;
    for (let index = 1; index < annotation.points.length; index += 1) {
      if (distanceToSegment(point, annotation.points[index - 1], annotation.points[index]) <= radius + tolerance) return true;
    }
    return annotation.points.length === 1
      && Math.hypot(point.x - annotation.points[0].x, point.y - annotation.points[0].y) <= radius + tolerance;
  }
  if ((annotation.kind === "effect" || annotation.kind === "eraser") && annotation.mode === "rectangle") {
    return pointInRect(point, annotation.rect, tolerance);
  }
  const radius = annotation.size / 2;
  for (let index = 1; index < annotation.points.length; index += 1) {
    if (distanceToSegment(point, annotation.points[index - 1], annotation.points[index]) <= radius + tolerance) return true;
  }
  return annotation.points.length === 1 && Math.hypot(point.x - annotation.points[0].x, point.y - annotation.points[0].y) <= radius + tolerance;
}

export function translateAnnotation(annotation: Annotation, delta: Point): Annotation {
  if (annotation.kind === "shape" || annotation.kind === "text") {
    return { ...annotation, rect: { ...annotation.rect, x: annotation.rect.x + delta.x, y: annotation.rect.y + delta.y } };
  }
  if (annotation.kind === "line") {
    return {
      ...annotation,
      from: { x: annotation.from.x + delta.x, y: annotation.from.y + delta.y },
      to: { x: annotation.to.x + delta.x, y: annotation.to.y + delta.y },
    };
  }
  if ("width" in annotation) {
    return {
      ...annotation,
      points: annotation.points.map((point) => ({ x: point.x + delta.x, y: point.y + delta.y })),
    };
  }
  return {
    ...annotation,
    rect: { ...annotation.rect, x: annotation.rect.x + delta.x, y: annotation.rect.y + delta.y },
    points: annotation.points.map((point) => ({ x: point.x + delta.x, y: point.y + delta.y })),
  };
}

export function scaleAnnotation(annotation: Annotation, from: Rect, to: Rect): Annotation {
  const sx = from.width === 0 ? 1 : to.width / from.width;
  const sy = from.height === 0 ? 1 : to.height / from.height;
  const map = (point: Point): Point => ({
    x: to.x + (point.x - from.x) * sx,
    y: to.y + (point.y - from.y) * sy,
  });
  if (annotation.kind === "shape" || annotation.kind === "text") {
    const topLeft = map({ x: annotation.rect.x, y: annotation.rect.y });
    const bottomRight = map({ x: annotation.rect.x + annotation.rect.width, y: annotation.rect.y + annotation.rect.height });
    return { ...annotation, rect: normalizeRect(topLeft, bottomRight) };
  }
  if (annotation.kind === "line") return { ...annotation, from: map(annotation.from), to: map(annotation.to) };
  if ("width" in annotation) {
    return { ...annotation, points: annotation.points.map(map) };
  }
  const topLeft = map({ x: annotation.rect.x, y: annotation.rect.y });
  const bottomRight = map({ x: annotation.rect.x + annotation.rect.width, y: annotation.rect.y + annotation.rect.height });
  return { ...annotation, rect: normalizeRect(topLeft, bottomRight), points: annotation.points.map(map) };
}

export function selectionHandlePoints(rect: Rect): Record<ResizeHandle, Point> {
  const centerX = rect.x + rect.width / 2;
  const centerY = rect.y + rect.height / 2;
  return {
    nw: { x: rect.x, y: rect.y },
    n: { x: centerX, y: rect.y },
    ne: { x: rect.x + rect.width, y: rect.y },
    e: { x: rect.x + rect.width, y: centerY },
    se: { x: rect.x + rect.width, y: rect.y + rect.height },
    s: { x: centerX, y: rect.y + rect.height },
    sw: { x: rect.x, y: rect.y + rect.height },
    w: { x: rect.x, y: centerY },
  };
}

export interface ToolbarPlacement {
  left: number;
  top: number;
  side: "above" | "below";
}

export function placeToolbar(
  selection: Rect,
  viewport: Rect,
  toolbarWidth: number,
  toolbarHeight: number,
  gap = 10,
): ToolbarPlacement {
  const left = clamp(selection.x + selection.width - toolbarWidth, viewport.x + gap, viewport.x + viewport.width - toolbarWidth - gap);
  const above = selection.y - toolbarHeight - gap;
  if (above >= viewport.y + gap) return { left, top: above, side: "above" };
  return {
    left,
    top: clamp(selection.y + selection.height + gap, viewport.y + gap, viewport.y + viewport.height - toolbarHeight - gap),
    side: "below",
  };
}

import { annotationBounds, clamp } from "./geometry";
import type {
  Annotation,
  EffectAnnotation,
  EraserAnnotation,
  LineAnnotation,
  PathAnnotation,
  Point,
  Rect,
  ShapeAnnotation,
  TextAnnotation,
} from "./types";

type DrawingContext = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
type DrawingSurface = HTMLCanvasElement | OffscreenCanvas;

function canvas(width: number, height: number): DrawingSurface {
  if (typeof OffscreenCanvas !== "undefined") {
    return new OffscreenCanvas(Math.max(1, Math.ceil(width)), Math.max(1, Math.ceil(height)));
  }
  if (typeof document === "undefined") throw new Error("当前 WebView2 不支持离屏截图合成。");
  const element = document.createElement("canvas");
  element.width = Math.max(1, Math.ceil(width));
  element.height = Math.max(1, Math.ceil(height));
  return element;
}

function context(element: DrawingSurface, readFrequently = false): DrawingContext {
  const value = element.getContext("2d", { willReadFrequently: readFrequently }) as DrawingContext | null;
  if (!value) throw new Error("当前 WebView2 不支持 Canvas 2D。");
  return value;
}

function tracePath(ctx: DrawingContext, points: Point[], size: number, tip: CanvasLineCap = "round"): void {
  if (points.length === 0) return;
  ctx.lineCap = tip;
  ctx.lineJoin = "round";
  ctx.lineWidth = size;
  if (points.length === 1) {
    ctx.beginPath();
    ctx.arc(points[0].x, points[0].y, size / 2, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  ctx.beginPath();
  ctx.moveTo(points[0].x, points[0].y);
  for (const point of points.slice(1)) ctx.lineTo(point.x, point.y);
  ctx.stroke();
}

function drawShape(ctx: DrawingContext, annotation: ShapeAnnotation): void {
  const { rect } = annotation;
  ctx.save();
  ctx.strokeStyle = annotation.stroke.color;
  ctx.lineWidth = annotation.stroke.width;
  ctx.setLineDash(annotation.stroke.lineStyle === "dashed" ? [annotation.stroke.width * 2, annotation.stroke.width * 1.5] : []);
  if (annotation.fill) ctx.fillStyle = annotation.fill;
  ctx.beginPath();
  if (annotation.shape === "ellipse") {
    ctx.ellipse(
      rect.x + rect.width / 2,
      rect.y + rect.height / 2,
      Math.max(0.5, rect.width / 2),
      Math.max(0.5, rect.height / 2),
      0,
      0,
      Math.PI * 2,
    );
  } else {
    ctx.rect(rect.x, rect.y, rect.width, rect.height);
  }
  if (annotation.fill) ctx.fill();
  ctx.stroke();
  ctx.restore();
}

function arrowHead(ctx: DrawingContext, from: Point, to: Point, width: number): void {
  const angle = Math.atan2(to.y - from.y, to.x - from.x);
  const length = Math.max(10, width * 3.2);
  ctx.beginPath();
  ctx.moveTo(to.x, to.y);
  ctx.lineTo(to.x - Math.cos(angle - Math.PI / 6) * length, to.y - Math.sin(angle - Math.PI / 6) * length);
  ctx.moveTo(to.x, to.y);
  ctx.lineTo(to.x - Math.cos(angle + Math.PI / 6) * length, to.y - Math.sin(angle + Math.PI / 6) * length);
  ctx.stroke();
}

function drawLine(ctx: DrawingContext, annotation: LineAnnotation): void {
  ctx.save();
  ctx.strokeStyle = annotation.stroke.color;
  ctx.lineWidth = annotation.stroke.width;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.setLineDash(annotation.stroke.lineStyle === "dashed" ? [annotation.stroke.width * 2, annotation.stroke.width * 1.5] : []);
  ctx.beginPath();
  ctx.moveTo(annotation.from.x, annotation.from.y);
  ctx.lineTo(annotation.to.x, annotation.to.y);
  ctx.stroke();
  ctx.setLineDash([]);
  if (annotation.line === "arrow" || annotation.line === "double-arrow") {
    arrowHead(ctx, annotation.from, annotation.to, annotation.stroke.width);
  }
  if (annotation.line === "double-arrow") arrowHead(ctx, annotation.to, annotation.from, annotation.stroke.width);
  ctx.restore();
}

function drawFreePath(ctx: DrawingContext, annotation: PathAnnotation): void {
  ctx.save();
  ctx.globalAlpha = annotation.opacity;
  ctx.strokeStyle = annotation.color;
  ctx.fillStyle = annotation.color;
  tracePath(ctx, annotation.points, annotation.width, annotation.tip === "square" ? "square" : "round");
  ctx.restore();
}

function font(annotation: TextAnnotation): string {
  return `${annotation.italic ? "italic " : ""}${annotation.bold ? "700 " : "400 "}${annotation.fontSize}px "${annotation.fontFamily}"`;
}

export function wrapText(ctx: DrawingContext, value: string, maxWidth: number): string[] {
  const lines: string[] = [];
  for (const paragraph of value.replace(/\r/g, "").split("\n")) {
    if (!paragraph) {
      lines.push("");
      continue;
    }
    let line = "";
    for (const character of paragraph) {
      const candidate = line + character;
      if (line && ctx.measureText(candidate).width > maxWidth) {
        lines.push(line);
        line = character;
      } else {
        line = candidate;
      }
    }
    if (line) lines.push(line);
  }
  return lines.length ? lines : [""];
}

function drawText(ctx: DrawingContext, annotation: TextAnnotation): void {
  ctx.save();
  ctx.font = font(annotation);
  ctx.textBaseline = "top";
  ctx.fillStyle = annotation.color;
  const lineHeight = annotation.fontSize * 1.32;
  const lines = wrapText(ctx, annotation.text, Math.max(annotation.fontSize, annotation.rect.width));
  for (let index = 0; index < lines.length; index += 1) {
    const y = annotation.rect.y + index * lineHeight;
    if (y >= annotation.rect.y + annotation.rect.height) break;
    const line = lines[index];
    ctx.fillText(line, annotation.rect.x, y, annotation.rect.width);
    if (annotation.underline && line) {
      const width = Math.min(annotation.rect.width, ctx.measureText(line).width);
      ctx.fillRect(annotation.rect.x, y + annotation.fontSize + 2, width, Math.max(1, annotation.fontSize / 16));
    }
  }
  ctx.restore();
}

function traceMask(ctx: DrawingContext, annotation: EffectAnnotation | EraserAnnotation): void {
  ctx.fillStyle = "#fff";
  ctx.strokeStyle = "#fff";
  if (annotation.mode === "rectangle") {
    ctx.fillRect(annotation.rect.x, annotation.rect.y, annotation.rect.width, annotation.rect.height);
  } else {
    tracePath(ctx, annotation.points, annotation.size);
  }
}

function maskedLayer(
  width: number,
  height: number,
  annotation: EffectAnnotation | EraserAnnotation,
  origin: Point,
  draw: (ctx: DrawingContext) => void,
): DrawingSurface {
  const layer = canvas(width, height);
  const layerCtx = context(layer);
  draw(layerCtx);
  const mask = canvas(width, height);
  const maskContext = context(mask);
  maskContext.save();
  maskContext.translate(-origin.x, -origin.y);
  traceMask(maskContext, annotation);
  maskContext.restore();
  layerCtx.save();
  layerCtx.globalCompositeOperation = "destination-in";
  layerCtx.drawImage(mask, 0, 0);
  layerCtx.restore();
  return layer;
}

function drawEffect(ctx: DrawingContext, annotation: EffectAnnotation, width: number, height: number): void {
  const rawBounds = annotationBounds(annotation);
  const expansion = annotation.effect === "blur" ? annotation.intensity * 2 : 1;
  const x = Math.floor(clamp(rawBounds.x - expansion, 0, width));
  const y = Math.floor(clamp(rawBounds.y - expansion, 0, height));
  const right = Math.ceil(clamp(rawBounds.x + rawBounds.width + expansion, 0, width));
  const bottom = Math.ceil(clamp(rawBounds.y + rawBounds.height + expansion, 0, height));
  const regionWidth = Math.max(1, right - x);
  const regionHeight = Math.max(1, bottom - y);
  const current = canvas(regionWidth, regionHeight);
  context(current).drawImage(ctx.canvas, x, y, regionWidth, regionHeight, 0, 0, regionWidth, regionHeight);
  const processed = canvas(regionWidth, regionHeight);
  const processedCtx = context(processed, true);
  if (annotation.effect === "blur") {
    processedCtx.filter = `blur(${Math.max(1, annotation.intensity)}px)`;
    processedCtx.drawImage(current, 0, 0);
    processedCtx.filter = "none";
  } else {
    const block = Math.max(2, Math.round(annotation.intensity));
    const small = canvas(Math.ceil(regionWidth / block), Math.ceil(regionHeight / block));
    const smallCtx = context(small);
    smallCtx.imageSmoothingEnabled = false;
    smallCtx.drawImage(current, 0, 0, small.width, small.height);
    processedCtx.imageSmoothingEnabled = false;
    processedCtx.drawImage(small, 0, 0, small.width, small.height, 0, 0, regionWidth, regionHeight);
  }
  const layer = maskedLayer(
    regionWidth,
    regionHeight,
    annotation,
    { x, y },
    (layerCtx) => layerCtx.drawImage(processed, 0, 0),
  );
  ctx.drawImage(layer, x, y);
}

function drawEraser(
  ctx: DrawingContext,
  source: CanvasImageSource,
  annotation: EraserAnnotation,
  width: number,
  height: number,
): void {
  const rawBounds = annotationBounds(annotation);
  const x = Math.floor(clamp(rawBounds.x, 0, width));
  const y = Math.floor(clamp(rawBounds.y, 0, height));
  const right = Math.ceil(clamp(rawBounds.x + rawBounds.width, 0, width));
  const bottom = Math.ceil(clamp(rawBounds.y + rawBounds.height, 0, height));
  const regionWidth = Math.max(1, right - x);
  const regionHeight = Math.max(1, bottom - y);
  const restored = maskedLayer(
    regionWidth,
    regionHeight,
    annotation,
    { x, y },
    (layerCtx) => layerCtx.drawImage(source, x, y, regionWidth, regionHeight, 0, 0, regionWidth, regionHeight),
  );
  ctx.drawImage(restored, x, y);
}

export function renderComposite(
  ctx: DrawingContext,
  source: CanvasImageSource,
  width: number,
  height: number,
  annotations: readonly Annotation[],
): void {
  ctx.save();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, width, height);
  ctx.drawImage(source, 0, 0, width, height);
  for (const annotation of annotations) {
    if (annotation.kind === "shape") drawShape(ctx, annotation);
    else if (annotation.kind === "line") drawLine(ctx, annotation);
    else if (annotation.kind === "pencil" || annotation.kind === "marker") drawFreePath(ctx, annotation);
    else if (annotation.kind === "text") drawText(ctx, annotation);
    else if (annotation.kind === "effect") drawEffect(ctx, annotation, width, height);
    else if (annotation.kind === "eraser") drawEraser(ctx, source, annotation, width, height);
  }
  ctx.restore();
}

export function drawAnnotationSelection(ctx: DrawingContext, annotation: Annotation): void {
  const bounds = annotationBounds(annotation);
  ctx.save();
  ctx.strokeStyle = "#ffffff";
  ctx.lineWidth = 3;
  ctx.setLineDash([6, 4]);
  ctx.strokeRect(bounds.x - 4, bounds.y - 4, bounds.width + 8, bounds.height + 8);
  ctx.strokeStyle = "#0a84ff";
  ctx.lineWidth = 1;
  ctx.strokeRect(bounds.x - 4, bounds.y - 4, bounds.width + 8, bounds.height + 8);
  ctx.restore();
}

export function intersectRect(left: Rect, right: Rect): Rect | null {
  const x = Math.max(left.x, right.x);
  const y = Math.max(left.y, right.y);
  const maxX = Math.min(left.x + left.width, right.x + right.width);
  const maxY = Math.min(left.y + left.height, right.y + right.height);
  return maxX > x && maxY > y ? { x, y, width: maxX - x, height: maxY - y } : null;
}

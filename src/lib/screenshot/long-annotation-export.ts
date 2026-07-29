import { annotationBounds, translateAnnotation } from "./geometry";
import { canvasPngBytes, decodePng } from "./image";
import { renderComposite } from "./render";
import type { Annotation, ScreenshotApi, ScreenshotExportAction, ScreenshotExportResult } from "./types";

export const LONG_CAPTURE_ANNOTATION_HALO = 128;
const MAX_CORE_STRIP_HEIGHT = 4096;

export interface LongCaptureAnnotationStripPlan {
  y: number;
  height: number;
  sourceY: number;
  sourceHeight: number;
  cropY: number;
}

export function planLongCaptureAnnotationStrips(
  totalHeight: number,
  stripHeight: number,
  halo = LONG_CAPTURE_ANNOTATION_HALO,
): LongCaptureAnnotationStripPlan[] {
  const safeHeight = Math.max(0, Math.round(totalHeight));
  const coreHeight = Math.round(stripHeight);
  if (!Number.isFinite(coreHeight) || coreHeight < 1 || coreHeight > MAX_CORE_STRIP_HEIGHT) {
    throw new Error("后端返回的长截图标注条带高度无效。");
  }
  const safeHalo = Math.max(0, Math.min(LONG_CAPTURE_ANNOTATION_HALO, Math.round(halo)));
  const plans: LongCaptureAnnotationStripPlan[] = [];
  for (let y = 0; y < safeHeight; y += coreHeight) {
    const height = Math.min(coreHeight, safeHeight - y);
    const sourceY = Math.max(0, y - safeHalo);
    const sourceBottom = Math.min(safeHeight, y + height + safeHalo);
    plans.push({
      y,
      height,
      sourceY,
      sourceHeight: sourceBottom - sourceY,
      cropY: y - sourceY,
    });
  }
  return plans;
}

export function annotationsForLongCaptureSource(
  annotations: readonly Annotation[],
  sourceY: number,
  sourceHeight: number,
): Annotation[] {
  const sourceBottom = sourceY + sourceHeight;
  return annotations.filter((annotation) => {
    const bounds = annotationBounds(annotation);
    const padding = annotation.kind === "effect" && annotation.effect === "blur"
      ? annotation.intensity * 2
      : annotation.kind === "line"
        ? Math.max(annotation.stroke.width / 2, annotation.stroke.width * 3.2)
        : annotation.kind === "shape"
          ? annotation.stroke.width / 2
          : 0;
    return bounds.y - padding < sourceBottom && bounds.y + bounds.height + padding > sourceY;
  }).map((annotation) => translateAnnotation(annotation, { x: 0, y: -sourceY }));
}

function canvas(width: number, height: number): HTMLCanvasElement {
  if (typeof document === "undefined") throw new Error("当前环境不支持长截图标注导出。");
  const value = document.createElement("canvas");
  value.width = Math.max(1, Math.round(width));
  value.height = Math.max(1, Math.round(height));
  return value;
}

function releaseImage(image: ImageBitmap | HTMLImageElement): void {
  if ("close" in image && typeof image.close === "function") image.close();
}

export async function exportAnnotatedLongCapture(
  api: ScreenshotApi,
  jobId: string,
  action: ScreenshotExportAction,
  width: number,
  height: number,
  annotations: readonly Annotation[],
): Promise<ScreenshotExportResult> {
  const prepared = await api.prepareLongCaptureAnnotationExport(jobId, action);
  if (prepared.canceled) return { action, canceled: true };
  const ticket = prepared.ticket;
  if (!ticket) throw new Error("后端未返回长截图标注导出票据。");

  try {
    const plans = planLongCaptureAnnotationStrips(height, prepared.stripHeight);
    for (const plan of plans) {
      const basePng = await api.getLongCaptureTile(jobId, plan.sourceY, plan.sourceHeight);
      const image = await decodePng(basePng);
      const sourceCanvas = canvas(width, plan.sourceHeight);
      const coreCanvas = canvas(width, plan.height);
      try {
        const sourceContext = sourceCanvas.getContext("2d", { willReadFrequently: true });
        const coreContext = coreCanvas.getContext("2d");
        if (!sourceContext || !coreContext) throw new Error("当前 WebView2 不支持长截图条带合成。");
        renderComposite(
          sourceContext,
          image,
          sourceCanvas.width,
          sourceCanvas.height,
          annotationsForLongCaptureSource(annotations, plan.sourceY, plan.sourceHeight),
        );
        coreContext.drawImage(
          sourceCanvas,
          0,
          plan.cropY,
          sourceCanvas.width,
          plan.height,
          0,
          0,
          coreCanvas.width,
          coreCanvas.height,
        );
        await api.uploadLongCaptureAnnotationStrip(ticket, plan.y, await canvasPngBytes(coreCanvas));
      } finally {
        releaseImage(image);
        sourceCanvas.width = 1;
        sourceCanvas.height = 1;
        coreCanvas.width = 1;
        coreCanvas.height = 1;
      }
    }
    return await api.finishLongCaptureAnnotationExport(ticket);
  } catch (error) {
    await api.cancelLongCaptureAnnotationExport(ticket).catch(() => undefined);
    throw error;
  }
}

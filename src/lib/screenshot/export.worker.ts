import { renderComposite } from "./render";
import {
  SCREENSHOT_EXPORT_PROTOCOL_VERSION,
  type ScreenshotExportWorkerRequest,
  type ScreenshotExportWorkerResponse,
} from "./export-protocol";

interface ExportWorkerScope {
  onmessage: ((event: MessageEvent<ScreenshotExportWorkerRequest>) => void) | null;
  postMessage(message: ScreenshotExportWorkerResponse, transfer: Transferable[]): void;
}

const scope = self as unknown as ExportWorkerScope;

scope.onmessage = (event) => {
  const request = event.data;
  if (request.type !== "export" || request.version !== SCREENSHOT_EXPORT_PROTOCOL_VERSION) return;
  void exportPng(request).then((png) => {
    scope.postMessage({
      type: "result",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: request.requestId,
      png,
    }, [png]);
  }).catch((error: unknown) => {
    scope.postMessage({
      type: "error",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: request.requestId,
      message: error instanceof Error ? error.message : "离屏截图合成失败。",
    }, []);
  });
};

async function exportPng(request: ScreenshotExportWorkerRequest): Promise<ArrayBuffer> {
  const source = await createImageBitmap(new Blob([request.framePng], { type: "image/png" }));
  try {
    if (source.width !== request.frameWidth || source.height !== request.frameHeight) {
      throw new Error("截图帧尺寸在导出前发生变化。");
    }
    const full = new OffscreenCanvas(request.frameWidth, request.frameHeight);
    const fullContext = full.getContext("2d", { willReadFrequently: true });
    if (!fullContext) throw new Error("WebView2 无法创建离屏合成画布。");
    renderComposite(fullContext, source, request.frameWidth, request.frameHeight, request.annotations);

    const width = Math.max(1, Math.round(request.selection.width));
    const height = Math.max(1, Math.round(request.selection.height));
    const output = new OffscreenCanvas(width, height);
    const outputContext = output.getContext("2d");
    if (!outputContext) throw new Error("WebView2 无法创建离屏导出画布。");
    outputContext.drawImage(
      full,
      Math.round(request.selection.x),
      Math.round(request.selection.y),
      width,
      height,
      0,
      0,
      width,
      height,
    );
    return (await output.convertToBlob({ type: "image/png" })).arrayBuffer();
  } finally {
    source.close();
  }
}

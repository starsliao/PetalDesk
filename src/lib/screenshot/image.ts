import type { ScreenshotSession } from "./types";

export function pngBlob(bytes: Uint8Array): Blob {
  return new Blob([bytes as BlobPart], { type: "image/png" });
}

function decodeImageElement(blob: Blob): Promise<HTMLImageElement> {
  return new Promise<HTMLImageElement>((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(url);
      resolve(image);
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("无法解码截图画面，请重新截图。"));
    };
    image.src = url;
  });
}

export async function decodePng(bytes: Uint8Array): Promise<ImageBitmap | HTMLImageElement> {
  const blob = pngBlob(bytes);
  if (typeof createImageBitmap !== "function") return decodeImageElement(blob);

  // Some long-lived WebView2 processes occasionally leave createImageBitmap
  // pending forever. Fall back to the DOM decoder and close a late bitmap.
  return new Promise<ImageBitmap | HTMLImageElement>((resolve, reject) => {
    let settled = false;
    let fallbackStarted = false;
    const finishFallback = () => {
      if (settled || fallbackStarted) return;
      fallbackStarted = true;
      void decodeImageElement(blob).then(
        (image) => {
          if (settled) return;
          settled = true;
          clearTimeout(fallbackTimer);
          resolve(image);
        },
        (error) => {
          if (settled) return;
          settled = true;
          clearTimeout(fallbackTimer);
          reject(error);
        },
      );
    };
    const fallbackTimer = setTimeout(finishFallback, 2_500);

    void createImageBitmap(blob).then(
      (bitmap) => {
        if (settled || fallbackStarted) {
          bitmap.close();
          return;
        }
        settled = true;
        clearTimeout(fallbackTimer);
        resolve(bitmap);
      },
      finishFallback,
    );
  });
}

export function imageDimensions(image: ImageBitmap | HTMLImageElement): { width: number; height: number } {
  return image instanceof HTMLImageElement
    ? { width: image.naturalWidth, height: image.naturalHeight }
    : { width: image.width, height: image.height };
}

export function validateFrameDimensions(
  session: ScreenshotSession,
  image: ImageBitmap | HTMLImageElement,
): void {
  const dimensions = imageDimensions(image);
  if (dimensions.width !== session.frameWidth || dimensions.height !== session.frameHeight) {
    throw new Error("截图尺寸与当前显示器不一致，请重新截图。");
  }
}

export async function canvasPngBytes(canvas: HTMLCanvasElement | OffscreenCanvas): Promise<Uint8Array> {
  if (canvas instanceof HTMLCanvasElement) {
    const blob = await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((value) => value ? resolve(value) : reject(new Error("生成 PNG 失败。")), "image/png");
    });
    return new Uint8Array(await blob.arrayBuffer());
  }
  const blob = await canvas.convertToBlob({ type: "image/png" });
  return new Uint8Array(await blob.arrayBuffer());
}

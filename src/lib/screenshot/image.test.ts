import { afterEach, describe, expect, it, vi } from "vitest";
import { decodePng } from "./image";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("decodePng", () => {
  it("falls back to an image element when createImageBitmap never settles", async () => {
    vi.useFakeTimers();
    const createObjectURL = vi.fn(() => "blob:screenshot-fallback");
    const revokeObjectURL = vi.fn();

    class FallbackImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      private value = "";

      set src(value: string) {
        this.value = value;
        this.onload?.();
      }

      get src(): string {
        return this.value;
      }
    }

    vi.stubGlobal("URL", { createObjectURL, revokeObjectURL });
    vi.stubGlobal("Image", FallbackImage);
    vi.stubGlobal("createImageBitmap", vi.fn(() => new Promise<ImageBitmap>(() => {})));

    const decoded = decodePng(Uint8Array.from([137, 80, 78, 71]));
    await vi.advanceTimersByTimeAsync(2_500);

    await expect(decoded).resolves.toBeInstanceOf(FallbackImage);
    expect(createObjectURL).toHaveBeenCalledOnce();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:screenshot-fallback");
  });
});

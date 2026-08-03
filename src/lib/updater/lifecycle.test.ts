import { describe, expect, it, vi } from "vitest";
import {
  addUpdateInstallPreparation,
  prepareCurrentWindowForUpdate,
} from "./lifecycle";

describe("update installation lifecycle", () => {
  it("waits for every registered persistence task", async () => {
    const order: string[] = [];
    const first = addUpdateInstallPreparation(async () => {
      await Promise.resolve();
      order.push("first");
    });
    const secondHandler = vi.fn(async () => {
      order.push("second");
    });
    const second = addUpdateInstallPreparation(secondHandler);

    await prepareCurrentWindowForUpdate();

    expect(secondHandler).toHaveBeenCalledOnce();
    expect(order.sort()).toEqual(["first", "second"]);
    first();
    second();
  });

  it("propagates a failed save so the backend can abort installation", async () => {
    const cleanup = addUpdateInstallPreparation(async () => {
      throw new Error("任务保存失败");
    });

    await expect(prepareCurrentWindowForUpdate()).rejects.toThrow("任务保存失败");
    cleanup();
  });
});

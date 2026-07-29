import { describe, expect, it } from "vitest";
import { longCaptureStatusFromEvent, visibleLongCaptureTiles } from "./long-capture";
import type { LongCaptureStatus } from "./types";

const status: LongCaptureStatus = {
  jobId: "job-1",
  sessionId: "session-1",
  state: "capturing",
  engine: "wheel",
  frameCount: 3,
  width: 500,
  height: 2400,
  canUndo: true,
};

describe("long screenshot helpers", () => {
  it("returns only visible and overscan tiles, including a short final tile", () => {
    expect(visibleLongCaptureTiles({
      totalHeight: 2500,
      scrollTop: 900,
      viewportHeight: 700,
      scale: 0.5,
      tileHeight: 1000,
      overscan: 1,
    })).toEqual([
      { y: 0, height: 1000 },
      { y: 1000, height: 1000 },
      { y: 2000, height: 500 },
    ]);
  });

  it("merges partial event payloads without discarding progress", () => {
    expect(longCaptureStatusFromEvent({
      status: { jobId: "job-1", sessionId: "session-1", frameCount: 4, height: 3200, message: "等待页面稳定" },
    }, status, "paused")).toEqual({
      ...status,
      state: "paused",
      frameCount: 4,
      height: 3200,
      message: "等待页面稳定",
    });
  });
});

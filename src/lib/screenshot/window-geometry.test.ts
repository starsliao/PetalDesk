import { describe, expect, it } from "vitest";
import { matchesScreenshotWindow } from "./window-geometry";
import type { ScreenshotSession } from "./types";

const session: ScreenshotSession = {
  id: "session-1",
  monitor: { x: -2560, y: 0, width: 2560, height: 1440, scaleFactor: 1.5 },
  frameWidth: 2560,
  frameHeight: 1440,
  capturedAt: "2026-07-27T00:00:00Z",
};

describe("screenshot window geometry validation", () => {
  it("accepts the captured monitor's physical position, size, and scale", () => {
    expect(matchesScreenshotWindow(session, {
      position: { x: -2560, y: 0 },
      size: { width: 2560, height: 1440 },
      scaleFactor: 1.5,
    })).toBe(true);
  });

  it("rejects display movement, resolution changes, and DPI changes", () => {
    expect(matchesScreenshotWindow(session, { position: { x: 0, y: 0 } })).toBe(false);
    expect(matchesScreenshotWindow(session, { size: { width: 1920, height: 1080 } })).toBe(false);
    expect(matchesScreenshotWindow(session, { scaleFactor: 1.25 })).toBe(false);
  });

  it("allows harmless floating-point scale noise", () => {
    expect(matchesScreenshotWindow(session, { scaleFactor: 1.5004 })).toBe(true);
  });
});

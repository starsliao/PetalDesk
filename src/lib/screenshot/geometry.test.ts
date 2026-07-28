import { describe, expect, it } from "vitest";
import {
  annotationBounds,
  clampRect,
  hitTestAnnotation,
  moveRect,
  normalizeRect,
  placeToolbar,
  resizeRect,
  scaleAnnotation,
  translateAnnotation,
} from "./geometry";
import type { ShapeAnnotation } from "./types";

const bounds = { x: 0, y: 0, width: 800, height: 600 };

function shape(): ShapeAnnotation {
  return {
    id: "shape-1",
    kind: "shape",
    shape: "rectangle",
    rect: { x: 100, y: 120, width: 200, height: 100 },
    stroke: { color: "#f00", width: 4, lineStyle: "solid" },
    fill: null,
  };
}

describe("screenshot geometry", () => {
  it("normalizes a selection dragged in any direction", () => {
    expect(normalizeRect({ x: 520, y: 410 }, { x: 120, y: 80 })).toEqual({
      x: 120,
      y: 80,
      width: 400,
      height: 330,
    });
  });

  it("clamps moves and all resize directions to the active monitor", () => {
    expect(moveRect({ x: 700, y: 500, width: 100, height: 100 }, { x: 50, y: 60 }, bounds)).toEqual({
      x: 700,
      y: 500,
      width: 100,
      height: 100,
    });
    expect(resizeRect({ x: 100, y: 100, width: 200, height: 200 }, "nw", { x: -50, y: -20 }, bounds)).toEqual({
      x: 0,
      y: 0,
      width: 300,
      height: 300,
    });
    expect(clampRect({ x: -20, y: -20, width: 900, height: 700 }, bounds)).toEqual(bounds);
  });

  it("places the toolbar above where possible and flips below near the top edge", () => {
    expect(placeToolbar({ x: 100, y: 300, width: 300, height: 160 }, bounds, 280, 90)).toMatchObject({ side: "above", top: 200 });
    expect(placeToolbar({ x: 100, y: 20, width: 300, height: 160 }, bounds, 280, 90)).toMatchObject({ side: "below", top: 190 });
  });

  it("hit-tests, moves, and resizes vector annotations", () => {
    const original = shape();
    expect(hitTestAnnotation(original, { x: 100, y: 170 })).toBe(true);
    expect(hitTestAnnotation(original, { x: 200, y: 170 })).toBe(false);
    const moved = translateAnnotation(original, { x: 20, y: -10 });
    expect(annotationBounds(moved)).toEqual({ x: 120, y: 110, width: 200, height: 100 });
    const scaled = scaleAnnotation(original, original.rect, { x: 100, y: 120, width: 400, height: 50 });
    expect(annotationBounds(scaled)).toEqual({ x: 100, y: 120, width: 400, height: 50 });
  });
});

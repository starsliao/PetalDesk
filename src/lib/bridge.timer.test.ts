import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { notesApi } from "./bridge";
import {
  PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY,
  PREVIOUS_TIMER_STORAGE_KEY,
  TIMER_DIGIT_OPACITY_STORAGE_KEY,
  TIMER_STORAGE_KEY,
  type TimerData,
} from "./timer";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

let backend: TimerData;

function emptyBackend(): TimerData {
  return {
    version: 1,
    accumulatedMs: 0,
    runningSince: null,
    logs: [],
    digitOpacity: 1,
  };
}

beforeEach(() => {
  localStorage.clear();
  backend = emptyBackend();
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  invoke.mockReset();
  invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_timer_data") return structuredClone(backend);
    if (command === "save_timer_data") {
      backend = structuredClone(args?.data as TimerData);
      return structuredClone(backend);
    }
    throw new Error(`unexpected command: ${command}`);
  });
});

afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  localStorage.clear();
});

describe("desktop timer data migration", () => {
  it("moves legacy WebView timer state and opacity into the data storage backend", async () => {
    localStorage.setItem(
      PREVIOUS_TIMER_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        accumulatedMs: 65_000,
        runningSince: null,
        logs: [{ id: "1-1", timestamp: 1, action: "pause", elapsedMs: 65_000 }],
      }),
    );
    localStorage.setItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY, "0.55");

    await expect(notesApi.migrateLegacyTimerData()).resolves.toBe(true);

    expect(backend).toMatchObject({
      accumulatedMs: 65_000,
      runningSince: null,
      digitOpacity: 0.55,
      logs: [{ action: "pause", elapsedMs: 65_000 }],
    });
    expect(localStorage.getItem(TIMER_STORAGE_KEY)).toBeNull();
    expect(localStorage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBeNull();
    expect(localStorage.getItem(PREVIOUS_TIMER_STORAGE_KEY)).toBeNull();
    expect(localStorage.getItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBeNull();
  });

  it("keeps an existing backend authoritative while removing stale legacy keys", async () => {
    backend = {
      version: 1,
      accumulatedMs: 8_000,
      runningSince: null,
      logs: [{ id: "backend", timestamp: 10, action: "pause", elapsedMs: 8_000 }],
      digitOpacity: 0.8,
    };
    localStorage.setItem(
      PREVIOUS_TIMER_STORAGE_KEY,
      JSON.stringify({ version: 1, accumulatedMs: 99_000, runningSince: null, logs: [] }),
    );

    await notesApi.migrateLegacyTimerData();

    expect(backend.accumulatedMs).toBe(8_000);
    expect(invoke.mock.calls.filter(([command]) => command === "save_timer_data")).toHaveLength(0);
    expect(localStorage.getItem(PREVIOUS_TIMER_STORAGE_KEY)).toBeNull();
  });

  it("does not clear legacy keys when the backend save fails", async () => {
    localStorage.setItem(
      PREVIOUS_TIMER_STORAGE_KEY,
      JSON.stringify({ version: 1, accumulatedMs: 1_000, runningSince: null, logs: [] }),
    );
    invoke.mockImplementation(async (command: string) => {
      if (command === "get_timer_data") return emptyBackend();
      throw new Error("disk unavailable");
    });

    await expect(notesApi.migrateLegacyTimerData()).rejects.toBeDefined();
    expect(localStorage.getItem(PREVIOUS_TIMER_STORAGE_KEY)).not.toBeNull();
  });
});

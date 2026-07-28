import { describe, expect, it } from "vitest";
import {
  DEFAULT_TIMER_DIGIT_OPACITY,
  PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY,
  PREVIOUS_TIMER_STORAGE_KEY,
  TIMER_DIGIT_OPACITY_STORAGE_KEY,
  TIMER_STORAGE_KEY,
  TIMER_LOG_LIMIT,
  TimerStore,
  formatTimerExact,
  formatTimerMain,
  formatTimerTimestamp,
  loadTimerDigitOpacity,
  normalizeTimerDigitOpacity,
  saveTimerDigitOpacity,
  timerActionLabel,
  type TimerStorage,
} from "./timer";

class MemoryStorage implements TimerStorage {
  values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

describe("TimerStore", () => {
  it("starts automatically and calculates elapsed time from its saved baseline", () => {
    const storage = new MemoryStorage();
    let now = 1_000;
    const timer = new TimerStore({ storage, now: () => now });

    expect(timer.snapshot()).toMatchObject({ elapsedMs: 0, isRunning: true, logs: [] });
    now += 3_723_456;

    expect(timer.snapshot().elapsedMs).toBe(3_723_456);
    expect(formatTimerMain(timer.snapshot().elapsedMs)).toBe("01:02");
    expect(formatTimerExact(timer.snapshot().elapsedMs)).toBe("01:02:03");
  });

  it("restores paused and running timers without counting paused time", () => {
    const storage = new MemoryStorage();
    let now = 10_000;
    let timer = new TimerStore({ storage, now: () => now });

    now += 65_000;
    timer.pause();
    now += 120_000;
    timer = new TimerStore({ storage, now: () => now });

    expect(timer.snapshot()).toMatchObject({ elapsedMs: 65_000, isRunning: false });

    timer.resume();
    now += 5_000;
    timer = new TimerStore({ storage, now: () => now });

    expect(timer.snapshot()).toMatchObject({ elapsedMs: 70_000, isRunning: true });
    expect(timer.snapshot().logs.map((entry) => entry.action)).toEqual(["pause", "resume"]);
  });

  it("moves persisted timer data from the previous product keys", () => {
    const storage = new MemoryStorage();
    storage.setItem(PREVIOUS_TIMER_STORAGE_KEY, JSON.stringify({
      version: 1,
      accumulatedMs: 12_000,
      runningSince: null,
      logs: [],
    }));
    storage.setItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY, "0.6");

    const timer = new TimerStore({ storage, now: () => 20_000 });
    expect(timer.snapshot()).toMatchObject({ elapsedMs: 12_000, isRunning: false });
    expect(loadTimerDigitOpacity(storage)).toBe(0.6);
    expect(storage.getItem(TIMER_STORAGE_KEY)).not.toBeNull();
    expect(storage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBe("0.6");
    expect(storage.getItem(PREVIOUS_TIMER_STORAGE_KEY)).toBeNull();
    expect(storage.getItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBeNull();
  });

  it("logs the time before reset and preserves whether the timer was running", () => {
    let now = 2_000;
    const timer = new TimerStore({ storage: new MemoryStorage(), now: () => now });
    now += 90_500;

    const resetRunning = timer.reset();
    expect(resetRunning).toMatchObject({ elapsedMs: 0, isRunning: true });
    expect(resetRunning.logs[0]).toMatchObject({
      action: "reset",
      timestamp: now,
      elapsedMs: 90_500,
    });

    now += 10_000;
    timer.pause();
    now += 20_000;
    const resetPaused = timer.reset();
    expect(resetPaused).toMatchObject({ elapsedMs: 0, isRunning: false });
    expect(resetPaused.logs.at(-1)).toMatchObject({ action: "reset", elapsedMs: 10_000 });
  });

  it("starts every visible session at zero and records the previous duration", () => {
    const storage = new MemoryStorage();
    let now = 5_000;
    let timer = new TimerStore({ storage, now: () => now });
    now += 90_000;
    timer.pause();
    now += 30_000;

    timer = new TimerStore({ storage, now: () => now });
    const snapshot = timer.startSession();

    expect(snapshot).toMatchObject({ elapsedMs: 0, isRunning: true });
    expect(snapshot.logs.at(-1)).toMatchObject({
      action: "reset",
      timestamp: now,
      elapsedMs: 90_000,
    });
  });

  it("always records a pause on close and can persistently clear all records", () => {
    const storage = new MemoryStorage();
    let now = 10_000;
    let timer = new TimerStore({ storage, now: () => now });
    timer.startSession();
    now += 8_000;
    timer.pause();
    now += 5_000;

    const closed = timer.pauseForClose();
    expect(closed).toMatchObject({ elapsedMs: 8_000, isRunning: false });
    expect(closed.logs.at(-1)).toMatchObject({
      action: "pause",
      timestamp: now,
      elapsedMs: 8_000,
    });

    expect(timer.clearLogs().logs).toEqual([]);
    timer = new TimerStore({ storage, now: () => now });
    expect(timer.snapshot().logs).toEqual([]);
  });

  it("keeps only the latest reasonable number of action records", () => {
    let now = 0;
    const timer = new TimerStore({ storage: new MemoryStorage(), now: () => now });

    for (let index = 0; index < TIMER_LOG_LIMIT + 25; index += 1) {
      now += 1_000;
      timer.toggle();
    }

    const { logs } = timer.snapshot();
    expect(logs).toHaveLength(TIMER_LOG_LIMIT);
    expect(logs[0].timestamp).toBe(26_000);
    expect(logs.at(-1)?.timestamp).toBe((TIMER_LOG_LIMIT + 25) * 1_000);
  });

  it("recovers safely from malformed persisted data", () => {
    const storage = new MemoryStorage();
    storage.setItem("broken", "{not-json");
    const timer = new TimerStore({ storage, storageKey: "broken", now: () => 42_000 });

    expect(timer.snapshot(45_000)).toMatchObject({ elapsedMs: 3_000, isRunning: true, logs: [] });
    expect(JSON.parse(storage.getItem("broken") ?? "")).toMatchObject({
      version: 1,
      accumulatedMs: 0,
      runningSince: 42_000,
    });
  });
});

describe("timer formatting", () => {
  it("does not wrap total hours after one day", () => {
    expect(formatTimerMain(125 * 3_600_000 + 7 * 60_000)).toBe("125:07");
    expect(formatTimerExact(125 * 3_600_000 + 7 * 60_000 + 8_999)).toBe("125:07:08");
  });

  it("uses stable Chinese action labels and a local date format", () => {
    const timestamp = new Date(2026, 6, 12, 9, 8, 7).getTime();
    expect(formatTimerTimestamp(timestamp)).toBe("2026-07-12 09:08:07");
    expect([timerActionLabel("reset"), timerActionLabel("pause"), timerActionLabel("resume")]).toEqual(
      ["重置", "暂停", "继续"],
    );
  });
});

describe("timer digit opacity", () => {
  it("defaults to fully opaque and persists a normalized preference", () => {
    const storage = new MemoryStorage();

    expect(loadTimerDigitOpacity(storage)).toBe(DEFAULT_TIMER_DIGIT_OPACITY);
    expect(saveTimerDigitOpacity(storage, 0.537)).toBe(0.54);
    expect(storage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBe("0.54");
    expect(loadTimerDigitOpacity(storage)).toBe(0.54);
  });

  it("clamps invalid and out-of-range opacity values", () => {
    expect(normalizeTimerDigitOpacity(Number.NaN)).toBe(DEFAULT_TIMER_DIGIT_OPACITY);
    expect(normalizeTimerDigitOpacity(0)).toBe(0.1);
    expect(normalizeTimerDigitOpacity(2)).toBe(1);
  });
});

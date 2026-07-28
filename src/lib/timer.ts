import { previousStorageKey, readMigratedStorageValue } from "./storage";

export const TIMER_STORAGE_KEY = "petaldesk.timer.v1";
export const TIMER_DIGIT_OPACITY_STORAGE_KEY = "petaldesk.timer.digit-opacity.v1";
export const PREVIOUS_TIMER_STORAGE_KEY = previousStorageKey("timer.v1");
export const PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY = previousStorageKey("timer.digit-opacity.v1");
export const DEFAULT_TIMER_DIGIT_OPACITY = 1;
export const MIN_TIMER_DIGIT_OPACITY = 0.1;
export const MAX_TIMER_DIGIT_OPACITY = 1;
export const TIMER_LOG_LIMIT = 500;

export type TimerAction = "reset" | "pause" | "resume";
export type TimerActionFilter = "all" | TimerAction;

export interface TimerLogEntry {
  id: string;
  timestamp: number;
  action: TimerAction;
  elapsedMs: number;
}

export interface TimerSnapshot {
  elapsedMs: number;
  isRunning: boolean;
  logs: TimerLogEntry[];
}

export interface TimerPersistedState {
  version: 1;
  accumulatedMs: number;
  runningSince: number | null;
  logs: TimerLogEntry[];
}

export interface TimerData extends TimerPersistedState {
  digitOpacity: number;
}

export interface TimerStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

export function normalizeTimerDigitOpacity(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DEFAULT_TIMER_DIGIT_OPACITY;
  }

  const clamped = Math.min(MAX_TIMER_DIGIT_OPACITY, Math.max(MIN_TIMER_DIGIT_OPACITY, value));
  return Math.round(clamped * 100) / 100;
}

export function loadTimerDigitOpacity(storage: TimerStorage | null): number {
  try {
    const raw = storage
      ? readMigratedStorageValue(
          storage,
          TIMER_DIGIT_OPACITY_STORAGE_KEY,
          PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY,
        )
      : null;
    if (raw === null || raw === undefined || raw.trim() === "") {
      return DEFAULT_TIMER_DIGIT_OPACITY;
    }
    return normalizeTimerDigitOpacity(Number(raw));
  } catch {
    return DEFAULT_TIMER_DIGIT_OPACITY;
  }
}

export function saveTimerDigitOpacity(storage: TimerStorage | null, value: number): number {
  const normalized = normalizeTimerDigitOpacity(value);
  try {
    storage?.setItem(TIMER_DIGIT_OPACITY_STORAGE_KEY, String(normalized));
  } catch {
    // Keep the in-memory preference usable if localStorage is unavailable.
  }
  return normalized;
}

export interface TimerStoreOptions {
  storage?: TimerStorage | null;
  storageKey?: string;
  now?: () => number;
}

const TIMER_ACTIONS = new Set<TimerAction>(["reset", "pause", "resume"]);

function nonNegativeFinite(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function normalizeLog(value: unknown, index: number): TimerLogEntry | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as Partial<TimerLogEntry>;
  const timestamp = nonNegativeFinite(candidate.timestamp);
  const elapsedMs = nonNegativeFinite(candidate.elapsedMs);
  if (
    timestamp === null ||
    elapsedMs === null ||
    typeof candidate.action !== "string" ||
    !TIMER_ACTIONS.has(candidate.action as TimerAction)
  ) {
    return null;
  }

  return {
    id:
      typeof candidate.id === "string" && candidate.id.length > 0
        ? candidate.id
        : `${timestamp}-${index}`,
    timestamp,
    action: candidate.action as TimerAction,
    elapsedMs,
  };
}

export function parseTimerPersistedState(raw: string | null): TimerPersistedState | null {
  if (!raw) return null;

  try {
    const value = JSON.parse(raw) as Partial<TimerPersistedState>;
    const accumulatedMs = nonNegativeFinite(value.accumulatedMs);
    const runningSince = value.runningSince === null ? null : nonNegativeFinite(value.runningSince);
    if (value.version !== 1 || accumulatedMs === null || runningSince === null && value.runningSince !== null) {
      return null;
    }

    const logs = Array.isArray(value.logs)
      ? value.logs
          .map(normalizeLog)
          .filter((entry): entry is TimerLogEntry => entry !== null)
          .slice(-TIMER_LOG_LIMIT)
      : [];

    return {
      version: 1,
      accumulatedMs,
      runningSince,
      logs,
    };
  } catch {
    return null;
  }
}

function pad(value: number, width = 2): string {
  return Math.floor(value).toString().padStart(width, "0");
}

/** Formats a duration as total hours and minutes, without wrapping after 24 hours. */
export function formatTimerMain(elapsedMs: number): string {
  const totalMinutes = Math.floor(Math.max(0, elapsedMs) / 60_000);
  return `${pad(Math.floor(totalMinutes / 60))}:${pad(totalMinutes % 60)}`;
}

/** Formats a duration as total hours, minutes and seconds, without wrapping after 24 hours. */
export function formatTimerExact(elapsedMs: number): string {
  const totalSeconds = Math.floor(Math.max(0, elapsedMs) / 1_000);
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  return `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

/** Formats a local timestamp without relying on locale-specific punctuation. */
export function formatTimerTimestamp(timestamp: number): string {
  const date = new Date(timestamp);
  return [
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`,
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`,
  ].join(" ");
}

export function timerActionLabel(action: TimerAction): string {
  if (action === "reset") return "重置";
  if (action === "pause") return "暂停";
  return "继续";
}

export class TimerStore {
  private state: TimerPersistedState;
  private readonly storage: TimerStorage | null;
  private readonly storageKey: string;
  private readonly now: () => number;
  private logSequence = 0;

  constructor(options: TimerStoreOptions = {}) {
    this.storage = options.storage ?? null;
    this.storageKey = options.storageKey ?? TIMER_STORAGE_KEY;
    this.now = options.now ?? Date.now;

    let restored: TimerPersistedState | null = null;
    try {
      const raw = this.storage && this.storageKey === TIMER_STORAGE_KEY
        ? readMigratedStorageValue(this.storage, TIMER_STORAGE_KEY, PREVIOUS_TIMER_STORAGE_KEY)
        : this.storage?.getItem(this.storageKey) ?? null;
      restored = parseTimerPersistedState(raw);
    } catch {
      // Storage can be unavailable in hardened browser or WebView configurations.
    }

    if (restored) {
      this.state = restored;
    } else {
      this.state = {
        version: 1,
        accumulatedMs: 0,
        runningSince: this.now(),
        logs: [],
      };
      this.persist();
    }
  }

  snapshot(at = this.now()): TimerSnapshot {
    return {
      elapsedMs: this.elapsedAt(at),
      isRunning: this.state.runningSince !== null,
      logs: [...this.state.logs],
    };
  }

  persistedState(): TimerPersistedState {
    return {
      ...this.state,
      logs: [...this.state.logs],
    };
  }

  reset(at = this.now()): TimerSnapshot {
    const elapsedMs = this.elapsedAt(at);
    const wasRunning = this.state.runningSince !== null;
    this.appendLog("reset", elapsedMs, at);
    this.state.accumulatedMs = 0;
    this.state.runningSince = wasRunning ? at : null;
    this.persist();
    return this.snapshot(at);
  }

  /** Starts a fresh visible timer session while retaining an audit record of the previous time. */
  startSession(at = this.now()): TimerSnapshot {
    const elapsedMs = this.elapsedAt(at);
    this.appendLog("reset", elapsedMs, at);
    this.state.accumulatedMs = 0;
    this.state.runningSince = at;
    this.persist();
    return this.snapshot(at);
  }

  pause(at = this.now()): TimerSnapshot {
    if (this.state.runningSince === null) return this.snapshot(at);
    const elapsedMs = this.elapsedAt(at);
    this.state.accumulatedMs = elapsedMs;
    this.state.runningSince = null;
    this.appendLog("pause", elapsedMs, at);
    this.persist();
    return this.snapshot(at);
  }

  /** Records window closure even when the timer was already paused. */
  pauseForClose(at = this.now()): TimerSnapshot {
    const elapsedMs = this.elapsedAt(at);
    this.state.accumulatedMs = elapsedMs;
    this.state.runningSince = null;
    this.appendLog("pause", elapsedMs, at);
    this.persist();
    return this.snapshot(at);
  }

  resume(at = this.now()): TimerSnapshot {
    if (this.state.runningSince !== null) return this.snapshot(at);
    const elapsedMs = this.state.accumulatedMs;
    this.state.runningSince = at;
    this.appendLog("resume", elapsedMs, at);
    this.persist();
    return this.snapshot(at);
  }

  toggle(at = this.now()): TimerSnapshot {
    return this.state.runningSince === null ? this.resume(at) : this.pause(at);
  }

  clearLogs(at = this.now()): TimerSnapshot {
    this.state.logs = [];
    this.persist();
    return this.snapshot(at);
  }

  private elapsedAt(at: number): number {
    if (this.state.runningSince === null) return this.state.accumulatedMs;
    return this.state.accumulatedMs + Math.max(0, at - this.state.runningSince);
  }

  private appendLog(action: TimerAction, elapsedMs: number, timestamp: number): void {
    this.logSequence += 1;
    this.state.logs.push({
      id: `${timestamp}-${this.logSequence}`,
      timestamp,
      action,
      elapsedMs,
    });
    if (this.state.logs.length > TIMER_LOG_LIMIT) {
      this.state.logs.splice(0, this.state.logs.length - TIMER_LOG_LIMIT);
    }
  }

  private persist(): void {
    try {
      this.storage?.setItem(this.storageKey, JSON.stringify(this.state));
    } catch {
      // The timer remains usable in memory if localStorage is full or disabled.
    }
  }
}

export function createTimerStore(options: TimerStoreOptions = {}): TimerStore {
  return new TimerStore(options);
}

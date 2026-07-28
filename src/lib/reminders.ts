import { invoke } from "@tauri-apps/api/core";
import { previousStorageKey, readMigratedStorageValue } from "./storage";

export type ReminderKind = "once" | "interval" | "daily" | "weekly" | "monthly" | "yearly";

export interface ReminderSchedule {
  kind: ReminderKind;
  anchorAt: string;
  intervalSeconds?: number | null;
}

export interface Reminder {
  id: string;
  title: string;
  message: string;
  schedule: ReminderSchedule;
  enabled: boolean;
  nextDueAt: string | null;
  lastTriggeredAt: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface UpsertReminderRequest {
  id?: string | null;
  title: string;
  message: string;
  schedule: ReminderSchedule;
  enabled: boolean;
}

export interface ReminderApi {
  isDesktop(): boolean;
  list(): Promise<Reminder[]>;
  upsert(request: UpsertReminderRequest): Promise<Reminder>;
  delete(id: string): Promise<void>;
  setEnabled(id: string, enabled: boolean): Promise<Reminder>;
}

interface AppError {
  code?: string;
  message: string;
}

const browserStorageKey = "petaldesk.browser-reminders.v1";
const previousBrowserStorageKey = previousStorageKey("browser-reminders.v1");

function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) {
    return String((error as AppError).message);
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as AppError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "提醒操作失败，请稍后重试。";
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

function readBrowserReminders(): Reminder[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const parsed = JSON.parse(
      readMigratedStorageValue(localStorage, browserStorageKey, previousBrowserStorageKey) ?? "[]",
    ) as unknown;
    return Array.isArray(parsed) ? parsed as Reminder[] : [];
  } catch {
    return [];
  }
}

function writeBrowserReminders(reminders: Reminder[]): void {
  localStorage.setItem(browserStorageKey, JSON.stringify(reminders));
}

function parseLocalDateTime(value: string): Date {
  const normalized = value.length === 16 ? `${value}:00` : value;
  const parsed = new Date(normalized);
  if (Number.isNaN(parsed.getTime())) throw new Error("提醒时间无效。");
  return parsed;
}

function daysInMonth(year: number, month: number): number {
  return new Date(year, month + 1, 0).getDate();
}

function nextBrowserDue(schedule: ReminderSchedule, after = new Date()): string | null {
  const anchor = parseLocalDateTime(schedule.anchorAt);
  if (schedule.kind === "once") return anchor > after ? toLocalDateTime(anchor, true) : null;

  if (schedule.kind === "interval") {
    const intervalMs = Math.max(60, schedule.intervalSeconds ?? 60) * 1_000;
    if (anchor > after) return toLocalDateTime(anchor, true);
    const steps = Math.floor((after.getTime() - anchor.getTime()) / intervalMs) + 1;
    return toLocalDateTime(new Date(anchor.getTime() + steps * intervalMs), true);
  }

  const time = {
    hours: anchor.getHours(),
    minutes: anchor.getMinutes(),
    seconds: anchor.getSeconds(),
  };
  let candidate = new Date(anchor);

  if (schedule.kind === "daily") {
    candidate = new Date(after.getFullYear(), after.getMonth(), after.getDate(), time.hours, time.minutes, time.seconds);
    if (candidate <= after) candidate.setDate(candidate.getDate() + 1);
    if (candidate < anchor) candidate = new Date(anchor);
  } else if (schedule.kind === "weekly") {
    candidate = new Date(after.getFullYear(), after.getMonth(), after.getDate(), time.hours, time.minutes, time.seconds);
    const dayOffset = (anchor.getDay() - candidate.getDay() + 7) % 7;
    candidate.setDate(candidate.getDate() + dayOffset);
    if (candidate <= after) candidate.setDate(candidate.getDate() + 7);
    if (candidate < anchor) candidate = new Date(anchor);
  } else if (schedule.kind === "monthly") {
    let year = after.getFullYear();
    let month = after.getMonth();
    for (let attempt = 0; attempt < 240; attempt += 1) {
      const day = Math.min(anchor.getDate(), daysInMonth(year, month));
      candidate = new Date(year, month, day, time.hours, time.minutes, time.seconds);
      if (candidate > after && candidate >= anchor) break;
      month += 1;
      if (month > 11) {
        month = 0;
        year += 1;
      }
    }
  } else {
    let year = Math.max(after.getFullYear(), anchor.getFullYear());
    for (let attempt = 0; attempt < 200; attempt += 1) {
      const day = Math.min(anchor.getDate(), daysInMonth(year, anchor.getMonth()));
      candidate = new Date(year, anchor.getMonth(), day, time.hours, time.minutes, time.seconds);
      if (candidate > after && candidate >= anchor) break;
      year += 1;
    }
  }

  return toLocalDateTime(candidate, true);
}

export function toLocalDateTime(date: Date, includeSeconds = false): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  const base = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return includeSeconds ? `${base}:${pad(date.getSeconds())}` : base;
}

export const remindersApi: ReminderApi = {
  isDesktop: isDesktopRuntime,

  async list(): Promise<Reminder[]> {
    if (isDesktopRuntime()) return command<Reminder[]>("list_reminders");
    return readBrowserReminders().sort((left, right) => {
      if (left.enabled !== right.enabled) return Number(right.enabled) - Number(left.enabled);
      return (left.nextDueAt ?? "~").localeCompare(right.nextDueAt ?? "~");
    });
  },

  async upsert(request: UpsertReminderRequest): Promise<Reminder> {
    if (isDesktopRuntime()) return command<Reminder>("upsert_reminder", { request });
    const reminders = readBrowserReminders();
    const now = toLocalDateTime(new Date(), true);
    const id = request.id ?? crypto.randomUUID();
    const existing = reminders.find((reminder) => reminder.id === id);
    const reminder: Reminder = {
      id,
      title: request.title.trim(),
      message: request.message.trim(),
      schedule: request.schedule,
      enabled: request.enabled,
      nextDueAt: request.enabled ? nextBrowserDue(request.schedule) : null,
      lastTriggeredAt: existing?.lastTriggeredAt ?? null,
      createdAt: existing?.createdAt ?? now,
      updatedAt: now,
    };
    if (!reminder.title) throw new Error("提醒标题不能为空。");
    const index = reminders.findIndex((item) => item.id === id);
    if (index >= 0) reminders[index] = reminder;
    else reminders.push(reminder);
    writeBrowserReminders(reminders);
    return structuredClone(reminder);
  },

  async delete(id: string): Promise<void> {
    if (isDesktopRuntime()) return command<void>("delete_reminder", { reminderId: id });
    writeBrowserReminders(readBrowserReminders().filter((reminder) => reminder.id !== id));
  },

  async setEnabled(id: string, enabled: boolean): Promise<Reminder> {
    if (isDesktopRuntime()) {
      return command<Reminder>("set_reminder_enabled", { reminderId: id, enabled });
    }
    const reminders = readBrowserReminders();
    const reminder = reminders.find((item) => item.id === id);
    if (!reminder) throw new Error("没有找到这个提醒。");
    reminder.enabled = enabled;
    reminder.nextDueAt = enabled ? nextBrowserDue(reminder.schedule) : null;
    reminder.updatedAt = toLocalDateTime(new Date(), true);
    writeBrowserReminders(reminders);
    return structuredClone(reminder);
  },
};

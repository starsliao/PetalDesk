import { invoke } from "@tauri-apps/api/core";
import { previousStorageKey, readMigratedStorageValue } from "./storage";

export interface GanttTask {
  id: string;
  name: string;
  progress: number;
  startDate: string;
  startHour: number;
  endDate: string;
  endHour: number;
  createdAt: string;
  updatedAt: string;
}

export interface UpsertGanttTaskRequest {
  id?: string | null;
  name: string;
  progress: number;
  startDate: string;
  startHour?: number;
  endDate: string;
  endHour?: number;
}

export interface GanttApi {
  isDesktop(): boolean;
  list(): Promise<GanttTask[]>;
  upsert(request: UpsertGanttTaskRequest): Promise<GanttTask>;
  delete(id: string): Promise<void>;
  reorder(ids: string[]): Promise<void>;
}

interface AppError {
  code?: string;
  message: string;
}

const DAY_MS = 86_400_000;
const DATE_KEY_PATTERN = /^(\d{4})-(\d{2})-(\d{2})$/;
const browserStorageKey = "petaldesk.browser-gantt.v1";
const previousBrowserStorageKey = previousStorageKey("browser-gantt.v1");

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
  return "甘特任务操作失败，请稍后重试。";
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

export function dateKeyToOrdinal(value: string): number {
  const match = DATE_KEY_PATTERN.exec(value);
  if (!match) throw new Error("日期格式无效。");
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const date = new Date(0);
  date.setUTCHours(0, 0, 0, 0);
  date.setUTCFullYear(year, month - 1, day);
  if (
    date.getUTCFullYear() !== year
    || date.getUTCMonth() !== month - 1
    || date.getUTCDate() !== day
  ) {
    throw new Error("日期格式无效。");
  }
  return Math.trunc(date.getTime() / DAY_MS);
}

export function ordinalToDateKey(ordinal: number): string {
  if (!Number.isSafeInteger(ordinal)) throw new Error("日期序号无效。");
  const date = new Date(ordinal * DAY_MS);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${String(date.getUTCFullYear()).padStart(4, "0")}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
}

export function addDateDays(value: string, days: number): string {
  return ordinalToDateKey(dateKeyToOrdinal(value) + Math.trunc(days));
}

export function dateHourToOrdinal(dateKey: string, hour: number): number {
  if (!Number.isInteger(hour) || hour < 0 || hour > 23) throw new Error("小时必须在 0 到 23 之间。");
  return dateKeyToOrdinal(dateKey) * 24 + hour;
}

export function ordinalToDateHour(ordinal: number): { dateKey: string; hour: number } {
  if (!Number.isSafeInteger(ordinal)) throw new Error("小时序号无效。");
  const dayOrdinal = Math.floor(ordinal / 24);
  return { dateKey: ordinalToDateKey(dayOrdinal), hour: ordinal - dayOrdinal * 24 };
}

export function todayDateKey(date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export function normalizeProgress(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, Math.round(value)));
}

function normalizeTask(value: unknown): GanttTask | null {
  if (typeof value !== "object" || value === null) return null;
  const task = value as Partial<GanttTask>;
  if (
    typeof task.id !== "string"
    || typeof task.name !== "string"
    || typeof task.startDate !== "string"
    || typeof task.endDate !== "string"
  ) return null;

  const startHour = validHour(task.startHour) ? task.startHour : 0;
  const endHour = validHour(task.endHour) ? task.endHour : 23;
  try {
    if (dateHourToOrdinal(task.startDate, startHour) > dateHourToOrdinal(task.endDate, endHour)) return null;
  } catch {
    return null;
  }

  const timestamp = new Date().toISOString();
  return {
    id: task.id,
    name: task.name.trim().slice(0, 200) || "新任务",
    progress: normalizeProgress(Number(task.progress)),
    startDate: task.startDate,
    startHour,
    endDate: task.endDate,
    endHour,
    createdAt: typeof task.createdAt === "string" ? task.createdAt : timestamp,
    updatedAt: typeof task.updatedAt === "string" ? task.updatedAt : timestamp,
  };
}

function validHour(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 23;
}

function readBrowserTasks(): GanttTask[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const parsed = JSON.parse(
      readMigratedStorageValue(localStorage, browserStorageKey, previousBrowserStorageKey) ?? "[]",
    ) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.map(normalizeTask).filter((task): task is GanttTask => task !== null);
  } catch {
    return [];
  }
}

function writeBrowserTasks(tasks: GanttTask[]): void {
  localStorage.setItem(browserStorageKey, JSON.stringify(tasks));
}

function normalizeRequest(request: UpsertGanttTaskRequest): UpsertGanttTaskRequest {
  const name = request.name.trim().slice(0, 200);
  if (!name) throw new Error("任务名称不能为空。");
  const startHour = request.startHour ?? 0;
  const endHour = request.endHour ?? 23;
  if (!validHour(startHour) || !validHour(endHour)) throw new Error("小时必须在 0 到 23 之间。");
  const start = dateHourToOrdinal(request.startDate, startHour);
  const end = dateHourToOrdinal(request.endDate, endHour);
  if (start > end) throw new Error("开始日期不能晚于结束日期。");
  return {
    id: request.id ?? null,
    name,
    progress: normalizeProgress(request.progress),
    startDate: request.startDate,
    startHour,
    endDate: request.endDate,
    endHour,
  };
}

function announceBrowserChange(): void {
  if (typeof window !== "undefined") window.dispatchEvent(new Event("petaldesk:gantt-changed"));
}

export const ganttApi: GanttApi = {
  isDesktop: isDesktopRuntime,

  async list(): Promise<GanttTask[]> {
    if (isDesktopRuntime()) return command<GanttTask[]>("list_gantt_tasks");
    return structuredClone(readBrowserTasks());
  },

  async upsert(request: UpsertGanttTaskRequest): Promise<GanttTask> {
    const normalized = normalizeRequest(request);
    if (isDesktopRuntime()) {
      return command<GanttTask>("upsert_gantt_task", { request: normalized });
    }

    const tasks = readBrowserTasks();
    const id = normalized.id ?? crypto.randomUUID();
    const existing = tasks.find((task) => task.id === id);
    const timestamp = new Date().toISOString();
    const task: GanttTask = {
      id,
      name: normalized.name,
      progress: normalized.progress,
      startDate: normalized.startDate,
      startHour: normalized.startHour ?? 0,
      endDate: normalized.endDate,
      endHour: normalized.endHour ?? 23,
      createdAt: existing?.createdAt ?? timestamp,
      updatedAt: timestamp,
    };
    const index = tasks.findIndex((item) => item.id === id);
    if (index >= 0) tasks[index] = task;
    else tasks.push(task);
    writeBrowserTasks(tasks);
    announceBrowserChange();
    return structuredClone(task);
  },

  async delete(id: string): Promise<void> {
    if (isDesktopRuntime()) return command<void>("delete_gantt_task", { taskId: id });
    writeBrowserTasks(readBrowserTasks().filter((task) => task.id !== id));
    announceBrowserChange();
  },

  async reorder(ids: string[]): Promise<void> {
    if (isDesktopRuntime()) return command<void>("reorder_gantt_tasks", { orderedIds: ids });

    const tasks = readBrowserTasks();
    const taskById = new Map(tasks.map((task) => [task.id, task]));
    if (
      ids.length !== tasks.length
      || new Set(ids).size !== ids.length
      || ids.some((id) => !taskById.has(id))
    ) {
      throw new Error("任务顺序与当前任务不匹配，请刷新后重试。");
    }
    writeBrowserTasks(ids.map((id) => taskById.get(id)!));
    announceBrowserChange();
  },
};

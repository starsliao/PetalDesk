import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  addDateDays,
  dateHourToOrdinal,
  dateKeyToOrdinal,
  ganttApi,
  normalizeProgress,
  ordinalToDateHour,
  ordinalToDateKey,
  todayDateKey,
} from "./gantt";
import { previousStorageKey } from "./storage";

const browserGanttKey = "petaldesk.browser-gantt.v1";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("gantt date helpers", () => {
  it("uses calendar-day ordinals without local timezone or DST drift", () => {
    expect(addDateDays("2024-02-28", 1)).toBe("2024-02-29");
    expect(addDateDays("2024-02-28", 2)).toBe("2024-03-01");
    expect(addDateDays("2026-03-08", 1)).toBe("2026-03-09");
    expect(ordinalToDateKey(dateKeyToOrdinal("2026-07-14"))).toBe("2026-07-14");
    expect(dateKeyToOrdinal("2026-07-15") - dateKeyToOrdinal("2026-07-14")).toBe(1);
  });

  it("rejects impossible dates and clamps progress", () => {
    expect(() => dateKeyToOrdinal("2026-02-30")).toThrow("日期格式无效");
    expect(() => dateKeyToOrdinal("14/07/2026")).toThrow("日期格式无效");
    expect(normalizeProgress(-20)).toBe(0);
    expect(normalizeProgress(52.6)).toBe(53);
    expect(normalizeProgress(180)).toBe(100);
  });

  it("formats today from local calendar fields", () => {
    expect(todayDateKey(new Date(2026, 6, 14, 23, 30))).toBe("2026-07-14");
  });

  it("round-trips hour ordinals across day boundaries", () => {
    const lastHour = dateHourToOrdinal("2026-07-14", 23);
    expect(ordinalToDateHour(lastHour)).toEqual({ dateKey: "2026-07-14", hour: 23 });
    expect(ordinalToDateHour(lastHour + 1)).toEqual({ dateKey: "2026-07-15", hour: 0 });
    expect(() => dateHourToOrdinal("2026-07-14", 24)).toThrow("小时必须在 0 到 23 之间");
  });
});

describe("gantt browser API", () => {
  it("moves tasks from the previous product storage key", async () => {
    await ganttApi.upsert({
      name: "迁移任务",
      progress: 30,
      startDate: "2026-07-14",
      endDate: "2026-07-15",
    });
    const previousValue = localStorage.getItem(browserGanttKey)!;
    localStorage.removeItem(browserGanttKey);
    const previousKey = previousStorageKey("browser-gantt.v1");
    localStorage.setItem(previousKey, previousValue);

    expect((await ganttApi.list())[0].name).toBe("迁移任务");
    expect(localStorage.getItem(browserGanttKey)).toBe(previousValue);
    expect(localStorage.getItem(previousKey)).toBeNull();
  });

  it("creates, updates and deletes locally persisted tasks", async () => {
    const created = await ganttApi.upsert({
      name: "  发布首版  ",
      progress: 15,
      startDate: "2026-07-14",
      endDate: "2026-07-20",
    });

    expect(created).toMatchObject({
      name: "发布首版",
      progress: 15,
      startDate: "2026-07-14",
      startHour: 0,
      endDate: "2026-07-20",
      endHour: 23,
    });
    expect(await ganttApi.list()).toEqual([created]);

    const updated = await ganttApi.upsert({
      id: created.id,
      name: "发布首版",
      progress: 125,
      startDate: "2026-07-15",
      endDate: "2026-07-22",
    });
    expect(updated.progress).toBe(100);
    expect(updated.startDate).toBe("2026-07-15");
    expect((await ganttApi.list())).toEqual([updated]);

    await ganttApi.delete(created.id);
    expect(await ganttApi.list()).toEqual([]);
  });

  it("does not persist invalid task ranges", async () => {
    await expect(ganttApi.upsert({
      name: "无效任务",
      progress: 0,
      startDate: "2026-07-20",
      endDate: "2026-07-14",
    })).rejects.toThrow("开始日期不能晚于结束日期");
    await expect(ganttApi.upsert({
      name: "无效小时",
      progress: 0,
      startDate: "2026-07-14",
      startHour: 18,
      endDate: "2026-07-14",
      endHour: 9,
    })).rejects.toThrow("开始日期不能晚于结束日期");
    expect(await ganttApi.list()).toEqual([]);
  });

  it("persists an explicit task order and rejects stale order requests", async () => {
    const first = await ganttApi.upsert({
      name: "第一项",
      progress: 0,
      startDate: "2026-07-14",
      endDate: "2026-07-15",
    });
    const second = await ganttApi.upsert({
      name: "第二项",
      progress: 50,
      startDate: "2026-07-14",
      endDate: "2026-07-16",
    });
    const third = await ganttApi.upsert({
      name: "第三项",
      progress: 100,
      startDate: "2026-07-14",
      endDate: "2026-07-17",
    });

    await ganttApi.reorder([third.id, first.id, second.id]);
    expect((await ganttApi.list()).map((task) => task.id)).toEqual([third.id, first.id, second.id]);

    await expect(ganttApi.reorder([third.id, first.id])).rejects.toThrow("任务顺序与当前任务不匹配");
    await expect(ganttApi.reorder([third.id, first.id, first.id])).rejects.toThrow("任务顺序与当前任务不匹配");
    expect((await ganttApi.list()).map((task) => task.id)).toEqual([third.id, first.id, second.id]);
  });
});

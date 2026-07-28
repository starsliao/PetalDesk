import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Reminder, ReminderApi, UpsertReminderRequest } from "../reminders";
import ReminderTool from "./ReminderTool.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function reminder(overrides: Partial<Reminder> = {}): Reminder {
  return {
    id: "550e8400-e29b-41d4-a716-446655440000",
    title: "喝水",
    message: "起来活动并喝一杯水",
    schedule: { kind: "daily", anchorAt: "2026-07-12T09:30:00" },
    enabled: true,
    nextDueAt: "2026-07-13T09:30:00",
    lastTriggeredAt: null,
    createdAt: "2026-07-12T08:00:00",
    updatedAt: "2026-07-12T08:00:00",
    ...overrides,
  };
}

function mockApi(items: Reminder[] = []): ReminderApi & {
  list: ReturnType<typeof vi.fn>;
  upsert: ReturnType<typeof vi.fn>;
  delete: ReturnType<typeof vi.fn>;
  setEnabled: ReturnType<typeof vi.fn>;
} {
  return {
    isDesktop: () => false,
    list: vi.fn().mockResolvedValue(items),
    upsert: vi.fn().mockImplementation(async (request: UpsertReminderRequest) => reminder({
      id: request.id ?? "new-id",
      title: request.title,
      message: request.message,
      schedule: request.schedule,
      enabled: request.enabled,
    })),
    delete: vi.fn().mockResolvedValue(undefined),
    setEnabled: vi.fn().mockImplementation(async (id: string, enabled: boolean) => reminder({ id, enabled })),
  };
}

describe("ReminderTool", () => {
  it("offers all six scheduling modes", async () => {
    const api = mockApi();
    const rendered = render(ReminderTool, { api });
    await waitFor(() => expect(api.list).toHaveBeenCalled());

    await fireEvent.click(rendered.getByRole("button", { name: "新建提醒" }));
    const mode = rendered.getByLabelText("定时方式");
    expect(mode.querySelectorAll("option")).toHaveLength(6);
    for (const label of ["固定时间一次", "间隔循环", "每天", "每周", "每月", "每年"]) {
      expect(rendered.getByRole("option", { name: label })).toBeInTheDocument();
    }
  });

  it("converts an interval amount and unit to seconds", async () => {
    const api = mockApi();
    const rendered = render(ReminderTool, { api });
    await waitFor(() => expect(api.list).toHaveBeenCalled());
    await fireEvent.click(rendered.getByRole("button", { name: "新建提醒" }));

    await fireEvent.input(rendered.getByPlaceholderText("例如：起来活动一下"), {
      target: { value: "深呼吸" },
    });
    await fireEvent.change(rendered.getByLabelText("定时方式"), { target: { value: "interval" } });
    await fireEvent.input(rendered.getByLabelText("间隔数值"), { target: { value: "2" } });
    await fireEvent.change(rendered.getByLabelText("间隔单位"), { target: { value: "hours" } });
    await fireEvent.input(rendered.getByLabelText("首次提醒时间"), {
      target: { value: "2026-07-12T12:00" },
    });
    await fireEvent.click(rendered.getByRole("button", { name: "创建提醒" }));

    await waitFor(() => expect(api.upsert).toHaveBeenCalledOnce());
    expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      title: "深呼吸",
      schedule: {
        kind: "interval",
        anchorAt: "2026-07-12T12:00",
        intervalSeconds: 7_200,
      },
    }));
  });

  it("fills the form when editing and keeps the reminder id", async () => {
    const existing = reminder({
      title: "周报",
      schedule: { kind: "weekly", anchorAt: "2026-07-13T17:45:00" },
    });
    const api = mockApi([existing]);
    const rendered = render(ReminderTool, { api });
    await waitFor(() => expect(rendered.getByText("周报")).toBeInTheDocument());

    await fireEvent.click(rendered.getByRole("button", { name: "编辑提醒“周报”" }));
    expect(rendered.getByPlaceholderText("例如：起来活动一下")).toHaveValue("周报");
    expect(rendered.getByLabelText("定时方式")).toHaveValue("weekly");
    expect(rendered.getByLabelText("首次提醒时间")).toHaveValue("2026-07-13T17:45");

    await fireEvent.click(rendered.getByRole("button", { name: "保存修改" }));
    await waitFor(() => expect(api.upsert).toHaveBeenCalledOnce());
    expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({ id: existing.id }));
  });

  it("can disable and delete a reminder", async () => {
    const existing = reminder();
    const api = mockApi([existing]);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const rendered = render(ReminderTool, { api });
    await waitFor(() => expect(rendered.getByText("喝水")).toBeInTheDocument());

    expect(rendered.getByText("已启用")).toBeInTheDocument();
    expect(rendered.getByText("下次 2026-07-13 09:30")).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "停用提醒“喝水”" }));
    await waitFor(() => expect(api.setEnabled).toHaveBeenCalledWith(existing.id, false));

    await fireEvent.click(rendered.getByRole("button", { name: "删除提醒“喝水”" }));
    await waitFor(() => expect(api.delete).toHaveBeenCalledWith(existing.id));
  });
});

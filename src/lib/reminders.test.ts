import { beforeEach, describe, expect, it } from "vitest";
import { remindersApi } from "./reminders";
import { previousStorageKey } from "./storage";

const browserRemindersKey = "petaldesk.browser-reminders.v1";

beforeEach(() => {
  localStorage.clear();
});

describe("reminder browser storage", () => {
  it("moves reminders from the previous product storage key", async () => {
    await remindersApi.upsert({
      title: "迁移提醒",
      message: "保留已有数据",
      schedule: { kind: "daily", anchorAt: "2030-01-01T09:00:00" },
      enabled: true,
    });
    const previousValue = localStorage.getItem(browserRemindersKey)!;
    localStorage.removeItem(browserRemindersKey);
    const previousKey = previousStorageKey("browser-reminders.v1");
    localStorage.setItem(previousKey, previousValue);

    expect((await remindersApi.list())[0].title).toBe("迁移提醒");
    expect(localStorage.getItem(browserRemindersKey)).toBe(previousValue);
    expect(localStorage.getItem(previousKey)).toBeNull();
  });
});

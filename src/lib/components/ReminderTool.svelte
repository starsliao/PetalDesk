<script lang="ts">
  import { onMount } from "svelte";
  import { Bell, LoaderCircle, Pencil, Plus, Power, Save, Trash2, X } from "@lucide/svelte";
  import {
    remindersApi,
    toLocalDateTime,
    type Reminder,
    type ReminderApi,
    type ReminderKind,
    type UpsertReminderRequest,
  } from "../reminders";

  interface Props {
    api?: ReminderApi;
  }

  type IntervalUnit = "minutes" | "hours" | "days" | "weeks";

  let { api = remindersApi }: Props = $props();

  const scheduleOptions: ReadonlyArray<{ value: ReminderKind; label: string }> = [
    { value: "once", label: "固定时间一次" },
    { value: "interval", label: "间隔循环" },
    { value: "daily", label: "每天" },
    { value: "weekly", label: "每周" },
    { value: "monthly", label: "每月" },
    { value: "yearly", label: "每年" },
  ];

  const intervalUnits: ReadonlyArray<{ value: IntervalUnit; label: string; seconds: number }> = [
    { value: "minutes", label: "分钟", seconds: 60 },
    { value: "hours", label: "小时", seconds: 3_600 },
    { value: "days", label: "天", seconds: 86_400 },
    { value: "weeks", label: "周", seconds: 604_800 },
  ];

  let reminders = $state<Reminder[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state("");
  let formOpen = $state(false);
  let editingId = $state<string | null>(null);
  let title = $state("");
  let message = $state("");
  let scheduleKind = $state<ReminderKind>("once");
  let anchorAt = $state(defaultAnchor());
  let intervalAmount = $state(30);
  let intervalUnit = $state<IntervalUnit>("minutes");
  let enabled = $state(true);

  let sortedReminders = $derived.by(() => [...reminders].sort((left, right) => {
    if (left.enabled !== right.enabled) return Number(right.enabled) - Number(left.enabled);
    return (left.nextDueAt ?? "~").localeCompare(right.nextDueAt ?? "~");
  }));

  let formHeading = $derived(editingId ? "编辑提醒" : "新建提醒");
  let anchorLabel = $derived(scheduleKind === "once" ? "提醒时间" : "首次提醒时间");
  let scheduleHint = $derived.by(() => {
    if (scheduleKind === "interval") return "从首次提醒时间开始，按设定间隔重复。";
    if (scheduleKind === "daily") return "以后每天在首次提醒的时刻重复。";
    if (scheduleKind === "weekly") return "以后每周在首次提醒的星期和时刻重复。";
    if (scheduleKind === "monthly") return "以后每月在同一日期和时刻重复；短月自动使用月末。";
    if (scheduleKind === "yearly") return "以后每年在同一月日和时刻重复；闰日自动使用二月末。";
    return "仅在这个日期时间提醒一次。";
  });

  function defaultAnchor(): string {
    const date = new Date(Date.now() + 5 * 60_000);
    date.setSeconds(0, 0);
    return toLocalDateTime(date);
  }

  function resetForm(): void {
    editingId = null;
    title = "";
    message = "";
    scheduleKind = "once";
    anchorAt = defaultAnchor();
    intervalAmount = 30;
    intervalUnit = "minutes";
    enabled = true;
    error = "";
  }

  function startCreate(): void {
    resetForm();
    formOpen = true;
  }

  function intervalParts(seconds: number | null | undefined): { amount: number; unit: IntervalUnit } {
    const value = Math.max(60, seconds ?? 60);
    for (const unit of [...intervalUnits].reverse()) {
      if (value % unit.seconds === 0) return { amount: value / unit.seconds, unit: unit.value };
    }
    return { amount: Math.max(1, Math.ceil(value / 60)), unit: "minutes" };
  }

  function startEdit(reminder: Reminder): void {
    editingId = reminder.id;
    title = reminder.title;
    message = reminder.message;
    scheduleKind = reminder.schedule.kind;
    anchorAt = reminder.schedule.anchorAt.slice(0, 16);
    const interval = intervalParts(reminder.schedule.intervalSeconds);
    intervalAmount = interval.amount;
    intervalUnit = interval.unit;
    enabled = reminder.enabled;
    error = "";
    formOpen = true;
  }

  function cancelForm(): void {
    formOpen = false;
    resetForm();
  }

  function currentIntervalSeconds(): number | null {
    if (scheduleKind !== "interval") return null;
    const unit = intervalUnits.find((item) => item.value === intervalUnit) ?? intervalUnits[0];
    return Math.max(1, Number(intervalAmount) || 1) * unit.seconds;
  }

  async function refresh(): Promise<void> {
    try {
      reminders = await api.list();
      error = "";
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "无法读取提醒。";
    } finally {
      loading = false;
    }
  }

  async function saveReminder(): Promise<void> {
    const normalizedTitle = title.trim();
    if (!normalizedTitle) {
      error = "请输入提醒标题。";
      return;
    }
    if (!anchorAt) {
      error = "请选择提醒时间。";
      return;
    }

    const request: UpsertReminderRequest = {
      id: editingId,
      title: normalizedTitle,
      message: message.trim(),
      enabled,
      schedule: {
        kind: scheduleKind,
        anchorAt,
        intervalSeconds: currentIntervalSeconds(),
      },
    };

    saving = true;
    error = "";
    try {
      await api.upsert(request);
      formOpen = false;
      resetForm();
      await refresh();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "保存提醒失败。";
    } finally {
      saving = false;
    }
  }

  async function toggleReminder(reminder: Reminder): Promise<void> {
    error = "";
    try {
      await api.setEnabled(reminder.id, !reminder.enabled);
      await refresh();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "更新提醒状态失败。";
    }
  }

  async function removeReminder(reminder: Reminder): Promise<void> {
    if (typeof window !== "undefined" && !window.confirm(`删除提醒“${reminder.title}”？`)) return;
    error = "";
    try {
      await api.delete(reminder.id);
      if (editingId === reminder.id) cancelForm();
      await refresh();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "删除提醒失败。";
    }
  }

  async function closeWindow(): Promise<void> {
    if (!api.isDesktop()) return;
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const appWindow = getCurrentWindow();
      try {
        await appWindow.close();
      } catch {
        await appWindow.destroy();
      }
    } catch {
      // Browser preview has no native window to close.
    }
  }

  function parseLocal(value: string): Date | null {
    const parsed = new Date(value.length === 16 ? `${value}:00` : value);
    return Number.isNaN(parsed.getTime()) ? null : parsed;
  }

  function dateTimeLabel(value: string | null): string {
    if (!value) return "";
    const date = parseLocal(value);
    if (!date) return value;
    const pad = (part: number) => String(part).padStart(2, "0");
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
  }

  function scheduleLabel(reminder: Reminder): string {
    const anchor = parseLocal(reminder.schedule.anchorAt);
    if (!anchor) return scheduleOptions.find((item) => item.value === reminder.schedule.kind)?.label ?? "提醒";
    const pad = (part: number) => String(part).padStart(2, "0");
    const time = `${pad(anchor.getHours())}:${pad(anchor.getMinutes())}`;
    if (reminder.schedule.kind === "once") return `一次 · ${dateTimeLabel(reminder.schedule.anchorAt)}`;
    if (reminder.schedule.kind === "interval") {
      const parts = intervalParts(reminder.schedule.intervalSeconds);
      const unit = intervalUnits.find((item) => item.value === parts.unit)?.label ?? "分钟";
      return `每 ${parts.amount} ${unit}`;
    }
    if (reminder.schedule.kind === "daily") return `每天 ${time}`;
    if (reminder.schedule.kind === "weekly") {
      const weekdays = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
      return `每周 ${weekdays[anchor.getDay()]} ${time}`;
    }
    if (reminder.schedule.kind === "monthly") return `每月 ${anchor.getDate()} 日 ${time}`;
    return `每年 ${anchor.getMonth() + 1} 月 ${anchor.getDate()} 日 ${time}`;
  }

  function stateLabel(reminder: Reminder): string {
    if (reminder.enabled) return "已启用";
    if (reminder.schedule.kind === "once" && reminder.lastTriggeredAt) return "已完成";
    return "已停用";
  }

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void refresh();

    if (api.isDesktop()) {
      void import("@tauri-apps/api/event").then(async ({ listen }) => {
        const cleanup = await listen("reminder_changed", () => void refresh());
        if (disposed) cleanup();
        else unlisten = cleanup;
      }).catch(() => undefined);
    }

    return () => {
      disposed = true;
      unlisten?.();
    };
  });
</script>

<section class="reminder-tool" data-testid="reminder-tool" aria-label="提醒管理">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Bell size={18} aria-hidden="true" />
      <h1 data-tauri-drag-region>提醒</h1>
    </div>
    <div class="window-actions">
      <button type="button" class="primary-icon" aria-label="新建提醒" title="新建提醒" onclick={startCreate}>
        <Plus size={18} aria-hidden="true" />
      </button>
      <button type="button" aria-label="关闭提醒窗口" title="关闭" onclick={() => void closeWindow()}>
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  </header>

  <main>
    {#if error}
      <div class="error-banner" role="alert">{error}</div>
    {/if}

    {#if formOpen}
      <form class="reminder-form" onsubmit={(event) => { event.preventDefault(); void saveReminder(); }}>
        <div class="form-heading">
          <div>
            <h2>{formHeading}</h2>
            <p>{scheduleHint}</p>
          </div>
          <button type="button" class="quiet-button" aria-label="取消编辑" onclick={cancelForm}>
            <X size={16} aria-hidden="true" />
          </button>
        </div>

        <label>
          <span>标题</span>
          <input bind:value={title} maxlength="120" placeholder="例如：起来活动一下" />
        </label>

        <label>
          <span>通知内容</span>
          <textarea bind:value={message} maxlength="500" rows="2" placeholder="可选，到点时显示在系统通知中"></textarea>
        </label>

        <div class="form-grid">
          <label>
            <span>定时方式</span>
            <select bind:value={scheduleKind} aria-label="定时方式">
              {#each scheduleOptions as option}
                <option value={option.value}>{option.label}</option>
              {/each}
            </select>
          </label>

          <label>
            <span>{anchorLabel}</span>
            <input type="datetime-local" step="60" bind:value={anchorAt} aria-label={anchorLabel} />
          </label>
        </div>

        {#if scheduleKind === "interval"}
          <div class="interval-fields">
            <label>
              <span>间隔数值</span>
              <input type="number" min="1" step="1" bind:value={intervalAmount} aria-label="间隔数值" />
            </label>
            <label>
              <span>间隔单位</span>
              <select bind:value={intervalUnit} aria-label="间隔单位">
                {#each intervalUnits as unit}
                  <option value={unit.value}>{unit.label}</option>
                {/each}
              </select>
            </label>
          </div>
        {/if}

        <div class="form-footer">
          <label class="enabled-field">
            <input type="checkbox" bind:checked={enabled} />
            <span>保存后启用</span>
          </label>
          <div class="form-actions">
            <button type="button" class="secondary-button" onclick={cancelForm}>取消</button>
            <button type="submit" class="primary-button" disabled={saving}>
              {#if saving}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{:else}<Save size={15} aria-hidden="true" />{/if}
              {editingId ? "保存修改" : "创建提醒"}
            </button>
          </div>
        </div>
      </form>
    {/if}

    <section class="reminder-list" aria-label="提醒任务">
      <div class="list-heading">
        <div>
          <h2>定时任务</h2>
          <span>{reminders.length} 项</span>
        </div>
        {#if !formOpen}
          <button type="button" class="new-button" onclick={startCreate}>
            <Plus size={16} aria-hidden="true" /> 新建
          </button>
        {/if}
      </div>

      {#if loading}
        <div class="empty-state" aria-busy="true">
          <LoaderCircle class="spinner" size={22} aria-hidden="true" />
          <span>正在读取提醒…</span>
        </div>
      {:else if sortedReminders.length === 0}
        <div class="empty-state">
          <Bell size={28} aria-hidden="true" />
          <strong>还没有提醒</strong>
          <span>创建一个一次性或周期提醒，到点由系统通知你。</span>
          <button type="button" class="primary-button" onclick={startCreate}>
            <Plus size={16} aria-hidden="true" /> 新建提醒
          </button>
        </div>
      {:else}
        <div class="cards">
          {#each sortedReminders as reminder (reminder.id)}
            <article class:disabled={!reminder.enabled} class="reminder-card">
              <div class="card-main">
                <div class="card-title-row">
                  <strong>{reminder.title}</strong>
                  <span class:enabled={reminder.enabled} class="state-pill">{stateLabel(reminder)}</span>
                </div>
                {#if reminder.message}<p>{reminder.message}</p>{/if}
                <div class="schedule-line">
                  <span>{scheduleLabel(reminder)}</span>
                  {#if reminder.enabled}
                    <span>{reminder.nextDueAt ? `下次 ${dateTimeLabel(reminder.nextDueAt)}` : "下次时间待计算"}</span>
                  {/if}
                  {#if reminder.lastTriggeredAt}<span>上次 {dateTimeLabel(reminder.lastTriggeredAt)}</span>{/if}
                </div>
              </div>
              <div class="card-actions" aria-label={`${reminder.title}操作`}>
                <button
                  type="button"
                  class:active={reminder.enabled}
                  aria-label={reminder.enabled ? `停用提醒“${reminder.title}”` : `启用提醒“${reminder.title}”`}
                  aria-pressed={reminder.enabled}
                  title={reminder.enabled ? "停用" : "启用"}
                  onclick={() => void toggleReminder(reminder)}
                >
                  <Power size={16} aria-hidden="true" />
                </button>
                <button type="button" aria-label={`编辑提醒“${reminder.title}”`} title="编辑" onclick={() => startEdit(reminder)}>
                  <Pencil size={16} aria-hidden="true" />
                </button>
                <button class="danger-button" type="button" aria-label={`删除提醒“${reminder.title}”`} title="删除" onclick={() => void removeReminder(reminder)}>
                  <Trash2 size={16} aria-hidden="true" />
                </button>
              </div>
            </article>
          {/each}
        </div>
      {/if}
    </section>
  </main>
</section>

<style>
  .reminder-tool { display: grid; width: 100%; height: 100%; min-width: 0; min-height: 0; grid-template-rows: 44px minmax(0, 1fr); color: #202020; background: #f3f3f3; }
  .titlebar { display: flex; min-width: 0; align-items: center; justify-content: space-between; padding: 5px 6px 5px 12px; background: rgb(250 250 250 / 96%); border-bottom: 1px solid #d7d7d7; user-select: none; }
  .brand, .window-actions, .form-heading, .form-footer, .form-actions, .list-heading, .list-heading > div, .card-title-row, .schedule-line, .card-actions, .new-button, .primary-button { display: flex; align-items: center; }
  .brand { min-width: 0; gap: 8px; color: #5b4b89; }
  h1 { margin: 0; color: #252525; font-size: 14px; font-weight: 650; }
  .window-actions { gap: 2px; }
  button { font: inherit; }
  .window-actions button, .quiet-button, .card-actions button { display: grid; width: 32px; height: 32px; padding: 0; place-items: center; color: #555; background: transparent; border: 0; border-radius: 5px; cursor: pointer; }
  .window-actions button:hover, .quiet-button:hover, .card-actions button:hover { color: #202020; background: #e7e7e7; }
  .window-actions .primary-icon { color: #5c3fa3; }
  .window-actions button:last-child:hover { color: #fff; background: #c42b1c; }
  main { min-width: 0; min-height: 0; padding: 16px; overflow: auto; }
  .error-banner { margin-bottom: 12px; padding: 9px 11px; color: #8c1d14; font-size: 12.5px; line-height: 1.4; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .reminder-form { padding: 16px; margin-bottom: 15px; background: #fff; border: 1px solid #d8d8d8; border-radius: 8px; box-shadow: 0 2px 9px rgb(0 0 0 / 5%); }
  .form-heading { justify-content: space-between; gap: 12px; margin-bottom: 14px; }
  h2 { margin: 0; color: #262626; font-size: 15px; font-weight: 650; }
  .form-heading p { margin: 4px 0 0; color: #6a6a6a; font-size: 11.5px; line-height: 1.4; }
  .reminder-form > label, .form-grid label, .interval-fields label { display: grid; min-width: 0; gap: 5px; margin-bottom: 12px; color: #4f4f4f; font-size: 12px; }
  input, textarea, select { width: 100%; min-width: 0; color: #202020; font: inherit; background: #fff; border: 1px solid #b8b8b8; border-radius: 5px; outline: 0; }
  input, select { height: 34px; padding: 0 9px; }
  textarea { min-height: 58px; padding: 8px 9px; resize: vertical; line-height: 1.4; }
  input:focus, textarea:focus, select:focus { border-color: #7352b9; box-shadow: inset 0 -2px #7352b9; }
  .form-grid, .interval-fields { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.25fr); gap: 10px; }
  .interval-fields { grid-template-columns: minmax(0, 1fr) minmax(120px, .7fr); }
  .form-footer { justify-content: space-between; gap: 12px; padding-top: 3px; }
  .enabled-field { display: flex !important; align-items: center; gap: 7px !important; margin: 0 !important; }
  .enabled-field input { width: 16px; height: 16px; margin: 0; accent-color: #6845ad; }
  .form-actions { gap: 7px; }
  .primary-button, .secondary-button, .new-button { min-height: 32px; justify-content: center; gap: 6px; padding: 5px 12px; border-radius: 5px; cursor: pointer; }
  .primary-button { color: #fff; background: #6845ad; border: 1px solid #573697; }
  .primary-button:hover { background: #5a399b; }
  .primary-button:disabled { cursor: default; opacity: .6; }
  .secondary-button, .new-button { color: #333; background: #fff; border: 1px solid #bdbdbd; }
  .secondary-button:hover, .new-button:hover { background: #f5f5f5; }
  .reminder-list { min-width: 0; }
  .list-heading { justify-content: space-between; gap: 12px; min-height: 34px; margin-bottom: 9px; }
  .list-heading > div { gap: 8px; }
  .list-heading span { color: #777; font-size: 11.5px; }
  .cards { display: grid; gap: 9px; }
  .reminder-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; padding: 13px 8px 12px 14px; background: #fff; border: 1px solid #d8d8d8; border-radius: 7px; }
  .reminder-card.disabled { background: #fafafa; }
  .card-main { min-width: 0; }
  .card-title-row { min-width: 0; justify-content: space-between; gap: 9px; }
  .card-title-row strong { min-width: 0; overflow: hidden; font-size: 13.5px; text-overflow: ellipsis; white-space: nowrap; }
  .state-pill { flex: 0 0 auto; max-width: 190px; padding: 2px 7px; overflow: hidden; color: #6d6d6d; font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap; background: #ededed; border-radius: 999px; }
  .state-pill.enabled { color: #3c246f; background: #eee7fb; }
  .reminder-card p { margin: 7px 0 0; overflow: hidden; color: #555; font-size: 12px; line-height: 1.4; overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
  .schedule-line { flex-wrap: wrap; gap: 5px 13px; margin-top: 9px; color: #747474; font-size: 10.8px; }
  .card-actions { align-self: center; flex-direction: column; gap: 1px; }
  .card-actions button.active { color: #6845ad; background: #eee7fb; }
  .card-actions .danger-button:hover { color: #c42b1c; background: #fff0ed; }
  .empty-state { display: flex; min-height: 220px; padding: 28px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: #777; text-align: center; background: #fff; border: 1px dashed #c7c7c7; border-radius: 8px; }
  .empty-state strong { color: #333; font-size: 14px; }
  .empty-state span { max-width: 330px; font-size: 12px; line-height: 1.5; }
  .empty-state .primary-button { margin-top: 4px; }
  :global(.spinner) { animation: spin 850ms linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 500px) { main { padding: 12px; } .form-grid, .interval-fields { grid-template-columns: 1fr; gap: 0; } .form-footer { align-items: stretch; flex-direction: column; } .form-actions { justify-content: flex-end; } .state-pill { max-width: 130px; } }
</style>

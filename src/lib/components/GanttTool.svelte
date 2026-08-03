<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    ChartGantt,
    CalendarDays,
    CheckCircle2,
    ChevronLeft,
    ChevronRight,
    GripVertical,
    LoaderCircle,
    Pencil,
    Plus,
    LocateFixed,
    Trash2,
    X,
    ZoomIn,
    ZoomOut,
  } from "@lucide/svelte";
  import {
    addDateDays,
    dateHourToOrdinal,
    dateKeyToOrdinal,
    ganttApi,
    normalizeProgress,
    ordinalToDateHour,
    todayDateKey,
    type GanttApi,
    type GanttTask,
    type UpsertGanttTaskRequest,
  } from "../gantt";
  import { addUpdateInstallPreparation } from "../updater";

  interface Props {
    api?: GanttApi;
    today?: string;
    saveDelayMs?: number;
  }

  type DragKind = "start" | "end" | "move";
  type TaskStatus = "not-started" | "in-progress" | "completed";
  type TaskFilter = "all" | TaskStatus;
  type DropPosition = "before" | "after";

  interface TimelineDrag {
    taskId: string;
    kind: DragKind;
    pointerId: number;
    originX: number;
    initialStartHour: number;
    initialEndHour: number;
    changed: boolean;
  }

  interface AxisPan {
    pointerId: number;
    originX: number;
    initialAxisStartHour: number;
    initialScrollLeft: number;
    unitHours: (typeof UNIT_HOURS)[number];
    changed: boolean;
  }

  interface ReorderPointer {
    pointerId: number;
    sourceId: string;
  }

  interface TaskContextMenu {
    taskId: string;
    x: number;
    y: number;
  }

  interface NameFit {
    fontSize: number;
    twoLines: boolean;
    truncated: boolean;
  }

  interface TaskRangeLayout {
    visible: boolean;
    left: number;
    width: number;
    startClipped: boolean;
    endClipped: boolean;
  }

  interface AxisDay {
    ordinal: number;
    startHour: number;
    endHour: number;
    dateKey: string;
    boundaryLabel: string;
    primaryLabel: string;
    secondaryLabel: string;
    weekend: boolean;
    today: boolean;
  }

  let { api = ganttApi, today = todayDateKey(), saveDelayMs = 420 }: Props = $props();

  const DAY_WIDTH = 28;
  const AXIS_CELL_COUNT = 56;
  const HOURS_PER_DAY = 24;
  const UNIT_HOURS = [168, 24, 12, 6, 3, 1] as const;
  const PAN_CELLS = 7;
  const LOCATE_LEADING_CELLS = 6;
  const WHEEL_GESTURE_END_MS = 180;
  const MIN_TIMELINE_HOUR = dateHourToOrdinal("0001-01-01", 0);
  const MAX_TIMELINE_HOUR = dateHourToOrdinal("9999-12-31", 23);
  const weekdays = ["日", "一", "二", "三", "四", "五", "六"];
  const filterOptions: ReadonlyArray<{ value: TaskFilter; label: string }> = [
    { value: "all", label: "全部" },
    { value: "not-started", label: "未开始" },
    { value: "in-progress", label: "进行中" },
    { value: "completed", label: "已完成" },
  ];

  function alignAxisHour(
    hour: number,
    unit: (typeof UNIT_HOURS)[number],
    nearest = false,
  ): number {
    if (unit === 168) {
      const dayOrdinal = Math.floor(hour / HOURS_PER_DAY);
      const weekday = new Date(dayOrdinal * 86_400_000).getUTCDay();
      let monday = (dayOrdinal - ((weekday + 6) % 7)) * HOURS_PER_DAY;
      if (nearest && hour - monday >= unit / 2) monday += unit;
      return monday;
    }
    const ratio = hour / unit;
    return (nearest ? Math.round(ratio) : Math.floor(ratio)) * unit;
  }

  function defaultAxisStartHour(dateKey: string): number {
    return clampAxisStart(
      alignAxisHour(dateHourToOrdinal(dateKey, 0) - PAN_CELLS * HOURS_PER_DAY, 24),
      24,
    );
  }

  function clampAxisStart(hour: number, unit: (typeof UNIT_HOURS)[number]): number {
    const maxStart = MAX_TIMELINE_HOUR + 1 - AXIS_CELL_COUNT * unit;
    return Math.max(MIN_TIMELINE_HOUR, Math.min(maxStart, hour));
  }

  function granularityLabel(hours: number): string {
    if (hours === 168) return "周";
    if (hours === 24) return "天";
    return `${hours} 小时`;
  }

  let tasks = $state<GanttTask[]>([]);
  let loading = $state(true);
  let creating = $state(false);
  let savingCount = $state(0);
  let error = $state("");
  let unitHours = $state<(typeof UNIT_HOURS)[number]>(24);
  // svelte-ignore state_referenced_locally
  let axisStartHour = $state(defaultAxisStartHour(today));
  let axisEndHour = $derived(axisStartHour + AXIS_CELL_COUNT * unitHours);
  let axisCellCount = AXIS_CELL_COUNT;
  let timelineWidth = $derived(axisCellCount * DAY_WIDTH);
  let dragState = $state<TimelineDrag | null>(null);
  let axisPanState = $state<AxisPan | null>(null);
  let activeFilter = $state<TaskFilter>("all");
  let compactFilterLayout = $state(false);
  let reorderDragId = $state<string | null>(null);
  let reorderDropId = $state<string | null>(null);
  let reorderDropPosition = $state<DropPosition>("after");
  let reorderPointerState = $state<ReorderPointer | null>(null);
  let reordering = $state(false);
  let completedPulseIds = $state<string[]>([]);
  let editingTaskId = $state<string | null>(null);
  let editingOriginalName = $state<string | null>(null);
  let taskContextMenu = $state<TaskContextMenu | null>(null);
  let nameFits = $state<Record<string, NameFit>>({});
  let nameFitFrame: number | undefined;
  let ganttToolElement = $state<HTMLElement | null>(null);
  let tableScrollElement = $state<HTMLDivElement | null>(null);
  let progressHeaderElement = $state<HTMLDivElement | null>(null);
  let axisHeaderElement = $state<HTMLDivElement | null>(null);

  const saveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const saveFlights = new Map<string, Promise<void>>();
  const dirtyIds = new Set<string>();
  const completionTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let externalRefreshPending = false;
  let wheelGestureLocked = false;
  let wheelUnlockTimer: ReturnType<typeof setTimeout> | undefined;

  let axisDays = $derived.by<AxisDay[]>(() => {
    const todayHour = dateHourToOrdinal(today, 12);
    return Array.from({ length: axisCellCount }, (_, index) => {
      const startHour = axisStartHour + index * unitHours;
      const endHour = startHour + unitHours;
      const { dateKey, hour } = ordinalToDateHour(startHour);
      const date = new Date(dateKeyToOrdinal(dateKey) * 86_400_000);
      const day = date.getUTCDate();
      const weekdayIndex = date.getUTCDay();
      const dayLevel = unitHours >= HOURS_PER_DAY;
      const boundary = index === 0 || hour === 0;
      const previousDate = index === 0
        ? null
        : new Date(dateKeyToOrdinal(ordinalToDateHour(startHour - unitHours).dateKey) * 86_400_000);
      const monthChanged = previousDate === null
        || previousDate.getUTCFullYear() !== date.getUTCFullYear()
        || previousDate.getUTCMonth() !== date.getUTCMonth();
      const boundaryLabel = dayLevel
        ? (monthChanged ? `${date.getUTCMonth() + 1}月` : "")
        : (boundary ? `${date.getUTCMonth() + 1}/${day}` : "");
      return {
        ordinal: startHour,
        startHour,
        endHour,
        dateKey,
        boundaryLabel,
        primaryLabel: dayLevel ? String(day) : String(hour).padStart(2, "0"),
        secondaryLabel: dayLevel ? weekdays[weekdayIndex] : "",
        weekend: weekdayIndex === 0 || weekdayIndex === 6,
        today: todayHour >= startHour && todayHour < endHour,
      };
    });
  });
  let visibleTasks = $derived(tasks.filter((task) => (
    matchesFilter(task, activeFilter) || completedPulseIds.includes(task.id)
  )));
  let statusCounts = $derived.by<Record<TaskFilter, number>>(() => {
    const counts: Record<TaskFilter, number> = {
      all: tasks.length,
      "not-started": 0,
      "in-progress": 0,
      completed: 0,
    };
    for (const task of tasks) counts[taskStatus(task)] += 1;
    return counts;
  });

  function taskStatus(task: GanttTask): TaskStatus {
    if (task.progress <= 0) return "not-started";
    if (task.progress >= 100) return "completed";
    return "in-progress";
  }

  function matchesFilter(task: GanttTask, filter: TaskFilter): boolean {
    return filter === "all" || taskStatus(task) === filter;
  }

  function taskRequest(task: GanttTask): UpsertGanttTaskRequest {
    return {
      id: task.id,
      name: task.name.trim() || "新任务",
      progress: normalizeProgress(task.progress),
      startDate: task.startDate,
      startHour: task.startHour,
      endDate: task.endDate,
      endHour: task.endHour,
    };
  }

  function taskStartOrdinal(task: GanttTask): number {
    return dateHourToOrdinal(task.startDate, Number.isInteger(task.startHour) ? task.startHour : 0);
  }

  function taskEndOrdinal(task: GanttTask): number {
    return dateHourToOrdinal(task.endDate, Number.isInteger(task.endHour) ? task.endHour : 23);
  }

  function rangePatch(startHour: number, endHour: number): Partial<GanttTask> {
    const start = ordinalToDateHour(startHour);
    const end = ordinalToDateHour(endHour);
    return {
      startDate: start.dateKey,
      startHour: start.hour,
      endDate: end.dateKey,
      endHour: end.hour,
    };
  }

  function taskById(id: string): GanttTask | undefined {
    return tasks.find((task) => task.id === id);
  }

  function updateTask(id: string, patch: Partial<GanttTask>): void {
    tasks = tasks.map((task) => task.id === id ? { ...task, ...patch } : task);
    scheduleNameFit();
  }

  function taskNameFit(taskId: string): NameFit {
    return nameFits[taskId] ?? { fontSize: 13, twoLines: false, truncated: false };
  }

  function scheduleNameFit(): void {
    if (typeof window === "undefined") return;
    if (nameFitFrame !== undefined) cancelAnimationFrame(nameFitFrame);
    nameFitFrame = requestAnimationFrame(() => {
      nameFitFrame = undefined;
      const elements = ganttToolElement?.querySelectorAll<HTMLElement>("[data-name-fit-id]") ?? [];
      if (elements.length === 0) return;
      const next = { ...nameFits };
      for (const element of elements) {
        const taskId = element.dataset.nameFitId;
        const task = taskId ? taskById(taskId) : undefined;
        const available = element.clientWidth;
        if (!taskId || !task || available <= 0) continue;
        let fontSize = 13;
        const textUnits = Array.from(task.name).reduce((total, character) => {
          if (character === " ") return total + .32;
          return total + (/^[\u0000-\u00ff]$/.test(character) ? .58 : 1);
        }, 0);
        const measure = () => textUnits * fontSize;
        let textWidth = measure();
        while (textWidth > available && fontSize > 10) {
          fontSize = Math.max(10, fontSize - .5);
          textWidth = measure();
        }
        const twoLines = textWidth > available;
        next[taskId] = {
          fontSize,
          twoLines,
          truncated: twoLines && textWidth > available * 2,
        };
      }
      nameFits = next;
    });
  }

  function openTaskContextMenu(event: MouseEvent, task: GanttTask): void {
    event.preventDefault();
    event.stopPropagation();
    const menuWidth = 132;
    const menuHeight = 76;
    taskContextMenu = {
      taskId: task.id,
      x: Math.max(6, Math.min(event.clientX, window.innerWidth - menuWidth - 6)),
      y: Math.max(6, Math.min(event.clientY, window.innerHeight - menuHeight - 6)),
    };
  }

  async function beginNameEdit(taskId: string): Promise<void> {
    taskContextMenu = null;
    editingOriginalName = taskById(taskId)?.name ?? null;
    editingTaskId = taskId;
    await tick();
    const input = document.querySelector<HTMLInputElement>(`input[data-task-edit-id="${taskId}"]`);
    input?.focus();
    input?.select();
  }

  function handleNameEditKeydown(event: KeyboardEvent, task: GanttTask): void {
    if (event.key === "Enter") {
      event.preventDefault();
      finishNameEdit(task);
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelNameEdit();
    }
  }

  function cancelNameEdit(): void {
    const taskId = editingTaskId;
    if (taskId && editingOriginalName !== null && taskById(taskId)) {
      const timer = saveTimers.get(taskId);
      if (timer) clearTimeout(timer);
      saveTimers.delete(taskId);
      dirtyIds.delete(taskId);
      updateTask(taskId, { name: editingOriginalName });
    }
    editingTaskId = null;
    editingOriginalName = null;
    taskContextMenu = null;
  }

  function deleteTaskFromContextMenu(): void {
    const task = taskContextMenu ? taskById(taskContextMenu.taskId) : undefined;
    taskContextMenu = null;
    if (task) void deleteTask(task);
  }

  function ensureTaskVisible(task: GanttTask): void {
    const start = taskStartOrdinal(task);
    const end = taskEndOrdinal(task);
    if (end >= axisStartHour && start < axisEndHour) return;
    axisStartHour = clampAxisStart(
      alignAxisHour(start - LOCATE_LEADING_CELLS * unitHours, unitHours),
      unitHours,
    );
    resetTimelineScroll();
  }

  function resetTimelineScroll(): void {
    if (tableScrollElement) tableScrollElement.scrollLeft = 0;
  }

  async function refresh(): Promise<void> {
    try {
      const loaded = await api.list();
      if (savingCount > 0 || dirtyIds.size > 0 || saveFlights.size > 0 || dragState || reorderDragId) {
        externalRefreshPending = true;
        return;
      }
      tasks = loaded;
      await tick();
      scheduleNameFit();
      error = "";
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "无法读取甘特任务。";
    } finally {
      loading = false;
    }
  }

  function scheduleTaskSave(id: string, delay = saveDelayMs): void {
    dirtyIds.add(id);
    const current = saveTimers.get(id);
    if (current) clearTimeout(current);
    saveTimers.set(id, setTimeout(() => void flushTask(id), delay));
  }

  function refreshPendingChanges(): void {
    if (
      !externalRefreshPending
      || savingCount > 0
      || dirtyIds.size > 0
      || saveFlights.size > 0
      || dragState
      || reorderDragId
    ) return;
    externalRefreshPending = false;
    void refresh();
  }

  async function flushTask(id: string): Promise<void> {
    const timer = saveTimers.get(id);
    if (timer) clearTimeout(timer);
    saveTimers.delete(id);

    const activeSave = saveFlights.get(id);
    if (activeSave) {
      await activeSave;
      if (dirtyIds.has(id)) await flushTask(id);
      return;
    }
    if (!dirtyIds.has(id)) return;

    let task = taskById(id);
    if (!task) {
      dirtyIds.delete(id);
      return;
    }
    if (!task.name.trim()) {
      updateTask(id, { name: "新任务" });
      task = taskById(id)!;
    }

    dirtyIds.delete(id);
    savingCount += 1;
    error = "";
    const operation = (async () => {
      try {
        const saved = await api.upsert(taskRequest(task!));
        if (!dirtyIds.has(id) && taskById(id)) updateTask(id, saved);
      } catch (reason) {
        dirtyIds.add(id);
        error = reason instanceof Error ? reason.message : "保存甘特任务失败。";
      } finally {
        savingCount = Math.max(0, savingCount - 1);
      }
    })();
    saveFlights.set(id, operation);
    await operation;
    if (saveFlights.get(id) === operation) saveFlights.delete(id);

    if (dirtyIds.has(id) && !error) scheduleTaskSave(id, 80);
    refreshPendingChanges();
  }

  async function flushAll(): Promise<void> {
    for (const task of tasks) {
      if (!task.name.trim()) {
        updateTask(task.id, { name: "新任务" });
        dirtyIds.add(task.id);
      }
    }
    await Promise.all(tasks.map((task) => flushTask(task.id)));
  }

  function changeName(task: GanttTask, event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    updateTask(task.id, { name: input.value.slice(0, 200) });
    dirtyIds.add(task.id);
    if (input.value.trim()) scheduleTaskSave(task.id);
    else {
      const timer = saveTimers.get(task.id);
      if (timer) clearTimeout(timer);
      saveTimers.delete(task.id);
    }
  }

  function finishNameEdit(task: GanttTask): void {
    const current = taskById(task.id);
    if (!current) return;
    const normalized = current.name.trim() || "新任务";
    if (normalized !== current.name) updateTask(task.id, { name: normalized });
    dirtyIds.add(task.id);
    editingTaskId = null;
    editingOriginalName = null;
    void flushTask(task.id);
    scheduleNameFit();
  }

  function changeProgress(task: GanttTask, event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    const progress = normalizeProgress(Number(input.value));
    if (task.progress < 100 && progress === 100) celebrateCompletion(task.id);
    else if (progress < 100) clearCompletionPulse(task.id);
    updateTask(task.id, { progress });
    scheduleTaskSave(task.id);
  }

  function clearCompletionPulse(id: string): void {
    const timer = completionTimers.get(id);
    if (timer) clearTimeout(timer);
    completionTimers.delete(id);
    completedPulseIds = completedPulseIds.filter((taskId) => taskId !== id);
  }

  function celebrateCompletion(id: string): void {
    clearCompletionPulse(id);
    completedPulseIds = [...completedPulseIds, id];
    completionTimers.set(id, setTimeout(() => clearCompletionPulse(id), 900));
  }

  async function createTask(): Promise<void> {
    if (creating) return;
    creating = true;
    savingCount += 1;
    error = "";
    try {
      const created = await api.upsert({
        id: null,
        name: "新任务",
        progress: 0,
        startDate: today,
        startHour: 0,
        endDate: addDateDays(today, 6),
        endHour: 23,
      });
      if (activeFilter !== "all" && activeFilter !== "not-started") activeFilter = "not-started";
      // The desktop event and the command response can arrive in either order.
      tasks = [...tasks.filter((task) => task.id !== created.id), created];
      ensureTaskVisible(created);
      editingTaskId = created.id;
      editingOriginalName = created.name;
      await tick();
      const input = document.querySelector<HTMLInputElement>(`input[data-task-edit-id="${created.id}"]`);
      input?.focus();
      input?.select();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "新建甘特任务失败。";
    } finally {
      savingCount = Math.max(0, savingCount - 1);
      creating = false;
      refreshPendingChanges();
    }
  }

  async function deleteTask(task: GanttTask): Promise<void> {
    if (!window.confirm(`删除任务“${task.name}”？`)) return;
    const timer = saveTimers.get(task.id);
    if (timer) clearTimeout(timer);
    saveTimers.delete(task.id);
    dirtyIds.delete(task.id);
    error = "";
    savingCount += 1;
    try {
      // Finish an in-flight update before deleting, otherwise a late response
      // could write the task back after the delete succeeds.
      const activeSave = saveFlights.get(task.id);
      if (activeSave) await activeSave;
      dirtyIds.delete(task.id);
      await api.delete(task.id);
      tasks = tasks.filter((item) => item.id !== task.id);
      if (editingTaskId === task.id) editingTaskId = null;
      if (editingTaskId === null) editingOriginalName = null;
      scheduleNameFit();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "删除甘特任务失败。";
    } finally {
      savingCount = Math.max(0, savingCount - 1);
      refreshPendingChanges();
    }
  }

  function beginReorderPointer(event: PointerEvent, task: GanttTask): void {
    if (
      event.button !== 0
      || reordering
      || dragState
      || axisPanState
      || reorderPointerState
    ) return;
    event.preventDefault();
    event.stopPropagation();
    ganttToolElement?.setPointerCapture?.(event.pointerId);
    reorderPointerState = { pointerId: event.pointerId, sourceId: task.id };
    reorderDragId = task.id;
    reorderDropId = null;
  }

  function updateReorderPointer(event: PointerEvent): void {
    const state = reorderPointerState;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    const table = tableScrollElement;
    if (table) {
      const bounds = table.getBoundingClientRect();
      if (event.clientY < bounds.top + 24) table.scrollTop = Math.max(0, table.scrollTop - 12);
      else if (event.clientY > bounds.bottom - 24) table.scrollTop += 12;
    }

    const rows = Array.from(ganttToolElement?.querySelectorAll<HTMLElement>(".task-row") ?? []);
    let targetRow: HTMLElement | null = null;
    let targetBounds: DOMRect | null = null;
    let nearestDistance = Number.POSITIVE_INFINITY;
    for (const row of rows) {
      if (row.dataset.taskId === state.sourceId) continue;
      const cell = row.querySelector<HTMLElement>(".task-cell");
      const bounds = cell?.getBoundingClientRect();
      if (!bounds || bounds.height <= 0) continue;
      const distance = event.clientY < bounds.top
        ? bounds.top - event.clientY
        : event.clientY > bounds.bottom
          ? event.clientY - bounds.bottom
          : 0;
      if (distance < nearestDistance) {
        nearestDistance = distance;
        targetRow = row;
        targetBounds = bounds;
      }
      if (distance === 0) break;
    }
    const targetId = targetRow?.dataset.taskId;
    if (!targetId || !targetBounds) {
      reorderDropId = null;
      return;
    }
    reorderDropId = targetId;
    reorderDropPosition = event.clientY < targetBounds.top + targetBounds.height / 2
      ? "before"
      : "after";
  }

  function finishReorderDrag(cancelled = false): void {
    const pointer = reorderPointerState;
    const sourceId = pointer?.sourceId ?? reorderDragId;
    const targetId = reorderDropId;
    const position = reorderDropPosition;
    if (pointer && ganttToolElement?.hasPointerCapture?.(pointer.pointerId)) {
      ganttToolElement.releasePointerCapture(pointer.pointerId);
    }
    reorderPointerState = null;
    reorderDragId = null;
    reorderDropId = null;
    if (!cancelled && sourceId && targetId) {
      void persistVisibleReorder(sourceId, targetId, position);
    }
  }

  function cancelReorderDrag(): void {
    finishReorderDrag(true);
    refreshPendingChanges();
  }

  async function persistVisibleReorder(
    sourceId: string,
    targetId: string,
    position: DropPosition,
  ): Promise<void> {
    if (sourceId === targetId || reordering) return;
    const currentVisible = tasks.filter((item) => matchesFilter(item, activeFilter));
    const source = currentVisible.find((item) => item.id === sourceId);
    if (!source) return;
    const reorderedVisible = currentVisible.filter((item) => item.id !== sourceId);
    const targetIndex = reorderedVisible.findIndex((item) => item.id === targetId);
    if (targetIndex < 0) return;
    reorderedVisible.splice(targetIndex + (position === "after" ? 1 : 0), 0, source);
    if (reorderedVisible.every((item, index) => item.id === currentVisible[index]?.id)) return;

    const previousOrder = tasks.map((item) => item.id);
    let visibleIndex = 0;
    const reordered = tasks.map((item) => (
      matchesFilter(item, activeFilter) ? reorderedVisible[visibleIndex++] : item
    ));
    tasks = reordered;
    reordering = true;
    savingCount += 1;
    error = "";
    try {
      await api.reorder(reordered.map((item) => item.id));
    } catch (reason) {
      const currentById = new Map(tasks.map((item) => [item.id, item]));
      const previousIds = new Set(previousOrder);
      tasks = [
        ...previousOrder.flatMap((id) => {
          const item = currentById.get(id);
          return item ? [item] : [];
        }),
        ...tasks.filter((item) => !previousIds.has(item.id)),
      ];
      error = reason instanceof Error ? reason.message : "调整任务顺序失败。";
    } finally {
      savingCount = Math.max(0, savingCount - 1);
      reordering = false;
      refreshPendingChanges();
    }
  }

  function moveReorderByKeyboard(event: KeyboardEvent, task: GanttTask): void {
    if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
    const currentVisible = tasks.filter((item) => matchesFilter(item, activeFilter));
    const index = currentVisible.findIndex((item) => item.id === task.id);
    const targetIndex = index + (event.key === "ArrowUp" ? -1 : 1);
    if (index < 0 || targetIndex < 0 || targetIndex >= currentVisible.length) return;
    event.preventDefault();
    const target = currentVisible[targetIndex];
    void persistVisibleReorder(
      task.id,
      target.id,
      event.key === "ArrowUp" ? "before" : "after",
    );
  }

  function setZoomUnit(nextUnit: (typeof UNIT_HOURS)[number], anchorHour?: number, anchorRatio = .5): void {
    if (nextUnit === unitHours) return;
    const anchor = anchorHour ?? axisStartHour + (axisEndHour - axisStartHour) * anchorRatio;
    const beforeLocalPixel = (anchor - axisStartHour) / unitHours * DAY_WIDTH;
    let nextStart = clampAxisStart(
      alignAxisHour(
        anchor - axisCellCount * nextUnit * anchorRatio,
        nextUnit,
        true,
      ),
      nextUnit,
    );
    let afterLocalPixel = (anchor - nextStart) / nextUnit * DAY_WIDTH;
    let scrollDelta = afterLocalPixel - beforeLocalPixel;
    if (
      anchorHour !== undefined
      && tableScrollElement
      && tableScrollElement.scrollLeft + scrollDelta < 0
    ) {
      const earlierStart = clampAxisStart(nextStart - nextUnit, nextUnit);
      if (earlierStart < nextStart) {
        nextStart = earlierStart;
        afterLocalPixel = (anchor - nextStart) / nextUnit * DAY_WIDTH;
        scrollDelta = afterLocalPixel - beforeLocalPixel;
      }
    }
    unitHours = nextUnit;
    axisStartHour = nextStart;
    if (anchorHour !== undefined && tableScrollElement) {
      tableScrollElement.scrollLeft = Math.max(0, tableScrollElement.scrollLeft + scrollDelta);
    }
  }

  function zoomTimeline(direction: "in" | "out", anchorHour?: number, anchorRatio = .5): boolean {
    const index = UNIT_HOURS.indexOf(unitHours);
    const nextIndex = direction === "in" ? Math.min(UNIT_HOURS.length - 1, index + 1) : Math.max(0, index - 1);
    if (nextIndex === index) return false;
    setZoomUnit(UNIT_HOURS[nextIndex], anchorHour, anchorRatio);
    return true;
  }

  function zoomTimelineFromViewport(direction: "in" | "out"): void {
    if (!axisHeaderElement || !tableScrollElement) {
      zoomTimeline(direction);
      return;
    }
    const axisBounds = axisHeaderElement.getBoundingClientRect();
    const scrollBounds = tableScrollElement.getBoundingClientRect();
    const stickyRight = progressHeaderElement?.getBoundingClientRect().right ?? scrollBounds.left;
    const visibleLeft = Math.max(axisBounds.left, scrollBounds.left, stickyRight);
    const visibleRight = Math.min(axisBounds.right, scrollBounds.right);
    if (axisBounds.width <= 0 || visibleRight <= visibleLeft) {
      zoomTimeline(direction);
      return;
    }
    const visibleCenter = (visibleLeft + visibleRight) / 2;
    const ratio = Math.max(0, Math.min(1, (visibleCenter - axisBounds.left) / axisBounds.width));
    const anchorHour = axisStartHour + ratio * (axisEndHour - axisStartHour);
    zoomTimeline(direction, anchorHour, ratio);
  }

  function handleTimelineWheel(event: WheelEvent): void {
    if (event.deltaY === 0) return;
    event.preventDefault();
    if (axisPanState) return;
    if (wheelUnlockTimer) clearTimeout(wheelUnlockTimer);
    wheelUnlockTimer = setTimeout(() => {
      wheelGestureLocked = false;
      wheelUnlockTimer = undefined;
    }, WHEEL_GESTURE_END_MS);
    if (wheelGestureLocked) return;
    wheelGestureLocked = true;
    const target = event.currentTarget as HTMLElement;
    const bounds = target.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (event.clientX - bounds.left) / Math.max(bounds.width, 1)));
    const anchorHour = axisStartHour + ratio * (axisEndHour - axisStartHour);
    zoomTimeline(event.deltaY < 0 ? "in" : "out", anchorHour, ratio);
  }

  function panTimeline(direction: -1 | 1): void {
    const delta = direction * unitHours * PAN_CELLS;
    axisStartHour = clampAxisStart(axisStartHour + delta, unitHours);
  }

  function beginAxisPan(event: PointerEvent): void {
    if (
      event.button !== 0
      || dragState
      || axisPanState
      || reorderDragId
      || reordering
    ) return;
    const target = event.target;
    if (target instanceof Element && target.closest(".task-range")) return;
    event.preventDefault();
    ganttToolElement?.setPointerCapture?.(event.pointerId);
    axisPanState = {
      pointerId: event.pointerId,
      originX: event.clientX,
      initialAxisStartHour: axisStartHour,
      initialScrollLeft: tableScrollElement?.scrollLeft ?? 0,
      unitHours,
      changed: false,
    };
  }

  function moveAxisPan(event: PointerEvent): void {
    const state = axisPanState;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    const table = tableScrollElement;
    const maxScroll = table ? Math.max(0, table.scrollWidth - table.clientWidth) : 0;
    const requestedScroll = state.initialScrollLeft + state.originX - event.clientX;
    let nextStart = state.initialAxisStartHour;
    let nextScroll = requestedScroll;

    if (nextScroll < 0) {
      const requestedCells = Math.ceil(-nextScroll / DAY_WIDTH);
      nextStart = clampAxisStart(
        state.initialAxisStartHour - requestedCells * state.unitHours,
        state.unitHours,
      );
      const appliedCells = (state.initialAxisStartHour - nextStart) / state.unitHours;
      nextScroll += appliedCells * DAY_WIDTH;
    } else if (nextScroll > maxScroll) {
      const requestedCells = Math.ceil((nextScroll - maxScroll) / DAY_WIDTH);
      nextStart = clampAxisStart(
        state.initialAxisStartHour + requestedCells * state.unitHours,
        state.unitHours,
      );
      const appliedCells = (nextStart - state.initialAxisStartHour) / state.unitHours;
      nextScroll -= appliedCells * DAY_WIDTH;
    }

    nextScroll = Math.max(0, Math.min(maxScroll, nextScroll));
    axisStartHour = nextStart;
    if (table) table.scrollLeft = nextScroll;
    axisPanState = {
      ...state,
      changed: nextStart !== state.initialAxisStartHour
        || Math.abs(nextScroll - state.initialScrollLeft) >= .5,
    };
  }

  function finishAxisPan(event: PointerEvent, cancelled = false): void {
    const state = axisPanState;
    if (!state || state.pointerId !== event.pointerId) return;
    if (ganttToolElement?.hasPointerCapture?.(event.pointerId)) {
      ganttToolElement.releasePointerCapture(event.pointerId);
    }
    axisPanState = null;
    if (!cancelled) return;
    axisStartHour = state.initialAxisStartHour;
    if (tableScrollElement) tableScrollElement.scrollLeft = state.initialScrollLeft;
  }

  function moveActivePointer(event: PointerEvent): void {
    if (dragState) moveTimelineDrag(event);
    else if (axisPanState) moveAxisPan(event);
    else if (reorderPointerState) updateReorderPointer(event);
  }

  function finishActivePointer(event: PointerEvent, cancelled = false): void {
    if (dragState) finishTimelineDrag(event, cancelled);
    else if (axisPanState) finishAxisPan(event, cancelled);
    else if (reorderPointerState) finishReorderDrag(cancelled);
  }

  function resetDayTimeline(): void {
    unitHours = 24;
    axisStartHour = defaultAxisStartHour(today);
    resetTimelineScroll();
  }

  function focusEarliestTask(): void {
    if (tasks.length === 0) {
      resetDayTimeline();
      return;
    }
    const earliest = Math.min(...tasks.map(taskStartOrdinal));
    axisStartHour = clampAxisStart(
      alignAxisHour(earliest - LOCATE_LEADING_CELLS * unitHours, unitHours),
      unitHours,
    );
    resetTimelineScroll();
  }

  function dragDates(state: TimelineDrag, clientX: number): { start: number; end: number } {
    const requestedDelta = Math.round((clientX - state.originX) / DAY_WIDTH) * unitHours;
    if (state.kind === "start") {
      return {
        start: Math.max(
          MIN_TIMELINE_HOUR,
          Math.min(state.initialEndHour, state.initialStartHour + requestedDelta),
        ),
        end: state.initialEndHour,
      };
    }
    if (state.kind === "end") {
      return {
        start: state.initialStartHour,
        end: Math.min(
          MAX_TIMELINE_HOUR,
          Math.max(state.initialStartHour, state.initialEndHour + requestedDelta),
        ),
      };
    }
    const boundedDelta = Math.max(
      MIN_TIMELINE_HOUR - state.initialStartHour,
      Math.min(MAX_TIMELINE_HOUR - state.initialEndHour, requestedDelta),
    );
    return {
      start: state.initialStartHour + boundedDelta,
      end: state.initialEndHour + boundedDelta,
    };
  }

  function beginTimelineDrag(event: PointerEvent, task: GanttTask, kind: DragKind): void {
    if (event.button !== 0 || axisPanState || reorderDragId) return;
    event.preventDefault();
    event.stopPropagation();
    ganttToolElement?.setPointerCapture?.(event.pointerId);
    dragState = {
      taskId: task.id,
      kind,
      pointerId: event.pointerId,
      originX: event.clientX,
      initialStartHour: taskStartOrdinal(task),
      initialEndHour: taskEndOrdinal(task),
      changed: false,
    };
  }

  function moveTimelineDrag(event: PointerEvent): void {
    const state = dragState;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    const dates = dragDates(state, event.clientX);
    const changed = dates.start !== state.initialStartHour || dates.end !== state.initialEndHour;
    dragState = { ...state, changed };
    updateTask(state.taskId, rangePatch(dates.start, dates.end));
  }

  function finishTimelineDrag(event: PointerEvent, cancelled = false): void {
    const state = dragState;
    if (!state || state.pointerId !== event.pointerId) return;
    if (ganttToolElement?.hasPointerCapture?.(event.pointerId)) {
      ganttToolElement.releasePointerCapture(event.pointerId);
    }
    dragState = null;
    if (cancelled) {
      updateTask(state.taskId, rangePatch(state.initialStartHour, state.initialEndHour));
      return;
    }
    if (state.changed) scheduleTaskSave(state.taskId, 120);
  }

  function moveByKeyboard(event: KeyboardEvent, task: GanttTask, kind: DragKind): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const current = taskById(task.id);
    if (!current) return;
    let start = taskStartOrdinal(current);
    let end = taskEndOrdinal(current);

    const amount = (event.shiftKey ? 7 : 1) * unitHours;
    const adjustedDelta = event.key === "ArrowLeft" ? -amount : amount;
    if (kind === "start") start = Math.max(MIN_TIMELINE_HOUR, Math.min(end, start + adjustedDelta));
    else if (kind === "end") end = Math.min(MAX_TIMELINE_HOUR, Math.max(start, end + adjustedDelta));
    else {
      const boundedDelta = Math.max(
        MIN_TIMELINE_HOUR - start,
        Math.min(MAX_TIMELINE_HOUR - end, adjustedDelta),
      );
      start += boundedDelta;
      end += boundedDelta;
    }

    if (start === taskStartOrdinal(current) && end === taskEndOrdinal(current)) return;
    updateTask(task.id, rangePatch(start, end));
    scheduleTaskSave(task.id);
  }

  function taskRangeLayout(task: GanttTask): TaskRangeLayout {
    const start = taskStartOrdinal(task);
    const endExclusive = taskEndOrdinal(task) + 1;
    const visibleStart = Math.max(start, axisStartHour);
    const visibleEnd = Math.min(endExclusive, axisEndHour);
    if (visibleEnd <= visibleStart) {
      return { visible: false, left: 0, width: 0, startClipped: false, endClipped: false };
    }
    return {
      visible: true,
      left: (visibleStart - axisStartHour) / unitHours * DAY_WIDTH + 2,
      width: Math.max(4, (visibleEnd - visibleStart) / unitHours * DAY_WIDTH - 4),
      startClipped: start < axisStartHour,
      endClipped: endExclusive > axisEndHour,
    };
  }

  function rangeLabel(task: GanttTask): string {
    const dayOnly = unitHours >= HOURS_PER_DAY && task.startHour === 0 && task.endHour === 23;
    if (dayOnly) return `${task.startDate.slice(5).replace("-", "/")} - ${task.endDate.slice(5).replace("-", "/")}`;
    const format = (ordinal: number): string => {
      const point = ordinalToDateHour(ordinal);
      return `${point.dateKey.slice(5).replace("-", "/")} ${String(point.hour).padStart(2, "0")}:00`;
    };
    return `${format(taskStartOrdinal(task))} - ${format(taskEndOrdinal(task))}`;
  }

  function formatHourOrdinal(ordinal: number): string {
    const point = ordinalToDateHour(ordinal);
    return `${point.dateKey} ${String(point.hour).padStart(2, "0")}:00`;
  }

  function formatTaskPoint(task: GanttTask, side: "start" | "end"): string {
    if (unitHours >= HOURS_PER_DAY && task.startHour === 0 && task.endHour === 23) {
      return side === "start" ? task.startDate : task.endDate;
    }
    return formatHourOrdinal(side === "start" ? taskStartOrdinal(task) : taskEndOrdinal(task));
  }

  async function closeWindow(): Promise<void> {
    await flushAll();
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

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const updateCompactFilterLayout = () => {
      compactFilterLayout = window.innerWidth <= 900;
      taskContextMenu = null;
      scheduleNameFit();
    };
    const closeTaskContextMenu = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".task-context-menu")) return;
      taskContextMenu = null;
    };
    const handleDocumentKeydown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (editingTaskId) cancelNameEdit();
      else taskContextMenu = null;
    };
    updateCompactFilterLayout();
    window.addEventListener("resize", updateCompactFilterLayout);
    document.addEventListener("pointerdown", closeTaskContextMenu, true);
    document.addEventListener("keydown", handleDocumentKeydown);
    void refresh();

    const handleChanged = () => {
      if (savingCount > 0 || dirtyIds.size > 0 || saveFlights.size > 0 || dragState || reorderDragId) {
        externalRefreshPending = true;
      }
      else void refresh();
    };
    window.addEventListener("petaldesk:gantt-changed", handleChanged);

    if (api.isDesktop()) {
      void import("@tauri-apps/api/event").then(async ({ listen }) => {
        const cleanup = await listen("gantt_changed", handleChanged);
        if (disposed) cleanup();
        else unlisten = cleanup;
      }).catch(() => undefined);
    }

    const handleBeforeUnload = () => void flushAll();
    window.addEventListener("beforeunload", handleBeforeUnload);
    const removeUpdatePreparation = addUpdateInstallPreparation(async () => {
      await flushAll();
      if (error) throw new Error(error);
    });
    return () => {
      disposed = true;
      removeUpdatePreparation();
      unlisten?.();
      window.removeEventListener("petaldesk:gantt-changed", handleChanged);
      window.removeEventListener("beforeunload", handleBeforeUnload);
      window.removeEventListener("resize", updateCompactFilterLayout);
      document.removeEventListener("pointerdown", closeTaskContextMenu, true);
      document.removeEventListener("keydown", handleDocumentKeydown);
      for (const timer of saveTimers.values()) clearTimeout(timer);
      saveTimers.clear();
      for (const timer of completionTimers.values()) clearTimeout(timer);
      completionTimers.clear();
      if (wheelUnlockTimer) clearTimeout(wheelUnlockTimer);
      if (nameFitFrame !== undefined) cancelAnimationFrame(nameFitFrame);
    };
  });
</script>

{#snippet statusFilter()}
  <div class="status-filter" role="group" aria-label="按进度筛选">
    {#each filterOptions as option (option.value)}
      <button
        type="button"
        class:active={activeFilter === option.value}
        aria-pressed={activeFilter === option.value}
        onclick={() => {
          activeFilter = option.value;
          cancelReorderDrag();
          scheduleNameFit();
        }}
      >
        <span>{option.label}</span>
        <small>{statusCounts[option.value]}</small>
      </button>
    {/each}
  </div>
{/snippet}

<section
  class:axis-panning={axisPanState !== null}
  class="gantt-tool"
  data-testid="gantt-tool"
  aria-label="任务甘特图"
  bind:this={ganttToolElement}
  onpointermove={moveActivePointer}
  onpointerup={finishActivePointer}
  onpointercancel={(event) => finishActivePointer(event, true)}
>
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <ChartGantt size={18} aria-hidden="true" />
      <h1 data-tauri-drag-region>任务甘特图</h1>
      {#if savingCount > 0}
        <LoaderCircle class="spinner saving-indicator" size={14} aria-label="正在保存" />
      {/if}
    </div>
    {#if !loading && tasks.length > 0 && !compactFilterLayout}
      <div class="titlebar-filter" data-tauri-drag-region>
        {@render statusFilter()}
      </div>
    {/if}
    <div class="window-actions">
      <div class="timeline-actions" role="group" aria-label={`时间轴控制，当前每格${granularityLabel(unitHours)}`}>
        <button type="button" aria-label="时间轴向左移动" title="向左移动时间轴" onclick={() => panTimeline(-1)}>
          <ChevronLeft size={17} aria-hidden="true" />
        </button>
        <button type="button" aria-label="时间轴向右移动" title="向右移动时间轴" onclick={() => panTimeline(1)}>
          <ChevronRight size={17} aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="缩小时间轴"
          title="缩小时间轴"
          disabled={unitHours === UNIT_HOURS[0]}
          onclick={() => zoomTimelineFromViewport("out")}
        >
          <ZoomOut size={16} aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="放大时间轴"
          title="放大时间轴"
          disabled={unitHours === UNIT_HOURS[UNIT_HOURS.length - 1]}
          onclick={() => zoomTimelineFromViewport("in")}
        >
          <ZoomIn size={16} aria-hidden="true" />
        </button>
        <button type="button" aria-label="重置为天级时间轴" title="重置为天级" onclick={resetDayTimeline}>
          <CalendarDays size={16} aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="定位最早任务"
          title="定位最早任务"
          disabled={tasks.length === 0}
          onclick={focusEarliestTask}
        >
          <LocateFixed size={16} aria-hidden="true" />
        </button>
      </div>
      <button
        type="button"
        class="primary-icon"
        aria-label="新建任务"
        title="新建任务"
        disabled={creating}
        onclick={() => void createTask()}
      >
        <Plus size={18} aria-hidden="true" />
      </button>
      <button type="button" aria-label="关闭甘特图窗口" title="关闭" onclick={() => void closeWindow()}>
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  </header>

  <main>
    {#if error}
      <div class="error-banner" role="alert">{error}</div>
    {/if}

    {#if loading}
      <div class="empty-state" aria-busy="true">
        <LoaderCircle class="spinner" size={22} aria-hidden="true" />
        <span>正在读取任务…</span>
      </div>
    {:else if tasks.length === 0}
      <div class="empty-state">
        <ChartGantt size={30} aria-hidden="true" />
        <strong>还没有任务</strong>
        <button type="button" class="primary-button" onclick={() => void createTask()}>
          <Plus size={16} aria-hidden="true" /> 新建任务
        </button>
      </div>
    {:else}
      {#if compactFilterLayout}
        <div class="gantt-toolbar">
          {@render statusFilter()}
        </div>
      {/if}
      <div class="table-scroll" aria-label="甘特任务表格" bind:this={tableScrollElement}>
        <div
          class="gantt-grid"
          role="table"
          aria-colcount="3"
          aria-rowcount={visibleTasks.length + 1}
          style={`--timeline-width: ${timelineWidth}px;`}
        >
          <div class="column-header task-column" role="columnheader">任务名称</div>
          <div class="column-header progress-column" role="columnheader" bind:this={progressHeaderElement}>进度</div>
          <div
            class="axis-header"
            role="columnheader"
            tabindex="-1"
            aria-label="时间轴"
            title={`拖动平移，滚轮缩放；当前每格${granularityLabel(unitHours)}`}
            data-axis-start-hour={axisStartHour}
            data-axis-end-hour={axisEndHour}
            data-axis-cell-count={axisCellCount}
            data-unit-hours={unitHours}
            bind:this={axisHeaderElement}
            onpointerdown={beginAxisPan}
            onwheel={handleTimelineWheel}
          >
            {#each axisDays as day (day.ordinal)}
              <div
                class:weekend={day.weekend}
                class:today={day.today}
                class:hour-level={unitHours < HOURS_PER_DAY}
                class="axis-day"
                title={unitHours < HOURS_PER_DAY ? formatHourOrdinal(day.startHour) : day.dateKey}
              >
                <span>{day.boundaryLabel}</span>
                <strong>{day.primaryLabel}</strong>
                {#if day.secondaryLabel}
                  <small>{day.secondaryLabel}</small>
                {/if}
              </div>
            {/each}
          </div>

          {#each visibleTasks as task, index (task.id)}
            {@const status = taskStatus(task)}
            {@const range = taskRangeLayout(task)}
            {@const fit = taskNameFit(task.id)}
            <div
              class:dragging-row={reorderDragId === task.id}
              class:drop-before={reorderDropId === task.id && reorderDropPosition === "before"}
              class:drop-after={reorderDropId === task.id && reorderDropPosition === "after"}
              class="task-row"
              role="row"
              tabindex="-1"
              data-task-id={task.id}
            >
            <div
              class:alternate={index % 2 === 1}
              class:status-not-started={status === "not-started"}
              class:status-in-progress={status === "in-progress"}
              class:status-completed={status === "completed"}
              class:celebrating={completedPulseIds.includes(task.id)}
              class="task-cell task-column"
              role="cell"
              tabindex="-1"
              style={`--task-progress: ${task.progress}%;`}
              oncontextmenu={(event) => openTaskContextMenu(event, task)}
            >
              <button
                type="button"
                class="reorder-handle"
                aria-label={`调整任务“${task.name}”的顺序`}
                title="拖动调整顺序"
                onpointerdown={(event) => beginReorderPointer(event, task)}
                onkeydown={(event) => moveReorderByKeyboard(event, task)}
              >
                <GripVertical size={16} aria-hidden="true" />
              </button>
              {#if editingTaskId === task.id}
                <input
                  class="task-name-input"
                  data-task-edit-id={task.id}
                  value={task.name}
                  maxlength="200"
                  aria-label={`编辑任务名称“${task.name}”`}
                  oninput={(event) => changeName(task, event)}
                  onkeydown={(event) => handleNameEditKeydown(event, task)}
                  onblur={() => finishNameEdit(task)}
                />
              {:else}
                <span
                  class:two-lines={fit.twoLines}
                  class:name-truncated={fit.truncated}
                  class="task-name-display"
                  data-name-fit-id={task.id}
                  style={`--name-font-size: ${fit.fontSize}px;`}
                  title={fit.truncated ? task.name : ""}
                >{task.name}</span>
              {/if}
              {#if status === "completed"}
                <span class="completion-badge" aria-label="已完成">
                  <CheckCircle2 size={12} aria-hidden="true" />
                  完成
                </span>
              {/if}
            </div>

            <div
              class:alternate={index % 2 === 1}
              class:status-not-started={status === "not-started"}
              class:status-in-progress={status === "in-progress"}
              class:status-completed={status === "completed"}
              class="progress-cell progress-column"
              role="cell"
            >
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                value={task.progress}
                aria-label={`任务进度“${task.name}”`}
                aria-valuetext={`${task.progress}%`}
                oninput={(event) => changeProgress(task, event)}
                onblur={() => void flushTask(task.id)}
              />
            </div>

            <div
              class:alternate={index % 2 === 1}
              class:status-not-started={status === "not-started"}
              class:status-in-progress={status === "in-progress"}
              class:status-completed={status === "completed"}
              class="timeline-cell"
              role="cell"
              tabindex="-1"
              aria-label={`${task.name}时间轴`}
              onpointerdown={beginAxisPan}
              onwheel={handleTimelineWheel}
            >
              {#if dateHourToOrdinal(today, 12) >= axisStartHour && dateHourToOrdinal(today, 12) < axisEndHour}
                <span
                  class="today-line"
                  style={`left: ${(dateHourToOrdinal(today, 12) - axisStartHour) / unitHours * DAY_WIDTH}px`}
                  aria-hidden="true"
                ></span>
              {/if}
              {#if range.visible}
                <div
                  class:dragging={dragState?.taskId === task.id}
                  class:status-not-started={status === "not-started"}
                  class:status-in-progress={status === "in-progress"}
                  class:status-completed={status === "completed"}
                  class:start-clipped={range.startClipped}
                  class:end-clipped={range.endClipped}
                  class="task-range"
                  style={`left: ${range.left}px; width: ${range.width}px;`}
                >
                  {#if !range.startClipped}
                    <button
                      type="button"
                      class="range-handle start-handle"
                      role="slider"
                      aria-label={`调整“${task.name}”的开始日期`}
                      aria-valuemin={axisStartHour}
                      aria-valuemax={taskEndOrdinal(task)}
                      aria-valuenow={taskStartOrdinal(task)}
                      aria-valuetext={formatTaskPoint(task, "start")}
                      title={`开始：${formatTaskPoint(task, "start")}`}
                      onpointerdown={(event) => beginTimelineDrag(event, task, "start")}
                      onkeydown={(event) => moveByKeyboard(event, task, "start")}
                      onblur={() => void flushTask(task.id)}
                    ></button>
                  {/if}
                  <button
                    type="button"
                    class="range-body"
                    role="slider"
                    aria-label={`平移“${task.name}”的日期范围`}
                    aria-valuemin={axisStartHour}
                    aria-valuemax={axisEndHour - 1}
                    aria-valuenow={taskStartOrdinal(task)}
                    aria-valuetext={`${formatTaskPoint(task, "start")} 至 ${formatTaskPoint(task, "end")}`}
                    title={`${formatTaskPoint(task, "start")} 至 ${formatTaskPoint(task, "end")}`}
                    onpointerdown={(event) => beginTimelineDrag(event, task, "move")}
                    onkeydown={(event) => moveByKeyboard(event, task, "move")}
                    onblur={() => void flushTask(task.id)}
                  >
                    <span>{rangeLabel(task)}</span>
                  </button>
                  {#if !range.endClipped}
                    <button
                      type="button"
                      class="range-handle end-handle"
                      role="slider"
                      aria-label={`调整“${task.name}”的结束日期`}
                      aria-valuemin={taskStartOrdinal(task)}
                      aria-valuemax={axisEndHour - 1}
                      aria-valuenow={taskEndOrdinal(task)}
                      aria-valuetext={formatTaskPoint(task, "end")}
                      title={`结束：${formatTaskPoint(task, "end")}`}
                      onpointerdown={(event) => beginTimelineDrag(event, task, "end")}
                      onkeydown={(event) => moveByKeyboard(event, task, "end")}
                      onblur={() => void flushTask(task.id)}
                    ></button>
                  {/if}
                </div>
              {/if}
            </div>
            </div>
          {/each}

          {#if visibleTasks.length === 0}
            <div class="no-filter-results" role="row">
              <div role="cell">当前筛选下没有任务</div>
            </div>
          {/if}
        </div>
      </div>
    {/if}
  </main>
  {#if taskContextMenu}
    {@const menu = taskContextMenu}
    <div
      class="task-context-menu"
      role="menu"
      tabindex="-1"
      aria-label="任务操作"
      style={`left: ${menu.x}px; top: ${menu.y}px;`}
      oncontextmenu={(event) => event.preventDefault()}
    >
      <button type="button" role="menuitem" onclick={() => void beginNameEdit(menu.taskId)}>
        <Pencil size={15} aria-hidden="true" />
        编辑
      </button>
      <button type="button" class="danger" role="menuitem" onclick={deleteTaskFromContextMenu}>
        <Trash2 size={15} aria-hidden="true" />
        删除
      </button>
    </div>
  {/if}
</section>

<style>
  .gantt-tool {
    display: grid;
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    grid-template-rows: 44px minmax(0, 1fr);
    color: #202020;
    background: #f3f3f3;
  }

  .titlebar {
    display: flex;
    min-width: 0;
    align-items: center;
    justify-content: space-between;
    padding: 5px 6px 5px 12px;
    background: rgb(250 250 250 / 96%);
    border-bottom: 1px solid #d7d7d7;
    user-select: none;
  }

  .brand,
  .window-actions,
  .primary-button {
    display: flex;
    align-items: center;
  }

  .brand {
    min-width: 0;
    flex: 0 1 auto;
    gap: 8px;
    color: #5b4b89;
  }

  .titlebar-filter {
    display: flex;
    min-width: 0;
    margin-right: auto;
    margin-left: 24px;
    align-items: center;
  }

  h1 {
    margin: 0;
    color: #252525;
    font-size: 14px;
    font-weight: 650;
  }

  :global(.saving-indicator) {
    color: #777;
  }

  .window-actions {
    flex: 0 0 auto;
    gap: 2px;
  }

  .timeline-actions {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: 5px;
    margin-right: 3px;
    border-right: 1px solid #dedede;
  }

  button,
  input {
    font: inherit;
  }

  .window-actions button {
    display: grid;
    width: 32px;
    height: 32px;
    padding: 0;
    place-items: center;
    color: #555;
    background: transparent;
    border: 0;
    border-radius: 5px;
    cursor: pointer;
  }

  .window-actions button:hover,
  .window-actions button:focus-visible {
    color: #202020;
    background: #e7e7e7;
  }

  .window-actions button:disabled {
    cursor: default;
    opacity: .5;
  }

  .window-actions .primary-icon {
    color: #5c3fa3;
  }

  .window-actions button:last-child:hover,
  .window-actions button:last-child:focus-visible {
    color: #fff;
    background: #c42b1c;
  }

  main {
    display: flex;
    min-width: 0;
    min-height: 0;
    padding: 14px;
    flex-direction: column;
    overflow: hidden;
  }

  .error-banner {
    flex: 0 0 auto;
    margin-bottom: 10px;
    padding: 8px 10px;
    color: #8c1d14;
    font-size: 12.5px;
    line-height: 1.4;
    background: #fff0ed;
    border: 1px solid #f1c1bb;
    border-radius: 6px;
  }

  .gantt-toolbar {
    display: none;
    flex: 0 0 auto;
    align-items: center;
    margin-bottom: 8px;
  }

  .status-filter {
    display: inline-flex;
    min-width: 0;
    padding: 2px;
    gap: 2px;
    background: #e9e9e9;
    border: 1px solid #d3d3d3;
    border-radius: 6px;
  }

  .status-filter button {
    display: flex;
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    align-items: center;
    justify-content: center;
    gap: 5px;
    color: #585858;
    font-size: 11.5px;
    background: transparent;
    border: 0;
    border-radius: 4px;
    cursor: pointer;
  }

  .status-filter button:hover,
  .status-filter button:focus-visible {
    color: #262626;
    background: rgb(255 255 255 / 64%);
    outline: 0;
  }

  .status-filter button:focus-visible {
    box-shadow: 0 0 0 2px rgb(95 65 160 / 32%);
  }

  .status-filter button.active {
    color: #342653;
    font-weight: 650;
    background: #fff;
    box-shadow: 0 1px 2px rgb(0 0 0 / 10%);
  }

  .status-filter small {
    min-width: 16px;
    padding: 1px 4px;
    color: #696969;
    font-size: 9.5px;
    font-variant-numeric: tabular-nums;
    line-height: 14px;
    text-align: center;
    background: rgb(0 0 0 / 6%);
    border-radius: 7px;
  }

  .status-filter button.active small {
    color: #fff;
    background: #66509a;
  }

  .table-scroll {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    overflow: auto;
    background: #fff;
    border: 1px solid #d5d5d5;
    border-radius: 6px;
    scrollbar-color: #b7b7b7 transparent;
    scrollbar-width: thin;
  }

  .gantt-grid {
    --task-column-width: 330px;
    --progress-column-width: 96px;
    --grid-width: calc(var(--task-column-width) + var(--progress-column-width) + var(--timeline-width));

    display: grid;
    width: max(100%, var(--grid-width));
    min-width: var(--grid-width);
    grid-template-columns: var(--task-column-width) var(--progress-column-width) var(--timeline-width);
    grid-template-rows: 50px;
    grid-auto-rows: 46px;
  }

  .task-row {
    display: contents;
  }

  .column-header,
  .axis-header {
    position: sticky;
    top: 0;
    z-index: 4;
    min-width: 0;
    background: #fafafa;
    border-bottom: 1px solid #d6d6d6;
  }

  .column-header {
    display: flex;
    align-items: center;
    padding: 0 12px;
    color: #5c5c5c;
    font-size: 11.5px;
    font-weight: 650;
  }

  .task-column {
    position: sticky;
    left: 0;
    z-index: 3;
    border-right: 1px solid #dfdfdf;
  }

  .progress-column {
    position: sticky;
    left: var(--task-column-width);
    z-index: 3;
    border-right: 1px solid #d4d4d4;
  }

  .column-header.task-column,
  .column-header.progress-column {
    z-index: 6;
  }

  .axis-header {
    display: flex;
    width: var(--timeline-width);
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }

  .axis-day {
    position: relative;
    display: grid;
    flex: 0 0 28px;
    height: 50px;
    padding: 4px 0 3px;
    align-content: center;
    place-items: center;
    color: #585858;
    border-right: 1px solid #e4e4e4;
    overflow: hidden;
  }

  .axis-day span {
    position: absolute;
    top: 2px;
    left: 3px;
    color: #7656ad;
    font-size: 8.5px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .axis-day strong {
    margin-top: 8px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    font-weight: 650;
  }

  .axis-day small {
    color: #929292;
    font-size: 9px;
  }

  .axis-day.hour-level {
    padding-top: 8px;
  }

  .axis-day.hour-level strong {
    margin-top: 0;
    color: #4f4f4f;
    font-size: 10px;
  }

  .axis-day.weekend {
    background: #f4f4f4;
  }

  .axis-day.today {
    color: #5a379a;
    background: #f0eafb;
  }

  .task-cell,
  .progress-cell,
  .timeline-cell {
    min-width: 0;
    background-color: #fff;
    border-bottom: 1px solid #e7e7e7;
  }

  .task-cell.alternate,
  .progress-cell.alternate,
  .timeline-cell.alternate {
    background-color: #fcfcfc;
  }

  .task-cell {
    position: sticky;
    display: flex;
    overflow: hidden;
    align-items: center;
    gap: 2px;
    padding: 4px 5px 4px 4px;
    isolation: isolate;
    box-shadow: inset 3px 0 #a4a7ac;
  }

  .task-cell::before {
    position: absolute;
    z-index: 0;
    inset: 0 auto 0 0;
    width: var(--task-progress);
    content: "";
    background: #e2f3f1;
    transition: width 180ms ease, background-color 180ms ease;
  }

  .task-cell > * {
    position: relative;
    z-index: 1;
  }

  .task-cell.status-not-started::before {
    background: #f0f1f2;
  }

  .task-cell.status-in-progress {
    box-shadow: inset 3px 0 #228c85;
  }

  .task-cell.status-completed {
    background-color: #e7f4eb;
    box-shadow: inset 3px 0 #2f8958;
  }

  .task-cell.status-completed::before {
    background: #d9efdf;
  }

  .task-row.dragging-row > .task-cell,
  .task-row.dragging-row > .progress-cell,
  .task-row.dragging-row > .timeline-cell {
    opacity: .55;
  }

  .task-row.drop-before > .task-cell::after,
  .task-row.drop-before > .progress-cell::after,
  .task-row.drop-before > .timeline-cell::after,
  .task-row.drop-after > .task-cell::after,
  .task-row.drop-after > .progress-cell::after,
  .task-row.drop-after > .timeline-cell::after {
    position: absolute;
    z-index: 8;
    right: 0;
    left: 0;
    height: 2px;
    content: "";
    background: #197a73;
    pointer-events: none;
  }

  .task-row.drop-before > .task-cell::after,
  .task-row.drop-before > .progress-cell::after,
  .task-row.drop-before > .timeline-cell::after {
    top: 0;
  }

  .task-row.drop-after > .task-cell::after,
  .task-row.drop-after > .progress-cell::after,
  .task-row.drop-after > .timeline-cell::after {
    bottom: 0;
  }

  .reorder-handle {
    display: grid;
    width: 22px;
    height: 28px;
    padding: 0;
    flex: 0 0 22px;
    place-items: center;
    color: #8a8a8a;
    background: transparent;
    border: 0;
    border-radius: 4px;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }

  .reorder-handle:hover,
  .reorder-handle:focus-visible {
    color: #385d59;
    background: rgb(255 255 255 / 72%);
    outline: 0;
  }

  .reorder-handle:focus-visible {
    box-shadow: 0 0 0 2px rgb(34 140 133 / 28%);
  }

  .reorder-handle:active {
    cursor: grabbing;
  }

  .task-name-input {
    min-width: 0;
    height: 30px;
    padding: 0 5px;
    flex: 1 1 auto;
    color: #252525;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    outline: 0;
    text-overflow: ellipsis;
  }

  .task-name-display {
    display: block;
    min-width: 0;
    max-height: 32px;
    flex: 1 1 auto;
    overflow: hidden;
    color: #252525;
    font-size: var(--name-font-size, 13px);
    font-weight: 550;
    letter-spacing: 0;
    line-height: 16px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-name-display.two-lines {
    display: -webkit-box;
    white-space: normal;
    overflow-wrap: anywhere;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
  }

  .task-name-display.name-truncated {
    cursor: help;
  }

  .task-name-input:focus {
    background: #fff;
    border-color: #7352b9;
    box-shadow: inset 0 -2px #7352b9;
  }

  .task-cell.status-completed .task-name-display,
  .task-cell.status-completed .task-name-input {
    color: #1f593c;
    font-weight: 700;
  }

  .task-cell.status-in-progress .task-name-display,
  .task-cell.status-in-progress .task-name-input {
    color: #216d69;
    font-weight: 600;
  }

  .completion-badge {
    display: inline-flex;
    height: 20px;
    padding: 0 5px;
    flex: 0 0 auto;
    align-items: center;
    gap: 3px;
    color: #236a45;
    font-size: 9.5px;
    font-weight: 700;
    white-space: nowrap;
    background: rgb(255 255 255 / 70%);
    border: 1px solid #99cdb0;
    border-radius: 7px;
  }

  .task-cell.celebrating {
    animation: completion-flash 850ms ease-out;
  }

  .task-cell.celebrating .completion-badge {
    animation: completion-pop 650ms cubic-bezier(.2, .9, .3, 1.35);
  }

  .progress-cell {
    position: sticky;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 9px;
  }

  .task-context-menu {
    position: fixed;
    z-index: 30;
    display: grid;
    width: 132px;
    padding: 4px;
    gap: 2px;
    background: #fff;
    border: 1px solid #d2d2d2;
    border-radius: 6px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 18%);
  }

  .task-context-menu button {
    display: flex;
    width: 100%;
    height: 32px;
    padding: 0 9px;
    align-items: center;
    gap: 8px;
    color: #303030;
    background: transparent;
    border: 0;
    border-radius: 4px;
    cursor: pointer;
  }

  .task-context-menu button:hover,
  .task-context-menu button:focus-visible {
    background: #f0f0f0;
    outline: 0;
  }

  .task-context-menu button.danger {
    color: #b42318;
  }

  .task-context-menu button.danger:hover,
  .task-context-menu button.danger:focus-visible {
    background: #fff0ed;
  }

  .progress-cell input {
    width: 100%;
    min-width: 0;
    margin: 0;
    accent-color: #8b8f95;
    cursor: pointer;
  }

  .progress-cell.status-in-progress input {
    accent-color: #228c85;
  }

  .progress-cell.status-completed input {
    accent-color: #2f8958;
  }

  .timeline-cell {
    position: relative;
    width: var(--timeline-width);
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    background-image: repeating-linear-gradient(
      to right,
      transparent 0 27px,
      #ededed 27px 28px
    );
  }

  .gantt-tool.axis-panning .axis-header,
  .gantt-tool.axis-panning .timeline-cell {
    cursor: grabbing;
  }

  .no-filter-results {
    display: grid;
    grid-column: 1 / -1;
    place-items: center;
    color: #777;
    font-size: 12px;
    background: #fbfbfb;
    border-bottom: 1px solid #e7e7e7;
  }

  .today-line {
    position: absolute;
    z-index: 1;
    top: 0;
    bottom: 0;
    width: 1px;
    background: rgb(104 69 173 / 34%);
    pointer-events: none;
  }

  .task-range {
    position: absolute;
    z-index: 2;
    top: 9px;
    height: 28px;
    min-width: 4px;
    overflow: visible;
    background: #8b8f95;
    border: 1px solid #72767d;
    border-radius: 4px;
    box-shadow: 0 1px 2px rgb(0 0 0 / 10%);
  }

  .task-range.dragging {
    filter: brightness(.92);
    box-shadow: 0 2px 5px rgb(34 89 84 / 22%);
  }

  .task-range.status-in-progress {
    background: #228c85;
    border-color: #176f69;
  }

  .task-range.status-completed {
    background: #318b5b;
    border-color: #246f47;
  }

  .range-body,
  .range-handle {
    position: absolute;
    top: 0;
    height: 100%;
    padding: 0;
    border: 0;
    outline: 0;
  }

  .range-body {
    right: 2px;
    left: 2px;
    display: block;
    overflow: hidden;
    color: #fff;
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    text-align: center;
    text-overflow: clip;
    white-space: nowrap;
    background: transparent;
    cursor: grab;
  }

  .task-range.start-clipped .range-body {
    left: 2px;
  }

  .task-range.end-clipped .range-body {
    right: 2px;
  }

  .range-body:active {
    cursor: grabbing;
  }

  .range-body span {
    pointer-events: none;
  }

  .range-handle {
    z-index: 2;
    width: 12px;
    background: transparent;
    cursor: ew-resize;
  }

  .range-handle::after {
    position: absolute;
    top: 4px;
    bottom: 4px;
    width: 2px;
    content: "";
    background: rgb(255 255 255 / 30%);
  }

  .start-handle {
    left: -5px;
    border-radius: 3px 0 0 3px;
  }

  .start-handle::after {
    left: 5px;
  }

  .end-handle {
    right: -5px;
    border-radius: 0 3px 3px 0;
  }

  .end-handle::after {
    right: 5px;
  }

  .range-body:focus-visible,
  .range-handle:focus-visible {
    outline: 2px solid #2d1856;
    outline-offset: 2px;
  }

  .empty-state {
    display: flex;
    flex: 1 1 auto;
    min-height: 220px;
    padding: 28px;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    gap: 9px;
    color: #777;
    text-align: center;
    background: #fff;
    border: 1px dashed #c7c7c7;
    border-radius: 8px;
  }

  .empty-state strong {
    color: #333;
    font-size: 14px;
  }

  .primary-button {
    min-height: 32px;
    justify-content: center;
    gap: 6px;
    padding: 5px 12px;
    margin-top: 4px;
    color: #fff;
    background: #6845ad;
    border: 1px solid #573697;
    border-radius: 5px;
    cursor: pointer;
  }

  .primary-button:hover,
  .primary-button:focus-visible {
    background: #5a399b;
  }

  :global(.spinner) {
    animation: spin 850ms linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  @keyframes completion-flash {
    0% { background-color: #e7f4eb; }
    28% { background-color: #c6ead2; }
    100% { background-color: #e7f4eb; }
  }

  @keyframes completion-pop {
    0% { opacity: 0; transform: scale(.72); }
    55% { opacity: 1; transform: scale(1.12); }
    100% { opacity: 1; transform: scale(1); }
  }

  @media (max-width: 760px) {
    main { padding: 10px; }
    .gantt-grid { --progress-column-width: 88px; }
    .gantt-toolbar { overflow-x: auto; }
    .progress-cell { padding: 0 7px; }
  }

  @media (max-width: 900px) {
    .titlebar-filter { display: none; }
    .gantt-toolbar { display: flex; }
  }

  @media (prefers-reduced-motion: reduce) {
    :global(.spinner),
    .task-cell.celebrating,
    .task-cell.celebrating .completion-badge { animation: none; }
  }
</style>

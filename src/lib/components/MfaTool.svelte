<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    AlertTriangle,
    Check,
    ClipboardPaste,
    Copy,
    Eye,
    EyeOff,
    FileOutput,
    GripVertical,
    Image,
    KeyRound,
    Keyboard,
    LoaderCircle,
    Pencil,
    Pin,
    PinOff,
    Plus,
    RefreshCw,
    QrCode,
    RotateCcw,
    ScanLine,
    Search,
    ShieldCheck,
    Trash2,
    Upload,
    X,
  } from "@lucide/svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import {
    formatOtpCode,
    maskedOtpCode,
    mfaApi,
    secondsUntil,
    validUntilMilliseconds,
    type MfaAlgorithm,
    type MfaApi,
    type MfaEntrySummary,
    type MfaEntryExport,
    type MfaTrashEntrySummary,
    type MfaImportPreview,
    type MfaRevealResult,
    type MfaStatus,
    type MfaUriPreviewError,
  } from "../mfa";

  interface Props {
    api?: MfaApi;
  }

  type AddMethod = "screen" | "uri" | "image" | "manual";
  type RecoveryDialogMode = "setup" | "unlock" | "change";
  type DropPosition = "before" | "after";

  interface ContextMenuState {
    entryId: string;
    x: number;
    y: number;
  }

  interface ReorderPointerState {
    pointerId: number;
    sourceId: string;
  }

  let { api = mfaApi }: Props = $props();

  const emojiGroups: ReadonlyArray<{ label: string; values: ReadonlyArray<string> }> = [
    { label: "常用", values: ["🔐", "🔑", "🛡️", "🌸", "⭐", "💼", "🏠", "👤"] },
    { label: "服务", values: ["🐙", "☁️", "📧", "💬", "🛒", "🏦", "🎮", "🧰"] },
    { label: "颜色", values: ["🟣", "🔵", "🟢", "🟡", "🟠", "🔴", "⚫", "⚪"] },
  ];

  const addMethods: ReadonlyArray<{ value: AddMethod; label: string }> = [
    { value: "screen", label: "扫描屏幕" },
    { value: "uri", label: "粘贴链接" },
    { value: "image", label: "二维码图片" },
    { value: "manual", label: "手动输入" },
  ];

  let entries = $state<MfaEntrySummary[]>([]);
  let status = $state<MfaStatus | null>(null);
  let loading = $state(true);
  let error = $state("");
  let searchText = $state("");
  let now = $state(Date.now());
  let revealed = $state<Record<string, MfaRevealResult>>({});
  let revealingIds = $state<Record<string, true>>({});
  let copyingIds = $state<Record<string, true>>({});
  let pinningIds = $state<Record<string, true>>({});
  let contextMenu = $state<ContextMenuState | null>(null);
  let contextMenuElement = $state<HTMLElement | null>(null);
  let toast = $state("");
  let reorderPointerState = $state<ReorderPointerState | null>(null);
  let reorderDragId = $state<string | null>(null);
  let reorderDropId = $state<string | null>(null);
  let reorderDropPosition = $state<DropPosition>("after");
  let reordering = $state(false);
  let mfaToolElement = $state<HTMLElement | null>(null);
  let mainElement = $state<HTMLElement | null>(null);

  let recoveryDialogMode = $state<RecoveryDialogMode | null>(null);
  let currentRecoveryPassword = $state("");
  let recoveryPassword = $state("");
  let recoveryPasswordConfirm = $state("");
  let recoveryError = $state("");
  let recoveryBusy = $state(false);
  let recoveryDialog = $state<HTMLElement | null>(null);
  let currentRecoveryPasswordInput = $state<HTMLInputElement | null>(null);
  let recoveryPasswordInput = $state<HTMLInputElement | null>(null);
  let recoveryReturnFocus: HTMLElement | null = null;

  let exportTarget = $state<MfaEntrySummary | null>(null);
  let exportResult = $state<MfaEntryExport | null>(null);
  let exportPassword = $state("");
  let exportError = $state("");
  let exportBusy = $state(false);
  let exportDialog = $state<HTMLElement | null>(null);
  let exportPasswordInput = $state<HTMLInputElement | null>(null);
  let exportReturnFocus: HTMLElement | null = null;

  let addOpen = $state(false);
  let addMethod = $state<AddMethod>("screen");
  let importBusy = $state(false);
  let previews = $state<MfaImportPreview[]>([]);
  let selectedSessionId = $state("");
  let importEmoji = $state("🔐");
  let uriText = $state("");
  let uriPreviewErrors = $state<MfaUriPreviewError[]>([]);
  let bulkUriImport = $state(false);
  let importError = $state("");
  let dragActive = $state(false);
  let imageInput = $state<HTMLInputElement | null>(null);
  let manualName = $state("");
  let manualIssuer = $state("");
  let manualAccount = $state("");
  let manualSecret = $state("");
  let manualAlgorithm = $state<MfaAlgorithm>("sha1");
  let manualDigits = $state(6);
  let manualPeriod = $state(30);

  let editing = $state<MfaEntrySummary | null>(null);
  let editName = $state("");
  let editIssuer = $state("");
  let editAccount = $state("");
  let editEmoji = $state("🔐");
  let editBusy = $state(false);
  let pendingDelete = $state<MfaEntrySummary | null>(null);
  let deleteBusy = $state(false);
  let addDialog = $state<HTMLElement | null>(null);
  let editDialog = $state<HTMLElement | null>(null);
  let editNameInput = $state<HTMLInputElement | null>(null);
  let contextReturnFocus: HTMLElement | null = null;
  let addReturnFocus: HTMLElement | null = null;
  let editReturnFocus: HTMLElement | null = null;
  let deleteReturnFocus: HTMLElement | null = null;

  let trashOpen = $state(false);
  let trashEntries = $state<MfaTrashEntrySummary[]>([]);
  let trashCount = $state(0);
  let trashLoading = $state(false);
  let trashError = $state("");
  let trashBusyIds = $state<Record<string, true>>({});
  let trashDialog = $state<HTMLElement | null>(null);
  let pendingPermanentDelete = $state<MfaTrashEntrySummary | null>(null);
  let pendingEmptyTrash = $state(false);
  let emptyTrashBusy = $state(false);
  let trashReturnFocus: HTMLElement | null = null;

  let filteredEntries = $derived.by(() => {
    const query = searchText.trim().toLocaleLowerCase();
    if (!query) return entries;
    return entries.filter((entry) => [entry.name, entry.issuer, entry.accountName]
      .some((value) => value.toLocaleLowerCase().includes(query)));
  });
  let searchActive = $derived(Boolean(searchText.trim()));
  let pinBusy = $derived(Object.keys(pinningIds).length > 0);

  let selectedPreview = $derived(previews.find((preview) => preview.sessionId === selectedSessionId) ?? null);
  let previewWarnings = $derived(bulkUriImport
    ? previews.flatMap((preview) => preview.warnings.map((warning) => `${preview.name}：${warning}`))
    : selectedPreview?.warnings ?? []);

  function reasonMessage(reason: unknown, fallback: string): string {
    return reason instanceof Error && reason.message ? reason.message : fallback;
  }

  function setBusy(map: Record<string, true>, id: string, value: boolean): Record<string, true> {
    const next = { ...map };
    if (value) next[id] = true;
    else delete next[id];
    return next;
  }

  function showToast(message: string): void {
    toast = message;
    window.setTimeout(() => {
      if (toast === message) toast = "";
    }, 2_000);
  }

  function metadata(entry: Pick<MfaEntrySummary, "issuer" | "accountName">): string {
    return [entry.issuer, entry.accountName].filter(Boolean).join(" · ") || "未填写账户说明";
  }

  function selectedEmoji(value: string): string {
    const trimmed = value.trim();
    if (!trimmed) return "🔐";
    const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
    return segmenter.segment(trimmed)[Symbol.iterator]().next().value?.segment || "🔐";
  }

  function setImportEmoji(value: string): void {
    importEmoji = selectedEmoji(value);
  }

  function setEditEmoji(value: string): void {
    editEmoji = selectedEmoji(value);
  }

  function revealFor(entry: MfaEntrySummary): MfaRevealResult | null {
    return revealed[entry.id] ?? null;
  }

  function remainingFor(entry: MfaEntrySummary): number {
    const current = revealFor(entry);
    return current ? Math.min(entry.period, secondsUntil(current.validUntil, now)) : entry.period;
  }

  function countdownOffset(entry: MfaEntrySummary): number {
    const ratio = Math.max(0, Math.min(1, remainingFor(entry) / entry.period));
    return 50.27 * (1 - ratio);
  }

  async function refreshList(): Promise<void> {
    try {
      const nextStatus = await api.getStatus();
      status = nextStatus;
      syncRecoveryDialog(nextStatus);
      if (!nextStatus.available || nextStatus.recoveryState === "password-required") {
        entries = [];
        trashEntries = [];
        trashCount = 0;
        revealed = {};
        error = "";
        return;
      }
      const [nextEntries, nextTrash] = await Promise.all([api.list(), api.listTrash()]);
      entries = nextEntries;
      trashCount = nextTrash.length;
      if (trashOpen) trashEntries = nextTrash;
      const validIds = new Set(nextEntries.map((entry) => entry.id));
      revealed = Object.fromEntries(Object.entries(revealed).filter(([id]) => validIds.has(id)));
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "无法读取验证器账户。");
    } finally {
      loading = false;
    }
  }

  async function revealCode(entry: MfaEntrySummary): Promise<void> {
    if (revealingIds[entry.id]) return;
    revealingIds = setBusy(revealingIds, entry.id, true);
    try {
      const result = await api.reveal(entry.id);
      if (!/^\d+$/.test(result.code) || result.code.length !== entry.digits) {
        throw new Error("验证码格式无效，请重新显示。");
      }
      revealed = { ...revealed, [entry.id]: result };
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "无法显示验证码。");
    } finally {
      revealingIds = setBusy(revealingIds, entry.id, false);
    }
  }

  function hideCode(entryId: string): void {
    const next = { ...revealed };
    delete next[entryId];
    revealed = next;
  }

  async function toggleReveal(entry: MfaEntrySummary): Promise<void> {
    if (revealed[entry.id]) {
      hideCode(entry.id);
      return;
    }
    await revealCode(entry);
  }

  async function refreshVisibleCodes(force = false): Promise<void> {
    const jobs: Promise<void>[] = [];
    for (const entry of entries) {
      const current = revealed[entry.id];
      if (!current) continue;
      const expiration = validUntilMilliseconds(current.validUntil);
      if (force || expiration <= Date.now() + 120) jobs.push(revealCode(entry));
    }
    await Promise.all(jobs);
  }

  async function copyCode(entry: MfaEntrySummary): Promise<void> {
    if (copyingIds[entry.id]) return;
    copyingIds = setBusy(copyingIds, entry.id, true);
    try {
      await api.copy(entry.id);
      showToast(`已复制“${entry.name}”的验证码`);
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "复制验证码失败。");
    } finally {
      copyingIds = setBusy(copyingIds, entry.id, false);
    }
  }

  function focusElementAfterRender(element: HTMLElement | null): void {
    void tick().then(() => element?.isConnected && element.focus());
  }

  function clearRecoveryInputs(): void {
    currentRecoveryPassword = "";
    recoveryPassword = "";
    recoveryPasswordConfirm = "";
    recoveryError = "";
  }

  function focusRecoveryPassword(): void {
    void tick().then(() => {
      const input = recoveryDialogMode === "change" ? currentRecoveryPasswordInput : recoveryPasswordInput;
      input?.focus();
    });
  }

  function syncRecoveryDialog(nextStatus: MfaStatus): void {
    const requiredMode = nextStatus.recoveryState === "setup-required"
      ? "setup"
      : nextStatus.recoveryState === "password-required"
        ? "unlock"
        : null;
    if (requiredMode) {
      if (recoveryDialogMode !== requiredMode) clearRecoveryInputs();
      recoveryDialogMode = requiredMode;
      focusRecoveryPassword();
      return;
    }
    if (recoveryDialogMode === "setup" || recoveryDialogMode === "unlock") {
      clearRecoveryInputs();
      recoveryDialogMode = null;
    }
  }

  function openRecoveryPasswordChange(): void {
    if (status?.recoveryState !== "ready" || status.protection === "browser-demo") return;
    recoveryReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    clearRecoveryInputs();
    recoveryDialogMode = "change";
    focusRecoveryPassword();
  }

  function cancelRecoveryDialog(): void {
    if (!recoveryDialogMode || recoveryBusy) return;
    const mandatory = recoveryDialogMode !== "change";
    clearRecoveryInputs();
    if (mandatory) {
      recoveryBusy = true;
      void closeWindow().finally(() => (recoveryBusy = false));
      return;
    }
    recoveryDialogMode = null;
    focusElementAfterRender(recoveryReturnFocus);
  }

  async function submitRecoveryPassword(): Promise<void> {
    const mode = recoveryDialogMode;
    if (!mode || recoveryBusy) return;
    if (mode === "change" && currentRecoveryPassword.length < 12) {
      recoveryError = "请输入至少 12 个字符的原恢复密码。";
      focusRecoveryPassword();
      return;
    }
    if (recoveryPassword.length < 12) {
      recoveryError = "恢复密码至少需要 12 个字符。";
      focusRecoveryPassword();
      return;
    }
    if (recoveryPassword.length > 256) {
      recoveryError = "恢复密码不能超过 256 个字符。";
      focusRecoveryPassword();
      return;
    }
    if (mode !== "unlock" && recoveryPassword !== recoveryPasswordConfirm) {
      recoveryError = "两次输入的恢复密码不一致。";
      focusRecoveryPassword();
      return;
    }

    recoveryBusy = true;
    recoveryError = "";
    try {
      if (mode === "unlock") await api.unlockWithRecoveryPassword(recoveryPassword);
      else if (mode === "change") await api.configureRecoveryPassword(recoveryPassword, currentRecoveryPassword);
      else await api.configureRecoveryPassword(recoveryPassword);
      clearRecoveryInputs();
      recoveryDialogMode = null;
      await refreshList();
      if (mode === "unlock") showToast("MFA 数据已在本机安全解锁");
      else if (mode === "change") showToast("MFA 恢复密码已修改");
      else showToast("MFA 恢复密码已设置");
    } catch (reason) {
      recoveryError = reasonMessage(
        reason,
        mode === "unlock"
          ? "恢复密码不正确，无法解锁 MFA 数据。"
          : mode === "change"
            ? "原恢复密码不正确，无法修改恢复密码。"
            : "无法保存 MFA 恢复密码。",
      );
      focusRecoveryPassword();
    } finally {
      recoveryBusy = false;
    }
  }

  function showContextMenu(entry: MfaEntrySummary, x: number, y: number, returnFocus: HTMLElement | null): void {
    const width = 170;
    const height = 246;
    contextReturnFocus = returnFocus;
    contextMenu = {
      entryId: entry.id,
      x: Math.max(6, Math.min(x, window.innerWidth - width - 6)),
      y: Math.max(6, Math.min(y, window.innerHeight - height - 6)),
    };
    void tick().then(() => contextMenuElement?.querySelector<HTMLElement>("button")?.focus());
  }

  function openContextMenu(event: MouseEvent, entry: MfaEntrySummary): void {
    event.preventDefault();
    showContextMenu(entry, event.clientX, event.clientY, event.currentTarget as HTMLElement);
  }

  function closeContextMenu(restoreFocus = false): void {
    contextMenu = null;
    if (restoreFocus) focusElementAfterRender(contextReturnFocus);
  }

  function handleCardKeydown(event: KeyboardEvent, entry: MfaEntrySummary): void {
    if (event.target !== event.currentTarget) return;
    if (event.key === "Enter") {
      event.preventDefault();
      void copyCode(entry);
      return;
    }
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      event.preventDefault();
      const card = event.currentTarget as HTMLElement;
      const bounds = card.getBoundingClientRect();
      showContextMenu(entry, bounds.left + 20, bounds.top + Math.min(48, bounds.height / 2), card);
    }
  }

  function handleContextMenuKeydown(event: KeyboardEvent): void {
    if (!contextMenuElement) return;
    if (event.key === "Escape") {
      event.preventDefault();
      closeContextMenu(true);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = Array.from(contextMenuElement.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
    if (items.length === 0) return;
    event.preventDefault();
    const activeIndex = items.indexOf(document.activeElement as HTMLButtonElement);
    const nextIndex = event.key === "Home"
      ? 0
      : event.key === "End"
        ? items.length - 1
        : event.key === "ArrowDown"
          ? (activeIndex + 1 + items.length) % items.length
          : (activeIndex - 1 + items.length) % items.length;
    items[nextIndex]?.focus();
  }

  function contextEntry(): MfaEntrySummary | null {
    return entries.find((entry) => entry.id === contextMenu?.entryId) ?? null;
  }

  function optimisticPinnedEntries(entry: MfaEntrySummary, pinned: boolean): MfaEntrySummary[] {
    const saved = { ...entry, pinned };
    const remaining = entries.filter((item) => item.id !== entry.id);
    return pinned
      ? [saved, ...remaining.filter((item) => item.pinned), ...remaining.filter((item) => !item.pinned)]
      : [...remaining.filter((item) => item.pinned), saved, ...remaining.filter((item) => !item.pinned)];
  }

  async function setEntryPinned(
    entry: MfaEntrySummary,
    pinned: boolean,
    returnFocus: HTMLElement | null,
  ): Promise<void> {
    if (pinBusy || reordering) return;
    const previousEntries = [...entries];
    pinningIds = setBusy(pinningIds, entry.id, true);
    entries = optimisticPinnedEntries(entry, pinned);
    error = "";
    try {
      entries = await api.setPinned(entry.id, pinned);
      showToast(pinned ? `已置顶“${entry.name}”` : `已取消置顶“${entry.name}”`);
    } catch (reason) {
      entries = previousEntries;
      error = reasonMessage(reason, pinned ? "置顶账户失败。" : "取消置顶失败。");
    } finally {
      pinningIds = setBusy(pinningIds, entry.id, false);
      focusElementAfterRender(returnFocus);
    }
  }

  function togglePinnedFromContext(entry: MfaEntrySummary): void {
    const returnFocus = contextReturnFocus;
    closeContextMenu();
    void setEntryPinned(entry, !entry.pinned, returnFocus);
  }

  function beginReorderPointer(event: PointerEvent, entry: MfaEntrySummary): void {
    if (
      event.button !== 0
      || searchActive
      || reordering
      || pinBusy
      || reorderPointerState
    ) return;
    event.preventDefault();
    event.stopPropagation();
    closeContextMenu();
    try {
      mfaToolElement?.setPointerCapture?.(event.pointerId);
    } catch {}
    reorderPointerState = { pointerId: event.pointerId, sourceId: entry.id };
    reorderDragId = entry.id;
    reorderDropId = null;
  }

  function updateReorderPointer(event: PointerEvent): void {
    const state = reorderPointerState;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();

    if (mainElement) {
      const bounds = mainElement.getBoundingClientRect();
      if (event.clientY < bounds.top + 28) mainElement.scrollTop = Math.max(0, mainElement.scrollTop - 12);
      else if (event.clientY > bounds.bottom - 28) mainElement.scrollTop += 12;
    }

    const source = entries.find((entry) => entry.id === state.sourceId);
    if (!source) return;
    const cards = Array.from(mfaToolElement?.querySelectorAll<HTMLElement>(".account-card[data-entry-id]") ?? []);
    let targetCard: HTMLElement | null = null;
    let targetBounds: DOMRect | null = null;
    let nearestDistance = Number.POSITIVE_INFINITY;
    for (const card of cards) {
      if (card.dataset.entryId === state.sourceId || card.dataset.entryPinned !== String(source.pinned)) continue;
      const bounds = card.getBoundingClientRect();
      if (bounds.height <= 0) continue;
      const distance = event.clientY < bounds.top
        ? bounds.top - event.clientY
        : event.clientY > bounds.bottom
          ? event.clientY - bounds.bottom
          : 0;
      if (distance < nearestDistance) {
        nearestDistance = distance;
        targetCard = card;
        targetBounds = bounds;
      }
      if (distance === 0) break;
    }

    const targetId = targetCard?.dataset.entryId;
    if (!targetId || !targetBounds) {
      reorderDropId = null;
      return;
    }
    reorderDropId = targetId;
    reorderDropPosition = event.clientY < targetBounds.top + targetBounds.height / 2 ? "before" : "after";
  }

  function finishReorderPointer(cancelled = false): void {
    const pointer = reorderPointerState;
    const sourceId = pointer?.sourceId ?? reorderDragId;
    const targetId = reorderDropId;
    const position = reorderDropPosition;
    if (pointer && mfaToolElement?.hasPointerCapture?.(pointer.pointerId)) {
      mfaToolElement.releasePointerCapture(pointer.pointerId);
    }
    reorderPointerState = null;
    reorderDragId = null;
    reorderDropId = null;
    if (!cancelled && sourceId && targetId) void persistReorder(sourceId, targetId, position);
  }

  function finishReorderFromEvent(event: PointerEvent, cancelled = false): void {
    if (!reorderPointerState || reorderPointerState.pointerId !== event.pointerId) return;
    finishReorderPointer(cancelled);
  }

  async function persistReorder(sourceId: string, targetId: string, position: DropPosition): Promise<void> {
    if (sourceId === targetId || searchActive || reordering || pinBusy) return;
    const source = entries.find((entry) => entry.id === sourceId);
    const target = entries.find((entry) => entry.id === targetId);
    if (!source || !target || source.pinned !== target.pinned) return;

    const previousEntries = [...entries];
    const reordered = entries.filter((entry) => entry.id !== sourceId);
    const targetIndex = reordered.findIndex((entry) => entry.id === targetId);
    if (targetIndex < 0) return;
    reordered.splice(targetIndex + (position === "after" ? 1 : 0), 0, source);
    if (reordered.every((entry, index) => entry.id === entries[index]?.id)) return;

    entries = reordered;
    reordering = true;
    error = "";
    try {
      entries = await api.reorder(reordered.map((entry) => entry.id));
    } catch (reason) {
      entries = previousEntries;
      error = reasonMessage(reason, "调整 MFA 账户顺序失败。");
    } finally {
      reordering = false;
    }
  }

  function moveReorderByKeyboard(event: KeyboardEvent, entry: MfaEntrySummary): void {
    if (
      !event.altKey
      || (event.key !== "ArrowUp" && event.key !== "ArrowDown")
      || searchActive
      || reordering
      || pinBusy
    ) return;
    const group = entries.filter((item) => item.pinned === entry.pinned);
    const index = group.findIndex((item) => item.id === entry.id);
    const targetIndex = index + (event.key === "ArrowUp" ? -1 : 1);
    if (index < 0 || targetIndex < 0 || targetIndex >= group.length) return;
    event.preventDefault();
    const target = group[targetIndex];
    void persistReorder(entry.id, target.id, event.key === "ArrowUp" ? "before" : "after");
  }

  function stopHandleClick(event: MouseEvent): void {
    event.stopPropagation();
  }

  function startEdit(entry: MfaEntrySummary): void {
    if (pinBusy || reordering) return;
    editReturnFocus = contextReturnFocus;
    closeContextMenu();
    editing = entry;
    editName = entry.name;
    editIssuer = entry.issuer;
    editAccount = entry.accountName;
    editEmoji = entry.iconEmoji || "🔐";
    error = "";
    void tick().then(() => editNameInput?.focus());
  }

  function clearExportState(): void {
    exportPassword = "";
    exportResult = null;
    exportError = "";
  }

  function startExport(entry: MfaEntrySummary): void {
    if (pinBusy || reordering) return;
    exportReturnFocus = contextReturnFocus;
    closeContextMenu();
    clearExportState();
    exportTarget = entry;
    void tick().then(() => exportPasswordInput?.focus());
  }

  function closeExport(): void {
    if (exportBusy) return;
    clearExportState();
    exportTarget = null;
    focusElementAfterRender(exportReturnFocus);
    exportReturnFocus = null;
  }

  async function submitExportPassword(): Promise<void> {
    const target = exportTarget;
    if (!target || exportBusy || exportResult) return;
    if (!exportPassword) {
      exportError = "请输入 MFA 恢复密码。";
      exportPasswordInput?.focus();
      return;
    }
    exportBusy = true;
    exportError = "";
    try {
      exportResult = await api.exportEntry(target.id, exportPassword);
      exportPassword = "";
      await tick();
      exportDialog?.querySelector<HTMLButtonElement>('[aria-label="复制密钥"]')?.focus();
    } catch (reason) {
      exportPassword = "";
      exportError = reasonMessage(reason, "恢复密码不正确，无法导出这个账户。");
      await tick();
      exportPasswordInput?.focus();
    } finally {
      exportBusy = false;
    }
  }

  async function copyExportValue(kind: "secret" | "uri"): Promise<void> {
    const result = exportResult;
    if (!result || exportBusy) return;
    const value = kind === "secret" ? result.secretBase32 : result.otpauthUri;
    try {
      await navigator.clipboard.writeText(value);
      showToast(kind === "secret" ? "密钥已复制" : "验证器链接已复制");
      exportError = "";
    } catch (reason) {
      exportError = reasonMessage(reason, kind === "secret" ? "复制密钥失败。" : "复制验证器链接失败。");
    }
  }

  function closeEdit(): void {
    if (editBusy) return;
    editing = null;
    focusElementAfterRender(editReturnFocus);
  }

  async function saveEdit(): Promise<void> {
    if (!editing || editBusy) return;
    if (!editName.trim()) {
      error = "请输入账户名称。";
      return;
    }
    editBusy = true;
    try {
      const saved = await api.update({
        id: editing.id,
        name: editName.trim(),
        issuer: editIssuer.trim(),
        accountName: editAccount.trim(),
        iconEmoji: selectedEmoji(editEmoji),
      });
      entries = entries.map((entry) => entry.id === saved.id ? saved : entry);
      editing = null;
      focusElementAfterRender(editReturnFocus);
      error = "";
      showToast("账户信息已更新");
    } catch (reason) {
      error = reasonMessage(reason, "保存账户失败。");
    } finally {
      editBusy = false;
    }
  }

  function requestDelete(entry: MfaEntrySummary): void {
    if (pinBusy || reordering) return;
    deleteReturnFocus = contextReturnFocus;
    closeContextMenu();
    pendingDelete = entry;
  }

  function restoreDeleteFocus(): void {
    void tick().then(() => {
      const target = deleteReturnFocus?.isConnected
        ? deleteReturnFocus
        : document.querySelector<HTMLElement>('.account-card[tabindex="0"]');
      target?.focus();
      deleteReturnFocus = null;
    });
  }

  async function confirmDelete(): Promise<void> {
    const entry = pendingDelete;
    if (!entry || deleteBusy) return;
    deleteBusy = true;
    try {
      await api.delete(entry.id);
      entries = entries.filter((item) => item.id !== entry.id);
      trashCount += 1;
      hideCode(entry.id);
      if (editing?.id === entry.id) editing = null;
      pendingDelete = null;
      restoreDeleteFocus();
      showToast("账户已移入回收站");
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "删除账户失败。");
    } finally {
      deleteBusy = false;
    }
  }

  function formatDeletedAt(value: string): string {
    const date = new Date(value);
    if (!Number.isFinite(date.getTime())) return value;
    return new Intl.DateTimeFormat("zh-CN", {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    }).format(date);
  }

  async function openTrash(): Promise<void> {
    if (status && !status.available) return;
    trashReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeContextMenu();
    trashOpen = true;
    trashLoading = true;
    trashError = "";
    error = "";
    try {
      trashEntries = await api.listTrash();
      trashCount = trashEntries.length;
    } catch (reason) {
      error = reasonMessage(reason, "无法读取 MFA 回收站。");
      trashOpen = false;
    } finally {
      trashLoading = false;
      if (trashOpen) void tick().then(() => trashDialog?.querySelector<HTMLElement>("button")?.focus());
    }
  }

  function closeTrash(): void {
    if (trashLoading || emptyTrashBusy || Object.keys(trashBusyIds).length > 0) return;
    trashOpen = false;
    trashEntries = [];
    trashError = "";
    pendingPermanentDelete = null;
    pendingEmptyTrash = false;
    focusElementAfterRender(trashReturnFocus);
    trashReturnFocus = null;
  }

  async function restoreTrashEntry(entry: MfaTrashEntrySummary): Promise<void> {
    if (trashBusyIds[entry.id] || emptyTrashBusy) return;
    trashBusyIds = setBusy(trashBusyIds, entry.id, true);
    try {
      await api.restore(entry.id);
      trashEntries = trashEntries.filter((item) => item.id !== entry.id);
      trashCount = Math.max(0, trashCount - 1);
      entries = await api.list();
      showToast(`已恢复“${entry.name}”`);
      trashError = "";
    } catch (reason) {
      trashError = reasonMessage(reason, "恢复 MFA 账户失败。");
    } finally {
      trashBusyIds = setBusy(trashBusyIds, entry.id, false);
    }
  }

  function requestPermanentDelete(entry: MfaTrashEntrySummary): void {
    if (trashBusyIds[entry.id] || emptyTrashBusy) return;
    pendingPermanentDelete = entry;
  }

  async function confirmPermanentDelete(): Promise<void> {
    const entry = pendingPermanentDelete;
    if (!entry || trashBusyIds[entry.id] || emptyTrashBusy) return;
    trashBusyIds = setBusy(trashBusyIds, entry.id, true);
    try {
      await api.permanentlyDelete(entry.id);
      trashEntries = trashEntries.filter((item) => item.id !== entry.id);
      trashCount = Math.max(0, trashCount - 1);
      pendingPermanentDelete = null;
      showToast(`已永久删除“${entry.name}”`);
      trashError = "";
    } catch (reason) {
      trashError = reasonMessage(reason, "永久删除 MFA 账户失败。");
    } finally {
      trashBusyIds = setBusy(trashBusyIds, entry.id, false);
    }
  }

  async function confirmEmptyTrash(): Promise<void> {
    if (!pendingEmptyTrash || emptyTrashBusy || Object.keys(trashBusyIds).length > 0) return;
    emptyTrashBusy = true;
    try {
      await api.emptyTrash();
      trashEntries = [];
      trashCount = 0;
      pendingEmptyTrash = false;
      showToast("MFA 回收站已清空");
      trashError = "";
    } catch (reason) {
      trashError = reasonMessage(reason, "清空 MFA 回收站失败。");
    } finally {
      emptyTrashBusy = false;
    }
  }

  function resetSensitiveInputs(): void {
    uriText = "";
    manualName = "";
    manualIssuer = "";
    manualAccount = "";
    manualSecret = "";
    manualAlgorithm = "sha1";
    manualDigits = 6;
    manualPeriod = 30;
  }

  async function discardPreviews(): Promise<void> {
    const sessions = previews.map((preview) => preview.sessionId);
    previews = [];
    selectedSessionId = "";
    uriPreviewErrors = [];
    bulkUriImport = false;
    importError = "";
    await Promise.all(sessions.map((sessionId) => api.cancelImport(sessionId).catch(() => undefined)));
  }

  function openAdd(): void {
    if (pinBusy || reordering) return;
    if (status && !status.available) {
      error = status.message || "MFA 数据保险库当前不可用。";
      return;
    }
    addReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    addOpen = true;
    addMethod = "screen";
    importEmoji = "🔐";
    previews = [];
    selectedSessionId = "";
    uriPreviewErrors = [];
    bulkUriImport = false;
    importError = "";
    resetSensitiveInputs();
    closeContextMenu();
    error = "";
    void tick().then(() => addDialog?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus());
  }

  async function closeAdd(): Promise<void> {
    if (importBusy) return;
    addOpen = false;
    resetSensitiveInputs();
    await discardPreviews();
    focusElementAfterRender(addReturnFocus);
  }

  async function switchAddMethod(method: AddMethod): Promise<void> {
    if (importBusy || method === addMethod) return;
    await discardPreviews();
    resetSensitiveInputs();
    addMethod = method;
    error = "";
  }

  async function handleMethodTabKeydown(event: KeyboardEvent, method: AddMethod): Promise<void> {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const currentIndex = addMethods.findIndex((item) => item.value === method);
    const nextIndex = event.key === "Home"
      ? 0
      : event.key === "End"
        ? addMethods.length - 1
        : event.key === "ArrowRight"
          ? (currentIndex + 1) % addMethods.length
          : (currentIndex - 1 + addMethods.length) % addMethods.length;
    const nextMethod = addMethods[nextIndex]?.value;
    if (!nextMethod) return;
    await switchAddMethod(nextMethod);
    await tick();
    addDialog?.querySelector<HTMLElement>(`[data-add-method="${nextMethod}"]`)?.focus();
  }

  function trapDialogFocus(event: KeyboardEvent, container: HTMLElement | null): void {
    if (event.key !== "Tab" || !container) return;
    const focusable = Array.from(container.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])',
    )).filter((element) => element.offsetParent !== null || element === document.activeElement);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && (document.activeElement === first || !container.contains(document.activeElement))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (document.activeElement === last || !container.contains(document.activeElement))) {
      event.preventDefault();
      first.focus();
    }
  }

  async function acceptPreviews(request: () => Promise<MfaImportPreview[]>): Promise<void> {
    if (importBusy) return;
    importBusy = true;
    error = "";
    importError = "";
    try {
      await discardPreviews();
      const result = await request();
      if (result.length === 0) throw new Error("没有识别到可用的 TOTP 二维码。");
      previews = result;
      selectedSessionId = result[0].sessionId;
      importEmoji = result[0].iconEmoji || importEmoji || "🔐";
    } catch (reason) {
      importError = reasonMessage(reason, "识别账户失败。");
    } finally {
      importBusy = false;
      manualSecret = "";
    }
  }

  async function scanScreen(): Promise<void> {
    await acceptPreviews(() => api.scanScreenQr());
  }

  async function previewUri(): Promise<void> {
    if (!uriText.trim()) {
      importError = "请粘贴 otpauth://totp 链接。";
      return;
    }
    if (importBusy) return;
    const sourceText = uriText;
    const nonEmptyLines = sourceText.split(/\r?\n/).filter((line) => line.trim());
    if (nonEmptyLines.length === 1) {
      await acceptPreviews(() => api.previewUri(nonEmptyLines[0].trim()));
      if (previews.length > 0) uriText = "";
      return;
    }
    importBusy = true;
    error = "";
    importError = "";
    try {
      await discardPreviews();
      const result = await api.previewUris(sourceText);
      uriPreviewErrors = result.errors;
      bulkUriImport = true;
      if (result.previews.length === 0) {
        importError = result.errors.length > 0
          ? "没有识别到可导入的 TOTP 链接，请根据行号修正。"
          : "没有识别到可导入的 TOTP 链接。";
        return;
      }
      previews = result.previews;
      selectedSessionId = result.previews[0].sessionId;
      importEmoji = result.previews[0].iconEmoji || importEmoji || "🔐";
      uriText = "";
    } catch (reason) {
      importError = reasonMessage(reason, "识别验证器链接失败。");
    } finally {
      importBusy = false;
    }
  }

  function isSupportedImage(file: File): boolean {
    return ["image/png", "image/jpeg", "image/webp"].includes(file.type.toLowerCase());
  }

  async function previewImage(file: File): Promise<void> {
    if (!isSupportedImage(file)) {
      error = "请选择 PNG、JPEG 或 WebP 图片。";
      return;
    }
    const maxImageBytes = 32 * 1024 * 1024;
    if (file.size > maxImageBytes) {
      error = "二维码图片不能超过 32 MiB，请选择较小的图片。";
      return;
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    try {
      await acceptPreviews(() => api.previewQrImage(bytes, file.type));
    } finally {
      bytes.fill(0);
    }
  }

  async function handleImageInput(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (file) await previewImage(file);
  }

  async function handleDrop(event: DragEvent): Promise<void> {
    event.preventDefault();
    dragActive = false;
    const file = Array.from(event.dataTransfer?.files ?? []).find(isSupportedImage);
    if (!file) {
      error = "拖入的内容中没有可识别的二维码图片。";
      return;
    }
    await previewImage(file);
  }

  async function handlePaste(event: ClipboardEvent): Promise<void> {
    if (!addOpen || importBusy) return;
    const file = Array.from(event.clipboardData?.files ?? []).find(isSupportedImage);
    if (file) {
      event.preventDefault();
      addMethod = "image";
      await previewImage(file);
      return;
    }
    if (addMethod === "manual") return;
    const text = event.clipboardData?.getData("text/plain") ?? "";
    if (text.trim().toLocaleLowerCase().startsWith("otpauth")) {
      event.preventDefault();
      addMethod = "uri";
      uriText = text;
      await previewUri();
    }
  }

  async function readClipboard(): Promise<void> {
    if (!navigator.clipboard) {
      error = "当前环境不能直接读取剪贴板，请使用 Ctrl+V 粘贴。";
      return;
    }
    try {
      if (navigator.clipboard.read) {
        const items = await navigator.clipboard.read();
        for (const item of items) {
          const imageType = item.types.find((type) => ["image/png", "image/jpeg", "image/webp"].includes(type));
          if (imageType) {
            addMethod = "image";
            const blob = await item.getType(imageType);
            await previewImage(new File([blob], "clipboard-image", { type: imageType }));
            return;
          }
        }
      }
      const text = await navigator.clipboard.readText();
      if (!text.trim()) throw new Error("剪贴板里没有链接或二维码图片。");
      addMethod = "uri";
      uriText = text;
      await previewUri();
    } catch (reason) {
      error = reasonMessage(reason, "读取剪贴板失败，请使用 Ctrl+V 粘贴。");
    }
  }

  async function previewManual(): Promise<void> {
    if (!manualSecret.trim()) {
      error = "请输入 Base32 密钥。";
      return;
    }
    if (!manualName.trim() && !manualIssuer.trim() && !manualAccount.trim()) {
      error = "请至少填写账户名称、服务商或账号。";
      return;
    }
    await acceptPreviews(() => api.previewManual({
      name: manualName.trim(),
      issuer: manualIssuer.trim(),
      accountName: manualAccount.trim(),
      secret: manualSecret.trim(),
      iconEmoji: importEmoji,
      algorithm: manualAlgorithm,
      digits: Number(manualDigits),
      period: Number(manualPeriod),
    }));
  }

  async function commitImport(): Promise<void> {
    if (importBusy || previews.length === 0 || (!bulkUriImport && !selectedPreview)) return;
    importBusy = true;
    error = "";
    importError = "";
    try {
      const savedEntries = bulkUriImport
        ? await api.commitImports(previews.map((preview) => ({
          sessionId: preview.sessionId,
          iconEmoji: selectedEmoji(preview.iconEmoji || "🔐"),
        })))
        : [await api.commitImport(selectedPreview!.sessionId, selectedEmoji(importEmoji))];
      if (savedEntries.length === 0) throw new Error("后端未返回已导入的账户。");
      entries = [...entries, ...savedEntries];
      if (!bulkUriImport) {
        const remainingSessions = previews
          .filter((preview) => preview.sessionId !== selectedPreview!.sessionId)
          .map((preview) => preview.sessionId);
        await Promise.all(remainingSessions.map((sessionId) => api.cancelImport(sessionId).catch(() => undefined)));
      }
      const importedCount = savedEntries.length;
      previews = [];
      selectedSessionId = "";
      uriPreviewErrors = [];
      bulkUriImport = false;
      importError = "";
      resetSensitiveInputs();
      addOpen = false;
      focusElementAfterRender(addReturnFocus);
      try {
        await api.copy(savedEntries[0].id);
        showToast(importedCount > 1
          ? `已添加 ${importedCount} 个账户，首个验证码已复制`
          : "账户已添加，首个验证码已复制");
      } catch {
        showToast(importedCount > 1
          ? `已添加 ${importedCount} 个账户，但首个验证码复制失败`
          : "账户已添加，但首个验证码复制失败");
      }
    } catch (reason) {
      importError = reasonMessage(reason, bulkUriImport ? "批量添加账户失败，未写入任何账户。" : "添加账户失败。");
    } finally {
      importBusy = false;
    }
  }

  async function closeWindow(): Promise<void> {
    try {
      await api.lock();
    } catch {
      // Closing must remain possible even when the backend is already shutting down.
    }
    revealed = {};
    resetSensitiveInputs();
    clearRecoveryInputs();
    clearExportState();
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
      // Browser preview has no native window.
    }
  }

  function handleGlobalPointerDown(event: PointerEvent): void {
    const target = event.target as Element | null;
    if (contextMenu && !target?.closest(".entry-context-menu")) closeContextMenu();
  }

  function handleWindowBlur(): void {
    if (reorderPointerState) finishReorderPointer(true);
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    if (reorderPointerState) {
      event.preventDefault();
      finishReorderPointer(true);
      return;
    }
    if (recoveryDialogMode && !recoveryBusy) {
      cancelRecoveryDialog();
      return;
    }
    if (exportTarget && !exportBusy) {
      closeExport();
      return;
    }
    if (trashOpen && !trashLoading && !emptyTrashBusy && Object.keys(trashBusyIds).length === 0) {
      closeTrash();
      return;
    }
    if (contextMenu) {
      closeContextMenu(true);
      return;
    }
    if (editing) {
      closeEdit();
      return;
    }
    if (addOpen && !importBusy) void closeAdd();
  }

  onMount(() => {
    let disposed = false;
    let clockHandle: number | undefined;
    let unlisten: (() => void) | undefined;
    let lastWallClock = Date.now();
    let lastMonotonic = performance.now();

    const tick = (): void => {
      const wallClock = Date.now();
      const monotonic = performance.now();
      const wallDelta = wallClock - lastWallClock;
      const monotonicDelta = monotonic - lastMonotonic;
      const clockJumped = Math.abs(wallDelta - monotonicDelta) > 5_000;
      lastWallClock = wallClock;
      lastMonotonic = monotonic;
      now = wallClock;
      void refreshVisibleCodes(clockJumped);
      const delay = Math.max(80, 1_020 - (now % 1_000));
      clockHandle = window.setTimeout(tick, delay);
    };
    const resync = (): void => {
      lastWallClock = Date.now();
      lastMonotonic = performance.now();
      now = lastWallClock;
      if (!document.hidden) void refreshVisibleCodes(true);
    };
    const beforeUnload = (): void => {
      revealed = {};
      void api.lock();
    };

    void refreshList();
    tick();
    window.addEventListener("focus", resync);
    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("pointerdown", handleGlobalPointerDown);
    window.addEventListener("keydown", handleGlobalKeydown);
    window.addEventListener("beforeunload", beforeUnload);
    document.addEventListener("visibilitychange", resync);

    if (api.isDesktop()) {
      void import("@tauri-apps/api/event").then(async ({ listen }) => {
        const cleanup = await listen("mfa_changed", () => void refreshList());
        if (disposed) cleanup();
        else unlisten = cleanup;
      }).catch(() => undefined);
    }

    return () => {
      disposed = true;
      finishReorderPointer(true);
      if (clockHandle !== undefined) window.clearTimeout(clockHandle);
      unlisten?.();
      window.removeEventListener("focus", resync);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("pointerdown", handleGlobalPointerDown);
      window.removeEventListener("keydown", handleGlobalKeydown);
      window.removeEventListener("beforeunload", beforeUnload);
      document.removeEventListener("visibilitychange", resync);
      revealed = {};
      resetSensitiveInputs();
      clearRecoveryInputs();
      clearExportState();
      void api.lock();
    };
  });
</script>

<section
  bind:this={mfaToolElement}
  class="mfa-tool"
  data-testid="mfa-tool"
  aria-label="MFA 验证器"
  onpaste={handlePaste}
  onpointermove={updateReorderPointer}
  onpointerup={(event) => finishReorderFromEvent(event)}
  onpointercancel={(event) => finishReorderFromEvent(event, true)}
  onlostpointercapture={(event) => finishReorderFromEvent(event, true)}
>
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <ShieldCheck size={18} aria-hidden="true" />
      <h1 data-tauri-drag-region>MFA 验证器</h1>
    </div>
    <label class="search-box">
      <Search size={15} aria-hidden="true" />
      <span class="visually-hidden">搜索账户</span>
      <input bind:value={searchText} type="search" placeholder="搜索" aria-label="搜索账户" />
    </label>
    <div class="window-actions">
      <button
        type="button"
        class="trash-action"
        aria-label={trashCount > 0 ? `打开 MFA 回收站，${trashCount} 项` : "打开 MFA 回收站"}
        title="回收站"
        disabled={Boolean(status && !status.available)}
        onclick={() => void openTrash()}
      >
        <Trash2 size={18} aria-hidden="true" />
        {#if trashCount > 0}<span aria-hidden="true">{trashCount > 99 ? "99+" : trashCount}</span>{/if}
      </button>
      {#if status?.recoveryState === "ready" && status.protection !== "browser-demo"}
        <button type="button" aria-label="修改 MFA 恢复密码" title="修改恢复密码" onclick={openRecoveryPasswordChange}>
          <KeyRound size={18} aria-hidden="true" />
        </button>
      {/if}
      <button
        type="button"
        class="primary-icon"
        aria-label="新增 MFA 账户"
        title={pinBusy || reordering ? "账户顺序保存完成后可添加" : "新增账户"}
        disabled={Boolean(status && !status.available) || pinBusy || reordering}
        onclick={openAdd}
      >
        <Plus size={18} aria-hidden="true" />
      </button>
      <button type="button" aria-label="关闭 MFA 验证器" title="关闭" onclick={() => void closeWindow()}>
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  </header>

  <main bind:this={mainElement}>
    {#if error}
      <div class="error-banner" role="alert">
        <AlertTriangle size={16} aria-hidden="true" />
        <span>{error}</span>
        <button type="button" aria-label="关闭错误提示" onclick={() => (error = "")}><X size={14} aria-hidden="true" /></button>
      </div>
    {/if}

    {#if status?.protection === "browser-demo"}
      <div class="info-banner">浏览器预览只显示模拟验证码；真实账户仅在 Windows 客户端加密保存。</div>
    {:else if status && status.captureExcluded === false}
      <div class="warning-banner">当前系统未能启用窗口防截屏保护，请避免在录屏或共享屏幕时显示验证码。</div>
    {/if}

    {#if status?.recoveredFromBackup}
      <div class="info-banner" role="status">
        {status.message || "MFA 主保险库缺失或损坏，已从最近的有效备份恢复。"}
      </div>
    {/if}

    {#if loading}
      <div class="empty-state" aria-busy="true">
        <LoaderCircle class="spinner" size={24} aria-hidden="true" />
        <span>正在安全打开验证器…</span>
      </div>
    {:else if status && !status.available}
      <div class="empty-state unavailable" role="alert">
        <div class="empty-icon danger"><AlertTriangle size={28} aria-hidden="true" /></div>
        <strong>MFA 数据保险库不可用</strong>
        <span>{status.message || "无法使用当前 Windows 用户解锁验证器数据，请检查数据目录和用户身份。"}</span>
      </div>
    {:else if entries.length === 0}
      <div class="empty-state">
        <div class="empty-icon"><ShieldCheck size={30} aria-hidden="true" /></div>
        <strong>还没有验证码</strong>
        <span>扫描二维码或粘贴 otpauth 链接即可添加。</span>
        <button class="primary-button" type="button" disabled={pinBusy || reordering} onclick={openAdd}><Plus size={16} aria-hidden="true" /> 添加账户</button>
      </div>
    {:else if filteredEntries.length === 0}
      <div class="empty-state compact">
        <Search size={25} aria-hidden="true" />
        <strong>没有匹配的账户</strong>
        <span>换一个名称、服务商或账号试试。</span>
      </div>
    {:else}
      <div class="account-list" aria-label="验证器账户" aria-busy={pinBusy || reordering}>
        {#each filteredEntries as entry (entry.id)}
          {@const currentReveal = revealFor(entry)}
          <div
            class:revealed={Boolean(currentReveal)}
            class:pinned={entry.pinned}
            class:dragging={reorderDragId === entry.id}
            class:drop-before={reorderDropId === entry.id && reorderDropPosition === "before"}
            class:drop-after={reorderDropId === entry.id && reorderDropPosition === "after"}
            class="account-card"
            role="button"
            tabindex="0"
            data-entry-id={entry.id}
            data-entry-pinned={String(entry.pinned)}
            aria-label={`${entry.name}，双击复制验证码`}
            oncontextmenu={(event) => openContextMenu(event, entry)}
            ondblclick={() => void copyCode(entry)}
            onkeydown={(event) => handleCardKeydown(event, entry)}
          >
            <button
              type="button"
              class="reorder-handle"
              aria-label={`调整“${entry.name}”的顺序`}
              aria-pressed={reorderDragId === entry.id}
              aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
              title={searchActive ? "清空搜索后可调整顺序" : "拖动调整顺序；Alt + 方向键移动"}
              disabled={searchActive || reordering || pinBusy}
              onpointerdown={(event) => beginReorderPointer(event, entry)}
              onkeydown={(event) => moveReorderByKeyboard(event, entry)}
              onclick={stopHandleClick}
              ondblclick={stopHandleClick}
            >
              <GripVertical size={17} strokeWidth={2.1} aria-hidden="true" />
            </button>
            <div class="account-icon" aria-hidden="true">{entry.iconEmoji || "🔐"}</div>
            <div class="account-main">
              <div class="account-heading">
                <strong>{entry.name}</strong>
                {#if entry.pinned}
                  <span class="pin-indicator" aria-label="已置顶" title="已置顶"><Pin size={11} fill="currentColor" aria-hidden="true" /></span>
                {/if}
                {#if copyingIds[entry.id]}<span class="copying-label">正在复制…</span>{/if}
              </div>
              <span class="account-meta" title={metadata(entry)}>{metadata(entry)}</span>
              <output
                class:masked={!currentReveal}
                class="otp-code"
                aria-label={currentReveal ? `验证码 ${currentReveal.code}` : "验证码已隐藏"}
                aria-live="polite"
              >
                {currentReveal ? formatOtpCode(currentReveal.code, entry.digits) : maskedOtpCode(entry.digits)}
              </output>
            </div>
            <button
              type="button"
              class:visible={Boolean(currentReveal)}
              class="reveal-button"
              aria-label={currentReveal ? `隐藏“${entry.name}”的验证码` : `显示“${entry.name}”的验证码`}
              aria-pressed={Boolean(currentReveal)}
              title={currentReveal ? "隐藏验证码" : "显示并持续更新"}
              disabled={Boolean(revealingIds[entry.id])}
              onclick={() => void toggleReveal(entry)}
              ondblclick={(event) => event.stopPropagation()}
            >
              {#if revealingIds[entry.id]}
                <LoaderCircle class="spinner" size={20} aria-hidden="true" />
              {:else if currentReveal}
                <svg class="countdown-ring" viewBox="0 0 20 20" aria-hidden="true">
                  <circle class="ring-track" cx="10" cy="10" r="8"></circle>
                  <circle class="ring-value" cx="10" cy="10" r="8" style={`stroke-dashoffset: ${countdownOffset(entry)}`}></circle>
                </svg>
                <span>{remainingFor(entry)}</span>
              {:else}
                <RefreshCw size={20} aria-hidden="true" />
              {/if}
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </main>

  {#if contextMenu}
    {@const entry = contextEntry()}
    {#if entry}
      <div
        bind:this={contextMenuElement}
        class="entry-context-menu"
        role="menu"
        tabindex="-1"
        aria-label={`${entry.name}操作`}
        style={`left: ${contextMenu.x}px; top: ${contextMenu.y}px;`}
        oncontextmenu={(event) => event.preventDefault()}
        onkeydown={handleContextMenuKeydown}
      >
        <button type="button" role="menuitem" onclick={() => { closeContextMenu(true); void copyCode(entry); }}>
          <Copy size={15} aria-hidden="true" /> 复制验证码
        </button>
        <button type="button" role="menuitem" onclick={() => { closeContextMenu(true); void toggleReveal(entry); }}>
          {#if revealed[entry.id]}<EyeOff size={15} aria-hidden="true" /> 隐藏验证码{:else}<Eye size={15} aria-hidden="true" /> 显示验证码{/if}
        </button>
        <button type="button" role="menuitem" disabled={pinBusy || reordering} onclick={() => togglePinnedFromContext(entry)}>
          {#if entry.pinned}<PinOff size={15} aria-hidden="true" /> 取消置顶{:else}<Pin size={15} aria-hidden="true" /> 置顶{/if}
        </button>
        <button type="button" role="menuitem" disabled={pinBusy || reordering} onclick={() => startExport(entry)}>
          <FileOutput size={15} aria-hidden="true" /> 导出
        </button>
        <button type="button" role="menuitem" disabled={pinBusy || reordering} onclick={() => startEdit(entry)}>
          <Pencil size={15} aria-hidden="true" /> 编辑
        </button>
        <button type="button" class="danger" role="menuitem" disabled={pinBusy || reordering} onclick={() => requestDelete(entry)}>
          <Trash2 size={15} aria-hidden="true" /> 删除
        </button>
      </div>
    {/if}
  {/if}

  {#if trashOpen}
    <div class="modal-backdrop trash-backdrop" role="presentation">
      <div
        bind:this={trashDialog}
        class="modal trash-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="mfa-trash-title"
        tabindex="-1"
        onkeydown={(event) => trapDialogFocus(event, trashDialog)}
      >
        <header class="modal-header">
          <div>
            <h2 id="mfa-trash-title">MFA 回收站</h2>
            <p>{trashCount > 0 ? `${trashCount} 个已删除账户` : "删除的账户会先保存在这里"}</p>
          </div>
          <button type="button" aria-label="关闭 MFA 回收站" disabled={trashLoading || emptyTrashBusy || Object.keys(trashBusyIds).length > 0} onclick={closeTrash}>
            <X size={18} aria-hidden="true" />
          </button>
        </header>
        <div class="trash-toolbar">
          <span>回收站内的账户仍受 MFA 保险库加密保护</span>
          <button
            type="button"
            class="trash-clear-button"
            disabled={trashEntries.length === 0 || trashLoading || emptyTrashBusy || Object.keys(trashBusyIds).length > 0}
            onclick={() => (pendingEmptyTrash = true)}
          >
            <Trash2 size={14} aria-hidden="true" /> 清空
          </button>
        </div>
        {#if trashError}
          <div class="trash-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{trashError}</span></div>
        {/if}
        <div class="trash-content">
          {#if trashLoading}
            <div class="trash-empty" aria-busy="true"><LoaderCircle class="spinner" size={23} aria-hidden="true" /><span>正在读取回收站…</span></div>
          {:else if trashEntries.length === 0}
            <div class="trash-empty"><Trash2 size={28} aria-hidden="true" /><strong>回收站是空的</strong></div>
          {:else}
            <div class="trash-list" aria-label="已删除的 MFA 账户">
              {#each trashEntries as entry (entry.id)}
                <article class="trash-entry">
                  <div class="trash-entry-icon" aria-hidden="true">{entry.iconEmoji || "🔐"}</div>
                  <div class="trash-entry-main">
                    <strong>{entry.name}</strong>
                    <span title={metadata(entry)}>{metadata(entry)}</span>
                    <small>删除于 {formatDeletedAt(entry.deletedAt)}</small>
                  </div>
                  <div class="trash-entry-actions">
                    <button
                      type="button"
                      aria-label={`恢复“${entry.name}”`}
                      title="恢复"
                      disabled={Boolean(trashBusyIds[entry.id]) || emptyTrashBusy}
                      onclick={() => void restoreTrashEntry(entry)}
                    >
                      {#if trashBusyIds[entry.id]}<LoaderCircle class="spinner" size={16} aria-hidden="true" />{:else}<RotateCcw size={16} aria-hidden="true" />{/if}
                    </button>
                    <button
                      type="button"
                      class="danger"
                      aria-label={`永久删除“${entry.name}”`}
                      title="永久删除"
                      disabled={Boolean(trashBusyIds[entry.id]) || emptyTrashBusy}
                      onclick={() => requestPermanentDelete(entry)}
                    ><Trash2 size={16} aria-hidden="true" /></button>
                  </div>
                </article>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    </div>
  {/if}

  {#if recoveryDialogMode}
    {@const isUnlocking = recoveryDialogMode === "unlock"}
    {@const isChanging = recoveryDialogMode === "change"}
    <div class="modal-backdrop recovery-backdrop" role="presentation">
      <div
        bind:this={recoveryDialog}
        class="modal recovery-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="mfa-recovery-title"
        tabindex="-1"
        onkeydown={(event) => trapDialogFocus(event, recoveryDialog)}
      >
        <form novalidate onsubmit={(event) => { event.preventDefault(); void submitRecoveryPassword(); }}>
          <header class="modal-header">
            <div>
              <h2 id="mfa-recovery-title">{isUnlocking ? "使用恢复密码迁移" : isChanging ? "修改 MFA 恢复密码" : "设置 MFA 恢复密码"}</h2>
              <p>{isUnlocking ? "验证成功后，这台电脑将恢复 Windows 免密解锁。" : "恢复密码仅用于换电脑或更换 Windows 用户。"}</p>
            </div>
            <button
              type="button"
              aria-label={isChanging ? "取消修改恢复密码" : "关闭 MFA 验证器"}
              disabled={recoveryBusy}
              onclick={cancelRecoveryDialog}
            ><X size={18} aria-hidden="true" /></button>
          </header>
          <div class="recovery-content">
            <div class="recovery-intro">
              <div class="recovery-icon"><KeyRound size={24} aria-hidden="true" /></div>
              <div class="recovery-copy">
                <strong>{isUnlocking ? "解锁从其他电脑迁移来的 MFA 数据" : isChanging ? "验证原密码后再设置新密码" : "本机使用仍然不需要输入密码"}</strong>
                <p>
                  {#if isUnlocking}
                    输入原电脑设置的恢复密码。解锁后，飞花会为当前 Windows 用户建立新的本机保护。
                  {:else if isChanging}
                    原恢复密码只用于验证身份；修改后，跨电脑迁移和账户导出都需要使用新密码。
                  {:else}
                    飞花日常由 Windows DPAPI 自动解锁。只有把飞花数据迁移到其他电脑或 Windows 用户时，才需要这个恢复密码。
                  {/if}
                </p>
              </div>
            </div>

            {#if isChanging}
              <label class="field">
                <span>原恢复密码</span>
                <input
                  bind:this={currentRecoveryPasswordInput}
                  bind:value={currentRecoveryPassword}
                  type="password"
                  minlength="12"
                  maxlength="256"
                  autocomplete="current-password"
                  aria-label="原恢复密码"
                  required
                />
              </label>
            {/if}

            <label class="field">
              <span>{isChanging ? "新恢复密码" : "恢复密码"}</span>
              <input
                bind:this={recoveryPasswordInput}
                bind:value={recoveryPassword}
                type="password"
                minlength="12"
                maxlength="256"
                autocomplete={isUnlocking ? "current-password" : "new-password"}
                aria-label={isChanging ? "新恢复密码" : "恢复密码"}
                required
              />
              {#if !isUnlocking}<small>至少 12 个字符，建议使用只有你知道的长密码。</small>{/if}
            </label>

            {#if !isUnlocking}
              <label class="field">
                <span>{isChanging ? "确认新恢复密码" : "确认恢复密码"}</span>
                <input
                  bind:value={recoveryPasswordConfirm}
                  type="password"
                  minlength="12"
                  maxlength="256"
                  autocomplete="new-password"
                  aria-label={isChanging ? "确认新恢复密码" : "确认恢复密码"}
                  required
                />
              </label>
              <div class="recovery-warning"><AlertTriangle size={15} aria-hidden="true" /><span>飞花不会保存或找回恢复密码。遗忘后只能在仍可打开 MFA 的原电脑上重新设置。</span></div>
            {/if}

            {#if recoveryError}
              <div class="recovery-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{recoveryError}</span></div>
            {/if}
          </div>
          <div class="modal-actions">
            <button class="secondary-button" type="button" disabled={recoveryBusy} onclick={cancelRecoveryDialog}>{isChanging ? "取消" : "关闭"}</button>
            <button class="primary-button" type="submit" disabled={recoveryBusy}>
              {#if recoveryBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{:else}<KeyRound size={15} aria-hidden="true" />{/if}
              {isUnlocking ? "解锁并迁移" : isChanging ? "保存新密码" : "设置恢复密码"}
            </button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if exportTarget}
    <div class="modal-backdrop export-backdrop" role="presentation">
      <div
        bind:this={exportDialog}
        class="modal export-modal"
        class:export-result-modal={Boolean(exportResult)}
        role="dialog"
        aria-modal="true"
        aria-labelledby="mfa-export-title"
        tabindex="-1"
        onkeydown={(event) => trapDialogFocus(event, exportDialog)}
      >
        <header class="modal-header">
          <div>
            <h2 id="mfa-export-title">导出“{exportTarget.name}”</h2>
            <p>{exportResult ? "可导入其他支持 TOTP 的验证器" : "需要先验证当前 MFA 恢复密码"}</p>
          </div>
          <button type="button" aria-label="关闭导出账户" disabled={exportBusy} onclick={closeExport}>
            <X size={18} aria-hidden="true" />
          </button>
        </header>

        {#if exportResult}
          <div class="export-content">
            <div class="export-warning">
              <AlertTriangle size={17} aria-hidden="true" />
              <span>密钥、链接和二维码都能生成此账户的验证码，请仅在可信设备上使用。</span>
            </div>
            <div class="export-account">
              <span class="preview-icon" aria-hidden="true">{exportResult.iconEmoji || "🔐"}</span>
              <div>
                <strong>{exportResult.name}</strong>
                <span>{metadata(exportResult)}</span>
                <small>{exportResult.algorithm.toUpperCase()} · {exportResult.digits} 位 · {exportResult.period} 秒</small>
              </div>
            </div>
            <section class="export-value export-secret" aria-labelledby="mfa-export-secret-label">
              <strong id="mfa-export-secret-label">密钥</strong>
              <code title={exportResult.secretBase32}>{exportResult.secretBase32}</code>
              <button type="button" aria-label="复制密钥" title="复制密钥" onclick={() => void copyExportValue("secret")}>
                <Copy size={16} aria-hidden="true" />
              </button>
            </section>
            <section class="export-value export-uri" aria-labelledby="mfa-export-uri-label">
              <strong id="mfa-export-uri-label">otpauth</strong>
              <code class="uri-value" title={exportResult.otpauthUri}>{exportResult.otpauthUri}</code>
              <button type="button" aria-label="复制 otpauth 链接" title="复制链接" onclick={() => void copyExportValue("uri")}>
                <Copy size={16} aria-hidden="true" />
              </button>
            </section>
            <section class="export-qr" aria-labelledby="mfa-export-qr-label">
              <div>
                <QrCode size={18} aria-hidden="true" />
                <strong id="mfa-export-qr-label">验证器二维码</strong>
              </div>
              <img src={exportResult.qrPngDataUrl} alt={`${exportResult.name} 的 TOTP 导入二维码`} />
            </section>
            {#if exportError}
              <div class="recovery-error export-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{exportError}</span></div>
            {/if}
          </div>
          <div class="modal-actions">
            <button class="primary-button" type="button" onclick={closeExport}>完成</button>
          </div>
        {:else}
          <form novalidate onsubmit={(event) => { event.preventDefault(); void submitExportPassword(); }}>
            <div class="export-password-content">
              <div class="recovery-intro">
                <div class="recovery-icon"><FileOutput size={23} aria-hidden="true" /></div>
                <div class="recovery-copy">
                  <strong>验证后显示此账户的迁移信息</strong>
                  <p>验证过程不会修改恢复密码，也不会更改账户数据。</p>
                </div>
              </div>
              <label class="field">
                <span>恢复密码</span>
                <input
                  bind:this={exportPasswordInput}
                  bind:value={exportPassword}
                  type="password"
                  minlength="12"
                  maxlength="256"
                  autocomplete="current-password"
                  aria-label="导出恢复密码"
                  required
                />
              </label>
              {#if exportError}
                <div class="recovery-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{exportError}</span></div>
              {/if}
            </div>
            <div class="modal-actions">
              <button class="secondary-button" type="button" disabled={exportBusy} onclick={closeExport}>取消</button>
              <button class="primary-button" type="submit" disabled={exportBusy || !exportPassword}>
                {#if exportBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{:else}<FileOutput size={15} aria-hidden="true" />{/if}
                验证并导出
              </button>
            </div>
          </form>
        {/if}
      </div>
    </div>
  {/if}

  {#if addOpen}
    <div class="modal-backdrop" role="presentation">
      <div
        bind:this={addDialog}
        class="modal add-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="add-mfa-title"
        tabindex="-1"
        onkeydown={(event) => trapDialogFocus(event, addDialog)}
      >
        <header class="modal-header">
          <div><h2 id="add-mfa-title">添加验证器账户</h2><p>密钥只会发送到本机加密保险库。</p></div>
          <button type="button" aria-label="关闭添加账户" disabled={importBusy} onclick={() => void closeAdd()}><X size={18} aria-hidden="true" /></button>
        </header>

        <div class="method-tabs" role="tablist" aria-label="添加方式">
          {#each addMethods as method}
            <button
              type="button"
              role="tab"
              data-add-method={method.value}
              class:active={addMethod === method.value}
              aria-selected={addMethod === method.value}
              tabindex={addMethod === method.value ? 0 : -1}
              disabled={importBusy}
              onclick={() => void switchAddMethod(method.value)}
              onkeydown={(event) => void handleMethodTabKeydown(event, method.value)}
            >
              {#if method.value === "screen"}<ScanLine size={16} aria-hidden="true" />
              {:else if method.value === "uri"}<ClipboardPaste size={16} aria-hidden="true" />
              {:else if method.value === "image"}<Image size={16} aria-hidden="true" />
              {:else}<Keyboard size={16} aria-hidden="true" />{/if}
              {method.label}
            </button>
          {/each}
        </div>

        {#if previews.length === 0}
          <div class="method-panel" role="tabpanel">
            {#if importError}
              <div class="import-operation-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{importError}</span></div>
            {/if}
            {#if addMethod === "screen"}
              <div class="method-hero">
                <div class="hero-icon"><ScanLine size={30} aria-hidden="true" /></div>
                <strong>扫描屏幕上的二维码</strong>
                <p>飞花会暂时隐藏此窗口，只扫描鼠标所在显示器。截图不会保存到磁盘。</p>
                <button class="primary-button" type="button" disabled={importBusy} onclick={() => void scanScreen()}>
                  {#if importBusy}<LoaderCircle class="spinner" size={16} aria-hidden="true" />{:else}<ScanLine size={16} aria-hidden="true" />{/if}
                  开始扫描
                </button>
              </div>
            {:else if addMethod === "uri"}
              <label class="field full-field">
                <span>验证器链接（一行一个）</span>
                <textarea bind:value={uriText} rows="7" spellcheck="false" autocomplete="off" aria-label="验证器链接（一行一个）" placeholder={'otpauth://totp/Example%3Aalice?secret=…\notpauth://totp/Example%3Abob?secret=…'}></textarea>
                <small>可一次识别多个标准 TOTP 链接；空行会忽略。</small>
              </label>
              {#if uriPreviewErrors.length}
                <div class="uri-preview-errors" role="alert" aria-label="链接识别错误">
                  <AlertTriangle size={15} aria-hidden="true" />
                  <div><strong>以下链接未识别</strong><ul>{#each uriPreviewErrors as item}<li><b>第 {item.line} 行</b><span>{item.message}</span></li>{/each}</ul></div>
                </div>
              {/if}
              <div class="panel-actions">
                <button class="secondary-button" type="button" disabled={importBusy} onclick={() => void readClipboard()}><ClipboardPaste size={15} aria-hidden="true" />读取剪贴板</button>
                <button class="primary-button" type="button" disabled={importBusy || !uriText.trim()} onclick={() => void previewUri()}>
                  {#if importBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{/if} 识别链接
                </button>
              </div>
            {:else if addMethod === "image"}
              <button
                type="button"
                class:drag-active={dragActive}
                class="drop-zone"
                disabled={importBusy}
                onclick={() => imageInput?.click()}
                ondragenter={(event) => { event.preventDefault(); dragActive = true; }}
                ondragover={(event) => event.preventDefault()}
                ondragleave={() => (dragActive = false)}
                ondrop={handleDrop}
              >
                {#if importBusy}<LoaderCircle class="spinner" size={28} aria-hidden="true" />{:else}<Upload size={28} aria-hidden="true" />{/if}
                <strong>拖放、粘贴或选择二维码图片</strong>
                <span>支持 PNG、JPEG、WebP</span>
              </button>
              <input class="visually-hidden" bind:this={imageInput} type="file" accept="image/png,image/jpeg,image/webp" onchange={handleImageInput} />
              <div class="panel-actions centered">
                <button class="secondary-button" type="button" disabled={importBusy} onclick={() => void readClipboard()}><ClipboardPaste size={15} aria-hidden="true" />从剪贴板粘贴</button>
              </div>
            {:else}
              <div class="manual-grid">
                <label class="field"><span>账户名称</span><input bind:value={manualName} autocomplete="off" placeholder="例如：GitHub" /></label>
                <label class="field"><span>服务商</span><input bind:value={manualIssuer} autocomplete="off" placeholder="Issuer" /></label>
                <label class="field full-field"><span>账号</span><input bind:value={manualAccount} autocomplete="off" placeholder="name@example.com" /></label>
                <label class="field full-field"><span>Base32 密钥</span><input bind:value={manualSecret} type="password" autocomplete="new-password" spellcheck="false" placeholder="输入密钥，不含空格也可以" /></label>
                <label class="field"><span>算法</span><select bind:value={manualAlgorithm}><option value="sha1">SHA-1</option><option value="sha256">SHA-256</option><option value="sha512">SHA-512</option></select></label>
                <label class="field"><span>位数</span><select bind:value={manualDigits}><option value={6}>6 位</option><option value={7}>7 位</option><option value={8}>8 位</option></select></label>
                <label class="field"><span>周期（秒）</span><input bind:value={manualPeriod} type="number" min="1" max="3600" step="1" /></label>
              </div>
              <div class="emoji-section compact-emoji">
                <span>图标</span>
                <div class="emoji-row">
                  {#each emojiGroups[0].values as emoji}
                    <button type="button" class:selected={importEmoji === emoji} aria-label={`使用 ${emoji} 图标`} onclick={() => setImportEmoji(emoji)}>{emoji}</button>
                  {/each}
                  <label class="custom-emoji"><span class="visually-hidden">自定义 Emoji</span><input value={importEmoji} maxlength="8" aria-label="自定义 Emoji" oninput={(event) => setImportEmoji((event.currentTarget as HTMLInputElement).value)} /></label>
                </div>
              </div>
              <div class="panel-actions">
                <span class="security-note"><ShieldCheck size={14} aria-hidden="true" />不会在前端保存密钥</span>
                <button class="primary-button" type="button" disabled={importBusy || !manualSecret.trim()} onclick={() => void previewManual()}>
                  {#if importBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{/if} 检查账户
                </button>
              </div>
            {/if}
          </div>
        {:else}
          <div class="preview-panel">
            {#if importError}
              <div class="import-operation-error" role="alert"><AlertTriangle size={15} aria-hidden="true" /><span>{importError}</span></div>
            {/if}
            <div class="preview-heading">
              <div><strong>确认识别结果</strong><span>{bulkUriImport ? `将一次导入 ${previews.length} 个账户，每项已分配独立图标` : previews.length > 1 ? `识别到 ${previews.length} 个标准账户，请选择一个` : "密钥不会显示在预览中"}</span></div>
              <button class="text-button" type="button" disabled={importBusy} onclick={() => void discardPreviews()}>重新识别</button>
            </div>
            {#if bulkUriImport}
              <div class="preview-list" role="list" aria-label="将导入的账户">
                {#each previews as preview (preview.sessionId)}
                  <div class="preview-card bulk-preview-card" role="listitem">
                    <span class="preview-icon">{preview.iconEmoji || "🔐"}</span>
                    <span class="preview-main"><strong>{preview.name}</strong><span>{metadata(preview)}</span><small>{preview.algorithm.toUpperCase()} · {preview.digits} 位 · {preview.period} 秒</small></span>
                    <Check size={17} aria-label="将导入" />
                  </div>
                {/each}
              </div>
            {:else}
              <div class="preview-list" role="radiogroup" aria-label="选择导入账户">
                {#each previews as preview (preview.sessionId)}
                  <label class:selected={selectedSessionId === preview.sessionId} class="preview-card">
                    <input type="radio" name="mfa-preview" value={preview.sessionId} bind:group={selectedSessionId} />
                    <span class="preview-icon">{preview.iconEmoji || "🔐"}</span>
                    <span class="preview-main"><strong>{preview.name}</strong><span>{metadata(preview)}</span><small>{preview.algorithm.toUpperCase()} · {preview.digits} 位 · {preview.period} 秒</small></span>
                    {#if selectedSessionId === preview.sessionId}<Check size={18} aria-label="已选择" />{/if}
                  </label>
                {/each}
              </div>
            {/if}
            {#if uriPreviewErrors.length}
              <div class="uri-preview-errors compact" role="alert" aria-label="链接识别错误">
                <AlertTriangle size={15} aria-hidden="true" />
                <div><strong>已跳过以下链接</strong><ul>{#each uriPreviewErrors as item}<li><b>第 {item.line} 行</b><span>{item.message}</span></li>{/each}</ul></div>
              </div>
            {/if}
            {#if previewWarnings.length}
              <div class="preview-warnings" role="status">
                <AlertTriangle size={16} aria-hidden="true" />
                <ul>{#each previewWarnings as warning}<li>{warning}</li>{/each}</ul>
              </div>
            {/if}
            {#if !bulkUriImport}
              <div class="emoji-section">
                <span>选择图标</span>
                {#each emojiGroups as group}
                  <div class="emoji-group"><small>{group.label}</small><div class="emoji-row">
                    {#each group.values as emoji}<button type="button" class:selected={importEmoji === emoji} aria-label={`使用 ${emoji} 图标`} onclick={() => setImportEmoji(emoji)}>{emoji}</button>{/each}
                  </div></div>
                {/each}
                <label class="custom-emoji wide"><span>自定义 Emoji</span><input value={importEmoji} maxlength="8" aria-label="自定义 Emoji" oninput={(event) => setImportEmoji((event.currentTarget as HTMLInputElement).value)} /></label>
              </div>
            {/if}
            <div class="modal-actions">
              <button class="secondary-button" type="button" disabled={importBusy} onclick={() => void closeAdd()}>取消</button>
              <button class="primary-button" type="button" disabled={importBusy || (bulkUriImport ? previews.length === 0 : !selectedPreview)} onclick={() => void commitImport()}>
                {#if importBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{:else}<Check size={15} aria-hidden="true" />{/if}
                {bulkUriImport ? `添加 ${previews.length} 个账户并复制首个验证码` : "添加并复制验证码"}
              </button>
            </div>
          </div>
        {/if}
      </div>
    </div>
  {/if}

  {#if editing}
    <div class="modal-backdrop" role="presentation">
      <div
        bind:this={editDialog}
        class="modal edit-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="edit-mfa-title"
        tabindex="-1"
        onkeydown={(event) => trapDialogFocus(event, editDialog)}
      >
        <form class="edit-form" onsubmit={(event) => { event.preventDefault(); void saveEdit(); }}>
          <header class="modal-header"><div><h2 id="edit-mfa-title">编辑账户</h2><p>出于安全考虑，这里不能查看或修改原密钥。</p></div><button type="button" aria-label="关闭编辑账户" disabled={editBusy} onclick={closeEdit}><X size={18} aria-hidden="true" /></button></header>
          <div class="edit-content">
            <label class="field"><span>账户名称</span><input bind:this={editNameInput} bind:value={editName} maxlength="100" required /></label>
            <label class="field"><span>服务商</span><input bind:value={editIssuer} maxlength="100" /></label>
            <label class="field"><span>账号</span><input bind:value={editAccount} maxlength="160" /></label>
            <div class="emoji-section"><span>选择图标</span>
              {#each emojiGroups as group}<div class="emoji-group"><small>{group.label}</small><div class="emoji-row">{#each group.values as emoji}<button type="button" class:selected={editEmoji === emoji} aria-label={`使用 ${emoji} 图标`} onclick={() => setEditEmoji(emoji)}>{emoji}</button>{/each}</div></div>{/each}
              <label class="custom-emoji wide"><span>自定义 Emoji</span><input value={editEmoji} maxlength="8" aria-label="编辑自定义 Emoji" oninput={(event) => setEditEmoji((event.currentTarget as HTMLInputElement).value)} /></label>
            </div>
          </div>
          <div class="modal-actions"><button class="secondary-button" type="button" disabled={editBusy} onclick={closeEdit}>取消</button><button class="primary-button" type="submit" disabled={editBusy || !editName.trim()}>{#if editBusy}<LoaderCircle class="spinner" size={15} aria-hidden="true" />{:else}<Check size={15} aria-hidden="true" />{/if}保存修改</button></div>
        </form>
      </div>
    </div>
  {/if}

  {#if toast}<div class="toast" role="status"><Check size={15} aria-hidden="true" />{toast}</div>{/if}
  <ConfirmDialog
    open={Boolean(pendingDelete)}
    title={pendingDelete ? `将“${pendingDelete.name}”移入回收站？` : "删除 MFA 账户？"}
    detail="账户会保留在加密的 MFA 回收站中，可以从右上角回收站恢复。"
    confirmLabel="移入回收站"
    busy={deleteBusy}
    oncancel={() => { if (!deleteBusy) { pendingDelete = null; restoreDeleteFocus(); } }}
    onconfirm={confirmDelete}
  />
  <ConfirmDialog
    open={Boolean(pendingPermanentDelete)}
    title={pendingPermanentDelete ? `永久删除“${pendingPermanentDelete.name}”？` : "永久删除 MFA 账户？"}
    detail="密钥将从 MFA 保险库中永久移除，此操作无法恢复。"
    confirmLabel="永久删除"
    busy={Boolean(pendingPermanentDelete && trashBusyIds[pendingPermanentDelete.id])}
    oncancel={() => { if (!pendingPermanentDelete || !trashBusyIds[pendingPermanentDelete.id]) pendingPermanentDelete = null; }}
    onconfirm={confirmPermanentDelete}
  />
  <ConfirmDialog
    open={pendingEmptyTrash}
    title="清空 MFA 回收站？"
    detail={`回收站中的 ${trashEntries.length} 个账户及其密钥将被永久删除，此操作无法恢复。`}
    confirmLabel="清空回收站"
    busy={emptyTrashBusy}
    oncancel={() => { if (!emptyTrashBusy) pendingEmptyTrash = false; }}
    onconfirm={confirmEmptyTrash}
  />
</section>

<style>
  :global(*) { box-sizing: border-box; }
  .mfa-tool { position: relative; display: grid; width: 100%; height: 100%; min-width: 0; min-height: 0; grid-template-rows: 44px minmax(0, 1fr); color: #202020; font-family: "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif; background: #f3f3f3; overflow: hidden; }
  .titlebar { display: flex; min-width: 0; align-items: center; padding: 5px 6px 5px 12px; gap: 10px; background: rgb(250 250 250 / 96%); border-bottom: 1px solid #d7d7d7; user-select: none; }
  .brand, .window-actions, .search-box, .error-banner, .primary-button, .secondary-button, .panel-actions, .security-note, .account-heading { display: flex; align-items: center; }
  .brand { flex: 0 0 auto; gap: 8px; color: #5b3f94; }
  h1 { margin: 0; color: #252525; font-size: 14px; font-weight: 650; white-space: nowrap; }
  .search-box { width: min(210px, 38%); height: 30px; padding: 0 8px; margin-left: auto; gap: 6px; color: #777; background: #fff; border: 1px solid #d2d2d2; border-radius: 5px; }
  .search-box:focus-within { border-color: #7352b9; box-shadow: inset 0 -2px #7352b9; }
  .search-box input { width: 100%; min-width: 0; height: 26px; padding: 0; background: transparent; border: 0; outline: 0; }
  .window-actions { flex: 0 0 auto; gap: 2px; }
  button, input, textarea, select { font: inherit; }
  .window-actions button, .modal-header > button, .error-banner button { display: grid; width: 32px; height: 32px; padding: 0; place-items: center; color: #555; background: transparent; border: 0; border-radius: 5px; cursor: pointer; }
  .window-actions button:hover, .modal-header > button:hover, .error-banner button:hover { color: #202020; background: #e7e7e7; }
  .window-actions button:disabled { opacity: .45; cursor: default; }
  .window-actions .primary-icon { color: #5c3fa3; }
  .window-actions .trash-action { position: relative; }
  .window-actions .trash-action > span { position: absolute; top: 1px; right: 0; min-width: 14px; height: 14px; padding: 0 3px; color: #fff; font-size: 8px; font-weight: 700; line-height: 14px; text-align: center; background: #b42318; border: 1px solid #fff; border-radius: 7px; }
  .window-actions button:last-child:hover { color: #fff; background: #c42b1c; }
  main { min-width: 0; min-height: 0; padding: 14px; overflow: auto; }
  .error-banner { min-height: 38px; padding: 7px 7px 7px 10px; margin-bottom: 10px; gap: 8px; color: #8c1d14; font-size: 12.5px; line-height: 1.4; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .error-banner span { flex: 1; }
  .error-banner button { width: 26px; height: 26px; color: #9b4037; }
  .info-banner, .warning-banner { padding: 7px 10px; margin-bottom: 10px; font-size: 11.5px; line-height: 1.4; border-radius: 6px; }
  .info-banner { color: #503775; background: #f0eafb; border: 1px solid #d8c9f0; }
  .warning-banner { color: #76520b; background: #fff8df; border: 1px solid #ead69a; }
  .account-list { display: grid; gap: 8px; }
  .account-card { position: relative; display: grid; min-height: 88px; padding: 11px 12px 11px 6px; grid-template-columns: 24px 48px minmax(0, 1fr) 42px; align-items: center; gap: 9px; background: #fff; border: 1px solid #d8d8d8; border-radius: 8px; box-shadow: 0 1px 4px rgb(0 0 0 / 4%); cursor: default; transition: border-color 120ms ease, box-shadow 120ms ease, transform 120ms ease, opacity 120ms ease; }
  .account-card:hover { border-color: #c5b7dc; box-shadow: 0 3px 10px rgb(58 37 93 / 8%); }
  .account-card:focus-visible { border-color: #8f72bc; outline: 2px solid rgb(104 69 173 / 24%); outline-offset: 1px; }
  .account-card.revealed { border-color: #baa5dc; background: linear-gradient(100deg, #fff 0%, #fcfaff 100%); }
  .account-card.pinned { border-left-color: #9474bd; box-shadow: inset 3px 0 #a78ac9, 0 1px 4px rgb(0 0 0 / 4%); }
  .account-card.dragging { z-index: 2; opacity: .7; transform: scale(.99); }
  .account-card.drop-before::after, .account-card.drop-after::after { position: absolute; z-index: 3; right: 4px; left: 4px; height: 3px; content: ""; background: #6d49a7; border-radius: 2px; box-shadow: 0 0 0 2px rgb(109 73 167 / 13%); pointer-events: none; }
  .account-card.drop-before::after { top: -6px; }
  .account-card.drop-after::after { bottom: -6px; }
  .reorder-handle { display: grid; width: 24px; height: 34px; padding: 0; place-items: center; color: #91899b; touch-action: none; user-select: none; background: transparent; border: 0; border-radius: 5px; cursor: grab; }
  .reorder-handle:hover, .reorder-handle:focus-visible, .account-card.dragging .reorder-handle { color: #593b86; background: #f0eaf7; outline: 0; }
  .reorder-handle:focus-visible { box-shadow: 0 0 0 2px rgb(104 69 173 / 23%); }
  .reorder-handle:active { cursor: grabbing; }
  .reorder-handle:disabled { color: #b9b5bd; background: transparent; cursor: not-allowed; opacity: .58; }
  .account-icon { display: grid; width: 46px; height: 46px; place-items: center; font-size: 24px; line-height: 1; background: #f0ecf7; border: 1px solid #ded4ed; border-radius: 50%; user-select: none; }
  .account-main { display: grid; min-width: 0; gap: 2px; }
  .account-heading { min-width: 0; gap: 7px; }
  .account-heading strong { min-width: 0; overflow: hidden; color: #28252d; font-size: 13.5px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
  .pin-indicator { display: grid; width: 18px; height: 18px; flex: 0 0 auto; place-items: center; color: #67469a; background: #eee7f7; border: 1px solid #ded2ed; border-radius: 50%; }
  .copying-label { color: #7a6995; font-size: 9.5px; white-space: nowrap; }
  .account-meta { overflow: hidden; color: #79747f; font-size: 10.5px; line-height: 1.35; text-overflow: ellipsis; white-space: nowrap; }
  .otp-code { margin-top: 2px; color: #362552; font-family: "Cascadia Mono", "Segoe UI Mono", Consolas, monospace; font-size: clamp(20px, 5.3vw, 27px); font-variant-numeric: tabular-nums; font-weight: 650; letter-spacing: .08em; line-height: 1.12; white-space: nowrap; }
  .otp-code.masked { color: #9a92a5; font-size: clamp(18px, 4.8vw, 24px); letter-spacing: .04em; }
  .reveal-button { position: relative; display: grid; width: 38px; height: 38px; padding: 0; place-items: center; color: #69557f; background: #f3eff8; border: 1px solid #dfd5eb; border-radius: 50%; cursor: pointer; }
  .reveal-button:hover, .reveal-button:focus-visible { color: #4e3377; background: #eae2f5; outline: 0; box-shadow: 0 0 0 2px rgb(104 69 173 / 18%); }
  .reveal-button:disabled { opacity: .65; cursor: wait; }
  .reveal-button.visible { color: #593a8b; background: #f7f3fc; }
  .reveal-button > span { position: absolute; font-size: 8.5px; font-weight: 700; font-variant-numeric: tabular-nums; }
  .countdown-ring { width: 26px; height: 26px; transform: rotate(-90deg); }
  .countdown-ring circle { fill: none; stroke-width: 2; }
  .ring-track { stroke: #ded5e8; }
  .ring-value { stroke: #7250ad; stroke-linecap: round; stroke-dasharray: 50.27; transition: stroke-dashoffset 220ms linear; }
  .empty-state { display: flex; min-height: 260px; padding: 32px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: #777; text-align: center; background: #fff; border: 1px dashed #c7c7c7; border-radius: 8px; }
  .empty-state.compact { min-height: 190px; }
  .empty-state strong { color: #343038; font-size: 14px; }
  .empty-state span { max-width: 310px; font-size: 12px; line-height: 1.5; }
  .empty-icon, .hero-icon { display: grid; width: 54px; height: 54px; place-items: center; color: #67449f; background: #eee7f8; border-radius: 50%; }
  .empty-icon.danger { color: #a62b20; background: #fbe9e7; }
  .empty-state.unavailable { border-style: solid; border-color: #e1c4bf; }
  .primary-button, .secondary-button { min-height: 33px; justify-content: center; gap: 6px; padding: 6px 12px; border-radius: 5px; cursor: pointer; }
  .primary-button { color: #fff; background: #6845ad; border: 1px solid #573697; }
  .primary-button:hover, .primary-button:focus-visible { background: #5a399b; outline: 0; }
  .secondary-button { color: #3e3a44; background: #fff; border: 1px solid #c7c4ca; }
  .secondary-button:hover, .secondary-button:focus-visible { background: #f1eff3; outline: 0; }
  .primary-button:disabled, .secondary-button:disabled { opacity: .55; cursor: default; }
  .entry-context-menu { position: fixed; z-index: 40; display: grid; width: 164px; padding: 4px; gap: 2px; background: #fff; border: 1px solid #d2d2d2; border-radius: 6px; box-shadow: 0 8px 24px rgb(0 0 0 / 18%); }
  .entry-context-menu button { display: flex; width: 100%; height: 34px; padding: 0 9px; align-items: center; gap: 8px; color: #303030; background: transparent; border: 0; border-radius: 4px; cursor: pointer; }
  .entry-context-menu button:hover, .entry-context-menu button:focus-visible { background: #f0f0f0; outline: 0; }
  .entry-context-menu button:disabled { color: #999; cursor: wait; opacity: .65; }
  .entry-context-menu button.danger { color: #b42318; }
  .entry-context-menu button.danger:hover { background: #fff0ed; }
  .modal-backdrop { position: fixed; z-index: 30; inset: 0; display: grid; padding: 18px; place-items: center; background: rgb(27 24 31 / 38%); backdrop-filter: blur(2px); }
  .recovery-backdrop, .export-backdrop, .trash-backdrop { z-index: 70; }
  .modal { display: flex; width: min(640px, 100%); max-height: 100%; min-height: 0; overflow: hidden; flex-direction: column; background: #f8f8f8; border: 1px solid #c7c5c9; border-radius: 10px; box-shadow: 0 18px 54px rgb(0 0 0 / 24%); }
  .add-modal { min-height: min(520px, 100%); }
  .edit-modal { width: min(500px, 100%); }
  .recovery-modal { width: min(510px, 100%); }
  .export-modal { width: min(560px, 100%); }
  .export-modal.export-result-modal { min-height: min(590px, 100%); }
  .trash-modal { width: min(570px, 100%); min-height: min(440px, 100%); }
  .recovery-modal form, .export-modal form { display: flex; min-height: 0; flex-direction: column; }
  .edit-form { display: flex; flex: 1 1 auto; min-height: 0; flex-direction: column; overflow: hidden; }
  .modal-header { display: flex; flex: 0 0 auto; min-height: 61px; padding: 11px 10px 10px 15px; align-items: center; justify-content: space-between; gap: 12px; background: #fff; border-bottom: 1px solid #dedde0; }
  .modal-header h2 { margin: 0; color: #29262d; font-size: 15px; font-weight: 650; }
  .modal-header p { margin: 3px 0 0; color: #77717d; font-size: 10.5px; }
  .modal-header > button:disabled { opacity: .5; cursor: default; }
  .method-tabs { display: grid; flex: 0 0 auto; padding: 9px 10px; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 4px; background: #f1f0f2; border-bottom: 1px solid #dddbe0; }
  .method-tabs button { display: flex; min-width: 0; min-height: 34px; padding: 5px 7px; align-items: center; justify-content: center; gap: 5px; overflow: hidden; color: #68636c; font-size: 11.5px; white-space: nowrap; background: transparent; border: 1px solid transparent; border-radius: 5px; cursor: pointer; }
  .method-tabs button:hover { color: #342b3e; background: rgb(255 255 255 / 60%); }
  .method-tabs button.active { color: #4b316f; font-weight: 650; background: #fff; border-color: #d1c6df; box-shadow: 0 1px 3px rgb(0 0 0 / 7%); }
  .method-tabs button:disabled { opacity: .55; cursor: default; }
  .method-panel, .preview-panel, .edit-content { flex: 1 1 auto; min-height: 0; padding: 16px; overflow: auto; }
  .method-hero { display: flex; min-height: 320px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; text-align: center; }
  .method-hero strong { margin-top: 4px; color: #302c35; font-size: 14px; }
  .method-hero p { max-width: 390px; margin: 0 0 7px; color: #77717d; font-size: 11.5px; line-height: 1.55; }
  .field { display: grid; min-width: 0; gap: 5px; color: #514c55; font-size: 11.5px; }
  .field > span, .emoji-section > span { font-weight: 600; }
  .field input, .field textarea, .field select, .custom-emoji input { width: 100%; min-width: 0; color: #202020; background: #fff; border: 1px solid #bdb9c1; border-radius: 5px; outline: 0; }
  .field input, .field select { height: 34px; padding: 0 9px; }
  .field textarea { min-height: 96px; padding: 8px 9px; resize: vertical; line-height: 1.45; }
  .field input:focus, .field textarea:focus, .field select:focus, .custom-emoji input:focus { border-color: #7352b9; box-shadow: inset 0 -2px #7352b9; }
  .field small { color: #7b7580; line-height: 1.45; }
  .recovery-content { display: grid; padding: 17px; gap: 13px; overflow: auto; }
  .recovery-intro { display: grid; padding: 12px; grid-template-columns: 42px minmax(0, 1fr); align-items: start; gap: 11px; color: #463752; background: #f2edf8; border: 1px solid #d9cce9; border-radius: 7px; }
  .recovery-icon { display: grid; width: 40px; height: 40px; place-items: center; color: #65439d; background: #fff; border: 1px solid #ded2eb; border-radius: 50%; }
  .recovery-copy { display: grid; min-width: 0; gap: 4px; }
  .recovery-copy strong { font-size: 12.5px; }
  .recovery-copy p { margin: 0; color: #6d6375; font-size: 11px; line-height: 1.5; }
  .recovery-warning, .recovery-error { display: flex; padding: 8px 9px; align-items: flex-start; gap: 7px; font-size: 10.5px; line-height: 1.45; border-radius: 6px; }
  .recovery-warning { color: #76520b; background: #fff8df; border: 1px solid #ead69a; }
  .recovery-error { color: #8c1d14; background: #fff0ed; border: 1px solid #f1c1bb; }
  .recovery-warning :global(svg), .recovery-error :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .export-password-content, .export-content { display: grid; min-height: 0; padding: 17px; gap: 12px; overflow: auto; }
  .export-result-modal .export-content { flex: 1 1 auto; padding: 14px 17px; grid-template-areas: "warning" "account" "secret" "uri" "qr" "error"; grid-template-rows: auto auto auto auto minmax(0, 1fr) auto; gap: 9px; overflow: hidden; }
  .export-warning { display: flex; padding: 9px 10px; align-items: flex-start; gap: 8px; color: #794b08; font-size: 10.5px; line-height: 1.45; background: #fff8df; border: 1px solid #ead69a; border-radius: 6px; }
  .export-result-modal .export-warning { grid-area: warning; }
  .export-warning :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .export-account { display: grid; padding: 9px 10px; grid-template-columns: 38px minmax(0, 1fr); align-items: center; gap: 10px; background: #f2edf8; border: 1px solid #ddd2e9; border-radius: 7px; }
  .export-result-modal .export-account { grid-area: account; }
  .export-account > div { display: grid; min-width: 0; gap: 2px; }
  .export-account strong, .export-account span, .export-account small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .export-account strong { color: #332b3d; font-size: 12.5px; }
  .export-account span { color: #716879; font-size: 10.5px; }
  .export-account small { color: #8b8291; font-size: 9.5px; }
  .export-value { display: grid; min-width: 0; grid-template-columns: max-content minmax(0, 1fr) 30px; align-items: center; gap: 8px; }
  .export-result-modal .export-secret { grid-area: secret; }
  .export-result-modal .export-uri { grid-area: uri; }
  .export-value > strong { min-width: 45px; color: #4d4752; font-size: 11.5px; }
  .export-value code { display: block; box-sizing: border-box; width: 100%; min-width: 0; min-height: 34px; padding: 7px 9px; overflow: hidden; color: #28232e; font-family: "Cascadia Mono", "Segoe UI Mono", Consolas, monospace; font-size: 10.5px; line-height: 1.55; text-overflow: ellipsis; white-space: nowrap; background: #fff; border: 1px solid #d5d1d9; border-radius: 6px; user-select: text; }
  .export-value button { display: grid; width: 30px; height: 30px; padding: 0; place-items: center; color: #5c3e88; background: #f1ebf8; border: 1px solid #ddd0eb; border-radius: 5px; cursor: pointer; }
  .export-value button:hover, .export-value button:focus-visible { color: #402665; background: #e8ddf4; outline: 0; box-shadow: 0 0 0 2px rgb(104 69 173 / 16%); }
  .export-qr { display: grid; justify-items: center; gap: 9px; }
  .export-qr > div { display: flex; align-items: center; gap: 7px; color: #4d4752; font-size: 11.5px; }
  .export-qr img { width: min(230px, 74vw); height: min(230px, 74vw); image-rendering: pixelated; background: #fff; border: 8px solid #fff; border-radius: 6px; box-shadow: 0 0 0 1px #d7d3da; }
  .export-result-modal .export-qr { min-height: 0; grid-area: qr; grid-template-rows: auto minmax(0, 1fr); align-items: center; gap: 6px; overflow: hidden; }
  .export-result-modal .export-qr img { width: min(190px, 30vh, 64vw); height: auto; max-height: 100%; object-fit: contain; aspect-ratio: 1; }
  .export-result-modal .export-error { grid-area: error; }
  .trash-toolbar { display: flex; min-height: 42px; padding: 7px 14px; align-items: center; justify-content: space-between; gap: 10px; color: #766f7a; font-size: 10.5px; background: #f2f1f3; border-bottom: 1px solid #dedce0; }
  .trash-clear-button { display: inline-flex; min-height: 28px; padding: 4px 8px; align-items: center; gap: 5px; color: #9f2d22; background: #fff; border: 1px solid #d5c8c6; border-radius: 5px; cursor: pointer; }
  .trash-clear-button:hover, .trash-clear-button:focus-visible { color: #8f2118; background: #fff0ed; border-color: #dfaaa4; outline: 0; }
  .trash-clear-button:disabled { color: #aaa; background: #f7f7f7; border-color: #ddd; cursor: default; }
  .trash-error { display: flex; padding: 8px 10px; margin: 10px 14px 0; align-items: flex-start; gap: 7px; color: #8c1d14; font-size: 10.5px; line-height: 1.45; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .trash-error :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .trash-content { flex: 1 1 auto; min-height: 0; padding: 12px 14px 14px; overflow: auto; }
  .trash-empty { display: flex; min-height: 260px; align-items: center; justify-content: center; flex-direction: column; gap: 8px; color: #88818d; font-size: 11px; }
  .trash-empty strong { color: #514a56; font-size: 12.5px; }
  .trash-list { display: grid; gap: 7px; }
  .trash-entry { display: grid; min-height: 66px; padding: 8px 8px 8px 10px; grid-template-columns: 38px minmax(0, 1fr) auto; align-items: center; gap: 9px; background: #fff; border: 1px solid #dad7dc; border-radius: 7px; }
  .trash-entry-icon { display: grid; width: 36px; height: 36px; place-items: center; font-size: 19px; background: #f0ecf5; border-radius: 50%; }
  .trash-entry-main { display: grid; min-width: 0; gap: 1px; }
  .trash-entry-main strong, .trash-entry-main span, .trash-entry-main small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .trash-entry-main strong { color: #302c34; font-size: 12px; }
  .trash-entry-main span { color: #777079; font-size: 10px; }
  .trash-entry-main small { color: #98919a; font-size: 9px; }
  .trash-entry-actions { display: flex; gap: 3px; }
  .trash-entry-actions button { display: grid; width: 31px; height: 31px; padding: 0; place-items: center; color: #5c3e88; background: transparent; border: 0; border-radius: 5px; cursor: pointer; }
  .trash-entry-actions button:hover, .trash-entry-actions button:focus-visible { background: #eee7f7; outline: 0; }
  .trash-entry-actions button.danger { color: #ac3025; }
  .trash-entry-actions button.danger:hover, .trash-entry-actions button.danger:focus-visible { background: #fff0ed; }
  .trash-entry-actions button:disabled { color: #aaa; cursor: wait; opacity: .65; }
  .full-field { grid-column: 1 / -1; }
  .manual-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  .panel-actions, .modal-actions { flex: 0 0 auto; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  .panel-actions.centered { justify-content: center; }
  .security-note { margin-right: auto; gap: 5px; color: #6a5c77; font-size: 10.5px; }
  .drop-zone { display: flex; width: 100%; min-height: 285px; padding: 26px; align-items: center; justify-content: center; flex-direction: column; gap: 8px; color: #6d6177; background: #fff; border: 2px dashed #c6bbd4; border-radius: 9px; cursor: pointer; }
  .drop-zone strong { margin-top: 3px; color: #413849; font-size: 13px; }
  .drop-zone span { color: #8a838f; font-size: 10.5px; }
  .drop-zone:hover, .drop-zone.drag-active { color: #563881; background: #f7f2fd; border-color: #8f71bb; }
  .drop-zone:disabled { opacity: .6; cursor: wait; }
  .import-operation-error { display: flex; min-height: 36px; padding: 8px 10px; margin-bottom: 10px; align-items: flex-start; gap: 7px; color: #8c1d14; font-size: 11px; line-height: 1.45; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .import-operation-error :global(svg), .uri-preview-errors > :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .uri-preview-errors { display: grid; padding: 9px 10px; margin-top: 10px; grid-template-columns: 16px minmax(0, 1fr); align-items: start; gap: 8px; color: #794b08; font-size: 10.5px; line-height: 1.45; background: #fff8df; border: 1px solid #ead69a; border-radius: 6px; }
  .uri-preview-errors.compact { margin-top: 9px; }
  .uri-preview-errors strong { color: #684109; font-size: 11px; }
  .uri-preview-errors ul { display: grid; padding: 0; margin: 5px 0 0; gap: 4px; list-style: none; }
  .uri-preview-errors li { display: grid; min-width: 0; grid-template-columns: 54px minmax(0, 1fr); gap: 5px; }
  .uri-preview-errors li b { font-weight: 650; white-space: nowrap; }
  .uri-preview-errors li span { overflow-wrap: anywhere; }
  .preview-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 10px; }
  .preview-heading > div { display: grid; gap: 2px; }
  .preview-heading strong { color: #312d35; font-size: 13px; }
  .preview-heading span { color: #7b7580; font-size: 10.5px; }
  .text-button { padding: 5px 8px; color: #5c3e88; font-size: 11px; background: transparent; border: 0; border-radius: 4px; cursor: pointer; }
  .text-button:hover { background: #eee7f7; }
  .preview-list { display: grid; max-height: 190px; gap: 6px; overflow: auto; }
  .preview-card { display: grid; min-height: 64px; padding: 8px 10px; grid-template-columns: auto 38px minmax(0, 1fr) 20px; align-items: center; gap: 9px; background: #fff; border: 1px solid #d8d5da; border-radius: 7px; cursor: pointer; }
  .preview-card.bulk-preview-card { grid-template-columns: 38px minmax(0, 1fr) 20px; cursor: default; }
  .bulk-preview-card > :global(svg) { color: #5d3c91; }
  .preview-card.selected { background: #f8f4fc; border-color: #9d81c1; box-shadow: 0 0 0 1px rgb(112 73 170 / 10%); }
  .preview-card input { accent-color: #6845ad; }
  .preview-icon { display: grid; width: 36px; height: 36px; place-items: center; font-size: 20px; background: #eee9f4; border-radius: 50%; }
  .preview-main { display: grid; min-width: 0; gap: 1px; }
  .preview-main strong, .preview-main span, .preview-main small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .preview-main strong { color: #302c34; font-size: 12.5px; }
  .preview-main span { color: #767079; font-size: 10.5px; }
  .preview-main small { color: #918b94; font-size: 9.5px; }
  .preview-warnings { display: flex; padding: 8px 10px; margin-top: 9px; gap: 7px; color: #76520b; font-size: 10.5px; line-height: 1.45; background: #fff8df; border: 1px solid #ead69a; border-radius: 6px; }
  .preview-warnings :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .preview-warnings ul { padding: 0 0 0 15px; margin: 0; }
  .emoji-section { display: grid; padding: 11px; margin-top: 11px; gap: 7px; background: #fff; border: 1px solid #dedbe1; border-radius: 7px; }
  .emoji-section > span { color: #504b54; font-size: 11.5px; }
  .emoji-group { display: grid; grid-template-columns: 42px minmax(0, 1fr); align-items: center; gap: 5px; }
  .emoji-group small { color: #8a848d; font-size: 9.5px; }
  .emoji-row { display: flex; flex-wrap: wrap; gap: 4px; }
  .emoji-row button { display: grid; width: 30px; height: 30px; padding: 0; place-items: center; font-size: 17px; background: #f5f4f6; border: 1px solid transparent; border-radius: 5px; cursor: pointer; }
  .emoji-row button:hover { background: #eee8f5; }
  .emoji-row button.selected { background: #e9def6; border-color: #9878bf; }
  .custom-emoji { display: flex; align-items: center; gap: 8px; color: #77717c; font-size: 10px; }
  .custom-emoji input { width: 44px; height: 30px; padding: 0 6px; font-size: 17px; text-align: center; }
  .custom-emoji.wide input { width: 50px; }
  .compact-emoji { padding: 8px 9px; }
  .compact-emoji > span { font-size: 10.5px; }
  .modal-actions { display: flex; padding: 11px 15px; margin: 0; background: #fff; border-top: 1px solid #dfdde1; }
  .edit-content { display: grid; gap: 11px; }
  .edit-content .emoji-section { margin-top: 1px; }
  .toast { position: fixed; z-index: 60; right: 18px; bottom: 18px; display: flex; max-width: calc(100% - 36px); min-height: 36px; padding: 7px 12px; align-items: center; gap: 7px; color: #fff; font-size: 11.5px; background: rgb(45 39 53 / 94%); border: 1px solid rgb(255 255 255 / 14%); border-radius: 6px; box-shadow: 0 8px 24px rgb(0 0 0 / 20%); }
  .visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
  :global(.spinner) { animation: spin 850ms linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 520px) {
    .titlebar { gap: 6px; }
    .brand h1 { display: none; }
    .search-box { width: auto; flex: 1 1 auto; }
    main { padding: 10px; }
    .account-card { padding: 9px 8px 9px 4px; grid-template-columns: 22px 42px minmax(0, 1fr) 38px; gap: 7px; }
    .reorder-handle { width: 22px; }
    .account-icon { width: 40px; height: 40px; font-size: 21px; }
    .otp-code { font-size: 20px; }
    .modal-backdrop { padding: 7px; }
    .method-tabs { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .manual-grid { grid-template-columns: 1fr; }
    .full-field { grid-column: auto; }
  }
  @media (max-height: 520px) {
    .modal-backdrop { padding-block: 8px; }
    .export-result-modal .modal-header { min-height: 52px; padding-block: 6px; }
    .export-result-modal .export-content { padding: 6px 10px; gap: 4px; }
    .export-result-modal .export-warning { display: none; }
    .export-result-modal .export-account { padding-block: 3px; }
    .export-result-modal .export-account small { display: none; }
    .export-result-modal .export-value code { min-height: 28px; padding-block: 4px; }
    .export-result-modal .export-value button { width: 28px; height: 28px; }
    .export-result-modal .export-qr { gap: 0; }
    .export-result-modal .export-qr > div { display: none; }
    .export-result-modal .export-qr img { width: min(120px, 56vw); height: auto; max-height: 100%; }
    .export-result-modal .modal-actions { padding-block: 6px; }
  }
  @media (prefers-reduced-motion: reduce) { .account-card, .ring-value { transition: none; } :global(.spinner) { animation: none; } }
</style>

<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import {
    AlertTriangle,
    Check,
    Clipboard,
    Copy,
    Eye,
    EyeOff,
    ExternalLink,
    KeyRound,
    Lock,
    LockKeyhole,
    Plus,
    RefreshCw,
    ScanSearch,
    Search,
    ShieldCheck,
    Sparkles,
    Trash2,
    Unlock,
    X,
  } from "@lucide/svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import { notesApi } from "$lib/bridge";
  import {
    generatePassword,
    isInsecurePasswordUrl,
    passwordOrigin,
    passwordApi,
    secondsUntilPasswordHidden,
    type PasswordApi,
    type PasswordBrowserDiagEntry,
    type PasswordBrowserStatus,
    type PasswordEntryInput,
    type PasswordEntrySummary,
    type PasswordEntryUpdateRequest,
    type PasswordGeneratorOptions,
    type PasswordRecoveryState,
    type PasswordRevealResult,
    type PasswordStatus,
    type PasswordTemplateRecordingTicket,
  } from "$lib/passwords";

  interface Props {
    api?: PasswordApi;
  }

  type EditorMode = "add" | "edit";
  type RecoveryMode = "setup" | "reuse" | "unlock" | "change";
  const BROWSER_STATUS_POLL_MS = 10_000;

  let { api = passwordApi }: Props = $props();

  let entries = $state<PasswordEntrySummary[]>([]);
  let status = $state<PasswordStatus | null>(null);
  let browser = $state<PasswordBrowserStatus | null>(null);
  let loading = $state(true);
  let error = $state("");
  let toast = $state("");
  let searchText = $state("");
  let editorMode = $state<EditorMode | null>(null);
  let editorDialog = $state<HTMLElement | null>(null);
  let editorFirstInput = $state<HTMLInputElement | null>(null);
  let editorReturnFocus: HTMLElement | null = null;
  let pendingDelete = $state<PasswordEntrySummary | null>(null);
  let deleting = $state(false);
  let revealed = $state<Record<string, PasswordRevealResult>>({});
  let revealBusy = $state<Record<string, true>>({});
  let copyBusy = $state<Record<string, "username" | "password"> | null>(null);
  let fillBusy = $state<Record<string, true>>({});
  let templateBusy = $state<Record<string, true>>({});
  let templateRecording = $state<PasswordTemplateRecordingTicket | null>(null);
  let templateCancelBusy = $state(false);
  let lockBusy = $state(false);
  let revealTimers = new Map<string, number>();
  let countdownTimer: number | null = null;
  let browserStatusTimer: number | null = null;
  let browserStatusRefreshInFlight: Promise<void> | null = null;
  let templateExpiryTimer: number | null = null;
  let unlistenEntriesChanged: (() => void) | undefined;
  let unlistenTemplateRecording: (() => void) | undefined;
  let destroyed = false;
  let now = $state(Date.now());

  type TooltipPlacement = "top" | "bottom";
  const tooltipId = "password-manager-tooltip";
  let tooltipText = $state("");
  let tooltipOwner = $state<HTMLButtonElement | null>(null);
  let tooltipVisible = $state(false);
  let tooltipLeft = $state(0);
  let tooltipTop = $state(0);
  let tooltipPlacement = $state<TooltipPlacement>("bottom");
  let tooltipElement = $state<HTMLDivElement | null>(null);

  let siteName = $state("");
  let loginUrl = $state("");
  let username = $state("");
  let password = $state("");
  let notes = $state("");
  let allowInsecureHttp = $state(false);
  let templateId = $state<string | null>(null);
  let editingId = $state<string | null>(null);
  let editorError = $state("");
  let editorBusy = $state(false);
  let passwordVisible = $state(false);

  let generatorOpen = $state(false);
  let generatorLength = $state(20);
  let generatorOptions = $state<PasswordGeneratorOptions>({
    lowercase: true,
    uppercase: true,
    digits: true,
    symbols: true,
    excludeAmbiguous: true,
    length: 20,
  });
  let generatorBusy = $state(false);

  let recoveryMode = $state<RecoveryMode | null>(null);
  let recoveryCurrent = $state("");
  let recoveryPassword = $state("");
  let recoveryConfirm = $state("");
  let recoveryError = $state("");
  let recoveryBusy = $state(false);
  let recoveryCurrentInput = $state<HTMLInputElement | null>(null);
  let recoveryInput = $state<HTMLInputElement | null>(null);

  let filteredEntries = $derived.by(() => {
    const query = searchText.trim().toLocaleLowerCase();
    if (!query) return entries;
    return entries.filter((entry) => [entry.siteName, entry.origin, entry.username, entry.notes]
      .some((value) => value.toLocaleLowerCase().includes(query)));
  });
  let isHttpOrigin = $derived.by(() => {
    return isInsecurePasswordUrl(loginUrl);
  });
  function reasonMessage(reason: unknown, fallback: string): string {
    return reason instanceof Error && reason.message ? reason.message : fallback;
  }

  function setBusy(map: Record<string, true>, id: string, busy: boolean): Record<string, true> {
    const next = { ...map };
    if (busy) next[id] = true;
    else delete next[id];
    return next;
  }

  function showToast(message: string): void {
    toast = message;
    window.setTimeout(() => {
      if (toast === message) toast = "";
    }, 2_000);
  }

  function originFor(url: string): string {
    return passwordOrigin(url.trim());
  }

  function isHttpUrl(url: string): boolean {
    return isInsecurePasswordUrl(url);
  }

  function displayOrigin(entry: PasswordEntrySummary): string {
    return entry.origin || originFor(entry.loginUrl) || entry.loginUrl;
  }

  function shortSiteName(entry: PasswordEntrySummary): string {
    return entry.siteName.trim() || displayOrigin(entry).replace(/^https?:\/\//, "");
  }

  function clearEditorInputs(): void {
    editingId = null;
    siteName = "";
    loginUrl = "";
    username = "";
    password = "";
    notes = "";
    allowInsecureHttp = false;
    templateId = null;
    generatorOpen = false;
    passwordVisible = false;
  }

  function closeEditor(force = false): void {
    if (editorBusy && !force) return;
    editorMode = null;
    editorError = "";
    clearEditorInputs();
    void tick().then(() => editorReturnFocus?.isConnected && editorReturnFocus.focus());
  }

  function resetEditor(): void {
    editorMode = "add";
    clearEditorInputs();
    editorMode = "add";
    loginUrl = "https://";
    editorError = "";
    editorReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void tick().then(() => editorFirstInput?.focus());
  }

  function editEntry(entry: PasswordEntrySummary): void {
    editorMode = "edit";
    editingId = entry.id;
    siteName = entry.siteName;
    loginUrl = entry.loginUrl;
    username = entry.username;
    password = "";
    notes = entry.notes;
    allowInsecureHttp = entry.allowInsecureHttp;
    templateId = entry.templateId ?? null;
    editorError = "";
    passwordVisible = false;
    editorReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void tick().then(() => editorFirstInput?.focus());
  }

  function revealFor(entry: PasswordEntrySummary): PasswordRevealResult | null {
    return revealed[entry.id] ?? null;
  }

  function clearReveal(id: string): void {
    const timer = revealTimers.get(id);
    if (timer !== undefined) window.clearTimeout(timer);
    revealTimers.delete(id);
    const next = { ...revealed };
    delete next[id];
    revealed = next;
  }

  function clearAllReveals(): void {
    for (const id of Object.keys(revealed)) clearReveal(id);
    revealed = {};
  }

  function clearLockedUiState(): void {
    clearAllReveals();
    clearEditorInputs();
    clearRecoveryInputs();
    clearTemplateRecording();
    editorMode = null;
    recoveryMode = null;
    pendingDelete = null;
    entries = [];
    searchText = "";
    revealBusy = {};
    copyBusy = null;
    fillBusy = {};
    templateBusy = {};
    if (status) {
      status = {
        ...status,
        locked: true,
        recoveryState: "password-required",
        captureEnabled: false,
      };
    }
    error = "";
  }

  function tooltipButtonFromTarget(target: EventTarget | null): HTMLButtonElement | null {
    if (!(target instanceof Element)) return null;
    const button = target.closest("button.icon-button[data-tooltip]");
    return button instanceof HTMLButtonElement && button.closest(".password-tool") ? button : null;
  }

  function hideTooltip(): void {
    tooltipOwner?.removeAttribute("aria-describedby");
    tooltipOwner = null;
    tooltipText = "";
    tooltipVisible = false;
    tooltipElement = null;
  }

  async function positionTooltip(): Promise<void> {
    await tick();
    if (!tooltipVisible || !tooltipOwner || !tooltipElement) return;
    const rect = tooltipOwner.getBoundingClientRect();
    const viewportWidth = Math.max(window.innerWidth, document.documentElement.clientWidth, 1);
    const viewportHeight = Math.max(window.innerHeight, document.documentElement.clientHeight, 1);
    const margin = 8;
    const gap = 8;
    const width = Math.min(tooltipElement.offsetWidth || Math.min(220, tooltipText.length * 7 + 16), viewportWidth - margin * 2);
    const height = tooltipElement.offsetHeight || 26;
    const preferredPlacement: TooltipPlacement = tooltipOwner.closest(".header-actions")
      ? "bottom"
      : tooltipOwner.dataset.tooltipPlacement === "bottom" ? "bottom" : "top";
    let placement = preferredPlacement;
    const canFitBottom = rect.bottom + gap + height <= viewportHeight - margin;
    const canFitTop = rect.top - gap - height >= margin;
    if (placement === "bottom" && !canFitBottom && canFitTop) placement = "top";
    if (placement === "top" && !canFitTop && canFitBottom) placement = "bottom";
    const maxLeft = Math.max(margin, viewportWidth - width - margin);
    const desiredLeft = rect.left + (rect.width - width) / 2;
    const desiredTop = placement === "bottom" ? rect.bottom + gap : rect.top - height - gap;
    const maxTop = Math.max(margin, viewportHeight - height - margin);
    tooltipPlacement = placement;
    tooltipLeft = Math.round(Math.max(margin, Math.min(desiredLeft, maxLeft)));
    tooltipTop = Math.round(Math.max(margin, Math.min(desiredTop, maxTop)));
  }

  function showTooltip(button: HTMLButtonElement): void {
    const text = button.dataset.tooltip?.trim();
    if (!text) return;
    if (tooltipOwner !== button) {
      tooltipOwner?.removeAttribute("aria-describedby");
      tooltipOwner = button;
      button.setAttribute("aria-describedby", tooltipId);
    }
    tooltipText = text;
    tooltipVisible = true;
    void positionTooltip();
  }

  function handleTooltipMouseOver(event: MouseEvent): void {
    const button = tooltipButtonFromTarget(event.target);
    if (!button) return;
    const related = event.relatedTarget;
    if (related instanceof Node && button.contains(related)) return;
    showTooltip(button);
  }

  function handleTooltipMouseOut(event: MouseEvent): void {
    const button = tooltipButtonFromTarget(event.target);
    if (!button || button !== tooltipOwner) return;
    const related = event.relatedTarget;
    if (related instanceof Node && button.contains(related)) return;
    if (button.matches(":focus")) return;
    hideTooltip();
  }

  function handleTooltipFocusIn(event: FocusEvent): void {
    const button = tooltipButtonFromTarget(event.target);
    if (button) showTooltip(button);
  }

  function handleTooltipFocusOut(event: FocusEvent): void {
    const button = tooltipButtonFromTarget(event.target);
    if (button === tooltipOwner) hideTooltip();
  }

  function clearTemplateRecording(): void {
    if (templateExpiryTimer !== null) window.clearTimeout(templateExpiryTimer);
    templateExpiryTimer = null;
    templateRecording = null;
  }

  function setTemplateRecording(ticket: PasswordTemplateRecordingTicket): void {
    clearTemplateRecording();
    templateRecording = ticket;
    const remainingSeconds = secondsUntilPasswordHidden(ticket.expiresAt);
    if (remainingSeconds > 0) {
      templateExpiryTimer = window.setTimeout(() => {
        clearTemplateRecording();
        showToast("模板录制会话已结束");
        void refresh();
      }, remainingSeconds * 1_000);
    }
  }

  function revealCountdown(entry: PasswordEntrySummary): number {
    const item = revealFor(entry);
    return item ? secondsUntilPasswordHidden(item.expiresAt, now) : 0;
  }

  async function revealPassword(entry: PasswordEntrySummary): Promise<void> {
    if (revealed[entry.id]) {
      clearReveal(entry.id);
      return;
    }
    if (revealBusy[entry.id]) return;
    revealBusy = setBusy(revealBusy, entry.id, true);
    try {
      const result = await api.reveal(entry.id);
      revealed = { ...revealed, [entry.id]: result };
      const timer = window.setTimeout(() => clearReveal(entry.id), Math.max(1_000, secondsUntilPasswordHidden(result.expiresAt) * 1_000));
      revealTimers.set(entry.id, timer);
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "无法显示密码。保险库可能已锁定。");
    } finally {
      revealBusy = setBusy(revealBusy, entry.id, false);
    }
  }

  async function copyUsername(entry: PasswordEntrySummary): Promise<void> {
    copyBusy = { [entry.id]: "username" };
    try {
      await api.copyUsername(entry.id);
      showToast(`已复制“${entry.siteName}”的账号`);
    } catch (reason) {
      error = reasonMessage(reason, "复制账号失败。");
    } finally {
      copyBusy = null;
    }
  }

  async function copyPassword(entry: PasswordEntrySummary): Promise<void> {
    copyBusy = { [entry.id]: "password" };
    try {
      await api.copyPassword(entry.id);
      showToast(`已复制“${entry.siteName}”的密码`);
    } catch (reason) {
      error = reasonMessage(reason, "复制密码失败。");
    } finally {
      copyBusy = null;
    }
  }

  async function openAndFill(entry: PasswordEntrySummary): Promise<void> {
    if (isHttpUrl(entry.loginUrl) && !entry.allowInsecureHttp) {
      error = "这个 HTTP origin 尚未明确允许，请编辑账户并勾选不安全连接选项。";
      return;
    }
    if (fillBusy[entry.id]) return;
    fillBusy = setBusy(fillBusy, entry.id, true);
    if (browser?.connection === "connected"
      && (browser.capturePermission === "unavailable" || browser.capturePermission === "unknown" || !browser.capturePermission)) {
      const communicationFailed = browser.capturePermission === "unknown" || !browser.capturePermission;
      try {
        await notesApi.openExternalLink(entry.loginUrl);
        error = communicationFailed
          ? "Firefox 扩展通信异常，暂时无法确认密码权限；站点已直接打开。"
          : "当前 Firefox 扩展不支持密码填充，请更新扩展；站点已直接打开。";
      } catch (reason) {
        error = reasonMessage(reason, communicationFailed
          ? "Firefox 扩展通信异常，且无法打开站点。"
          : "当前扩展不支持密码填充，且无法打开站点。");
      } finally {
        fillBusy = setBusy(fillBusy, entry.id, false);
      }
      return;
    }
    if (browser?.connection !== "connected") {
      try {
        await notesApi.openExternalLink(entry.loginUrl);
        error = "";
        showToast(`已打开“${entry.siteName}”，可从飞花复制账号和密码`);
      } catch (reason) {
        error = reasonMessage(reason, "无法打开站点。你仍可以复制凭据后手动访问。");
      } finally {
        fillBusy = setBusy(fillBusy, entry.id, false);
      }
      return;
    }
    try {
      await api.startFill(entry.id);
      error = "";
      showToast(`已在 Firefox 中打开“${entry.siteName}”，请在页面确认填充`);
    } catch (reason) {
      await refreshBrowserStatus().catch(() => undefined);
      error = reasonMessage(reason, "无法连接 Firefox 扩展。你仍可以手动打开网址并复制凭据。");
      try {
        await notesApi.openExternalLink(entry.loginUrl);
      } catch {
        // The URL is already validated by the password store; opening is only a fallback.
      }
    } finally {
      fillBusy = setBusy(fillBusy, entry.id, false);
    }
  }

  function validateEditor(): string {
    if (!siteName.trim()) return "请输入站点名称。";
    if (!originFor(loginUrl)) return "请输入有效的 HTTP 或 HTTPS 登录地址。";
    if (!username.trim()) return "请输入用户名或账号。";
    if (!editingId && !password) return "请输入密码，或使用密码生成器生成。";
    if (isHttpOrigin && !allowInsecureHttp) return "HTTP 连接默认禁止保存，请确认你信任这个内网 origin。";
    if (password.length > 4096) return "密码不能超过 4096 个字符。";
    return "";
  }

  async function saveEntry(): Promise<void> {
    if (editorBusy) return;
    const validation = validateEditor();
    if (validation) {
      editorError = validation;
      return;
    }
    editorBusy = true;
    editorError = "";
    const base = {
      siteName: siteName.trim(),
      loginUrl: loginUrl.trim(),
      username: username.trim(),
      notes: notes.trim(),
      templateId,
      allowInsecureHttp: isHttpOrigin && allowInsecureHttp,
    };
    try {
      if (editingId) {
        const request: PasswordEntryUpdateRequest = { ...base, id: editingId, ...(password ? { password } : {}) };
        await api.update(request);
        showToast("密码账户已更新");
      } else {
        const request: PasswordEntryInput = { ...base, password };
        await api.create(request);
        showToast("密码账户已保存");
      }
      editorBusy = false;
      closeEditor(true);
      await refresh();
    } catch (reason) {
      editorError = reasonMessage(reason, "保存密码账户失败。");
    } finally {
      editorBusy = false;
    }
  }

  async function deleteEntry(): Promise<void> {
    if (!pendingDelete || deleting) return;
    deleting = true;
    try {
      await api.delete(pendingDelete.id);
      clearReveal(pendingDelete.id);
      showToast(`已删除“${pendingDelete.siteName}”`);
      pendingDelete = null;
      await refresh();
    } catch (reason) {
      error = reasonMessage(reason, "删除密码账户失败。");
    } finally {
      deleting = false;
    }
  }

  async function generateDraftPassword(): Promise<void> {
    if (generatorBusy) return;
    generatorBusy = true;
    try {
      const options = { ...generatorOptions, length: generatorLength };
      password = await api.generatePassword(options);
      generatorOpen = false;
      passwordVisible = false;
      showToast("已生成随机密码");
    } catch (reason) {
      editorError = reasonMessage(reason, "生成密码失败。");
    } finally {
      generatorBusy = false;
    }
  }

  function clearRecoveryInputs(): void {
    recoveryCurrent = "";
    recoveryPassword = "";
    recoveryConfirm = "";
    recoveryError = "";
  }

  function closeRecovery(): void {
    if (recoveryBusy) return;
    const mode = recoveryMode;
    clearRecoveryInputs();
    recoveryMode = null;
    if (mode && mode !== "change") void closeWindow();
  }

  function focusRecoveryInput(): void {
    void tick().then(() => {
      const input = recoveryMode === "change" ? recoveryCurrentInput : recoveryInput;
      input?.focus();
    });
  }

  function openRecovery(mode: RecoveryMode): void {
    recoveryMode = mode;
    clearRecoveryInputs();
    focusRecoveryInput();
  }

  function syncRequiredRecovery(next: PasswordStatus): void {
    const recoveryAvailable = next.available && next.recoveryState !== "unavailable";
    const required = recoveryAvailable
      ? next.recoveryState === "setup-required"
        ? next.sharedRecoveryConfigured ? "reuse" : "setup"
        : next.recoveryState === "password-required"
          ? "unlock"
          : null
      : null;
    if (required) {
      if (recoveryMode !== required) clearRecoveryInputs();
      recoveryMode = required;
      focusRecoveryInput();
    }
    if (!required && (recoveryMode === "setup" || recoveryMode === "reuse" || recoveryMode === "unlock")) {
      recoveryMode = null;
      clearRecoveryInputs();
    }
  }

  async function submitRecovery(): Promise<void> {
    const mode = recoveryMode;
    if (!mode || recoveryBusy) return;
    if (mode === "change" && recoveryCurrent.length < 12) {
      recoveryError = "请输入至少 12 个字符的原恢复密码。";
      focusRecoveryInput();
      return;
    }
    if (recoveryPassword.length < 12) {
      recoveryError = "恢复密码至少需要 12 个字符。";
      void tick().then(() => recoveryInput?.focus());
      return;
    }
    if (recoveryPassword.length > 256) {
      recoveryError = "恢复密码不能超过 256 个字符。";
      void tick().then(() => recoveryInput?.focus());
      return;
    }
    if ((mode === "setup" || mode === "change") && recoveryPassword !== recoveryConfirm) {
      recoveryError = "两次输入的恢复密码不一致。";
      void tick().then(() => recoveryInput?.focus());
      return;
    }
    recoveryBusy = true;
    recoveryError = "";
    try {
      if (mode === "unlock") await api.unlockWithRecoveryPassword(recoveryPassword);
      else if (mode === "change") await api.configureRecoveryPassword(recoveryPassword, recoveryCurrent);
      else await api.configureRecoveryPassword(recoveryPassword);
      clearRecoveryInputs();
      recoveryMode = null;
      await refresh();
      showToast(mode === "unlock" ? "密码保险库已解锁" : mode === "change" ? "全局恢复密码已修改" : mode === "reuse" ? "密码管理器已启用" : "全局恢复密码已设置");
    } catch (reason) {
      recoveryError = reasonMessage(reason, mode === "reuse" ? "全局恢复密码不正确，无法启用密码管理器。" : "恢复密码操作失败。");
      focusRecoveryInput();
    } finally {
      recoveryBusy = false;
    }
  }

  function refreshBrowserStatus(announceGrant = false): Promise<void> {
    if (browserStatusRefreshInFlight) return browserStatusRefreshInFlight;
    const request = (async () => {
      const previousPermission = browser?.capturePermission;
      const next = await api.getBrowserStatus();
      browser = next;
      if (next.capturePermission === "granted" && announceGrant && previousPermission && previousPermission !== "granted") {
        error = "";
        showToast(status?.captureEnabled ? "Firefox 已授权，登录信息检测已开启" : "Firefox 密码填充已授权");
      }
    })();
    browserStatusRefreshInFlight = request;
    void request.then(
      () => {
        if (browserStatusRefreshInFlight === request) browserStatusRefreshInFlight = null;
      },
      () => {
        if (browserStatusRefreshInFlight === request) browserStatusRefreshInFlight = null;
      },
    );
    return request;
  }

  function scheduleBrowserStatusRefresh(): void {
    if (destroyed || !api.isDesktop()) return;
    if (browserStatusTimer !== null) window.clearTimeout(browserStatusTimer);
    browserStatusTimer = window.setTimeout(async () => {
      browserStatusTimer = null;
      try {
        await refreshBrowserStatus(true);
      } catch {
        // The normalized desktop API normally reports communication errors as status.
      } finally {
        scheduleBrowserStatusRefresh();
      }
    }, BROWSER_STATUS_POLL_MS);
  }

  async function lockVault(): Promise<void> {
    if (lockBusy) return;
    lockBusy = true;
    try {
      await api.lock();
      clearLockedUiState();
      await refresh();
    } catch (reason) {
      error = reasonMessage(reason, "锁定密码保险库失败。");
    } finally {
      lockBusy = false;
    }
  }

  async function refresh(): Promise<void> {
    try {
      const nextStatus = await api.getStatus();
      status = nextStatus;
      syncRequiredRecovery(nextStatus);
      await refreshBrowserStatus();
      if (!nextStatus.available || nextStatus.locked || nextStatus.recoveryState !== "ready") {
        entries = [];
        clearAllReveals();
      } else {
        entries = await api.list();
        const validIds = new Set(entries.map((entry) => entry.id));
        revealed = Object.fromEntries(Object.entries(revealed).filter(([id]) => validIds.has(id)));
      }
      error = "";
    } catch (reason) {
      entries = [];
      clearAllReveals();
      error = reasonMessage(reason, "无法读取密码保险库。");
    } finally {
      loading = false;
    }
  }

  async function openInstallPage(): Promise<void> {
    const url = browser?.installUrl;
    if (!url) return;
    try {
      await notesApi.openExternalLink(url);
    } catch (reason) {
      error = reasonMessage(reason, "无法打开 Firefox 扩展安装页面。");
    }
  }

  async function startTemplateRecording(entry: PasswordEntrySummary): Promise<void> {
    if (browser?.connection !== "connected" || browser.capturePermission !== "granted" || templateRecording || templateBusy[entry.id]) return;
    templateBusy = setBusy(templateBusy, entry.id, true);
    try {
      setTemplateRecording(await api.startTemplateRecording(entry.id));
      showToast(`已在 Firefox 中打开“${entry.siteName}”的模板录制`);
      error = "";
    } catch (reason) {
      error = reasonMessage(reason, "无法开始录制站点模板。");
    } finally {
      templateBusy = setBusy(templateBusy, entry.id, false);
    }
  }

  async function cancelTemplateRecording(): Promise<void> {
    if (!templateRecording || templateCancelBusy) return;
    const sessionId = templateRecording.sessionId;
    templateCancelBusy = true;
    try {
      await api.cancelTemplateRecording(sessionId);
      clearTemplateRecording();
      showToast("模板录制已取消");
    } catch (reason) {
      error = reasonMessage(reason, "取消模板录制失败。");
    } finally {
      templateCancelBusy = false;
    }
  }

  async function closeWindow(): Promise<void> {
    clearAllReveals();
    clearEditorInputs();
    clearRecoveryInputs();
    if (templateRecording) {
      clearTemplateRecording();
    }
    if (!api.isDesktop()) {
      window.close();
      return;
    }
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

  function browserLabel(connection: PasswordBrowserStatus["connection"]): string {
    return connection === "connected"
      ? "Firefox 已连接"
      : connection === "extension-missing"
        ? "未安装扩展"
        : connection === "native-host-missing"
          ? "未注册本机桥接"
          : connection === "unsupported"
            ? "Firefox 不可用"
            : "Firefox 未连接";
  }

  function formatDiagTime(atUnixMs: number): string {
    const date = new Date(atUnixMs);
    return Number.isFinite(date.getTime()) ? date.toLocaleString("zh-CN", { hour12: false }) : "未知时间";
  }

  function capturePermissionLabel(permission: PasswordBrowserStatus["capturePermission"]): string {
    return permission === "granted" ? "已随安装授予" : permission === "unavailable" ? "当前扩展不支持" : "未知";
  }

  function lastRequestLabel(entry: PasswordBrowserDiagEntry): string {
    const outcome = entry.event === "completed" ? "成功" : "失败";
    return `${entry.event} · ${outcome} · ${formatDiagTime(entry.atUnixMs)}`;
  }

  function shortConnectionId(connectionId: string): string {
    return connectionId.length > 8 ? `${connectionId.slice(0, 8)}…` : connectionId;
  }

  async function copyDiagnostics(): Promise<void> {
    const lines = (browser?.diagnostics ?? []).map((entry) =>
      `${formatDiagTime(entry.atUnixMs)} ${entry.layer} ${entry.event} ${entry.detail}`.trim());
    try {
      await navigator.clipboard.writeText(lines.length > 0 ? lines.join("\n") : "（暂无诊断记录）");
      showToast("诊断信息已复制");
    } catch {
      showToast("复制诊断信息失败");
    }
  }

  onMount(() => {
    destroyed = false;
    document.addEventListener("mouseover", handleTooltipMouseOver);
    document.addEventListener("mouseout", handleTooltipMouseOut);
    document.addEventListener("focusin", handleTooltipFocusIn);
    document.addEventListener("focusout", handleTooltipFocusOut);
    document.addEventListener("click", hideTooltip);
    document.addEventListener("scroll", hideTooltip, true);
    window.addEventListener("resize", hideTooltip);
    window.addEventListener("blur", hideTooltip);
    void refresh();
    countdownTimer = window.setInterval(() => (now = Date.now()), 1_000);
    if (api.isDesktop()) {
      scheduleBrowserStatusRefresh();
      void import("@tauri-apps/api/event")
        .then(async ({ listen }) => {
          const cleanup = await listen<{ entryId?: string; action?: string }>("password_entries_changed", ({ payload }) => {
            if (payload.action === "template" && payload.entryId === templateRecording?.entryId) {
              clearTemplateRecording();
              showToast("站点模板已保存");
            }
            void refresh();
          });
          const templateCleanup = await listen<PasswordTemplateRecordingTicket>("password-template-recording-status", ({ payload }) => {
            if (!templateRecording || payload.sessionId !== templateRecording.sessionId) return;
            if (payload.state === "opening" || payload.state === "recording") {
              setTemplateRecording(payload);
            } else if (payload.state === "failed") {
              clearTemplateRecording();
              error = payload.message || "站点模板录制失败。";
            } else {
              clearTemplateRecording();
              showToast(payload.state === "completed" ? "站点模板已保存" : "模板录制已取消");
              if (payload.state === "completed") void refresh();
            }
          });
          if (destroyed) {
            cleanup();
            templateCleanup();
          } else {
            unlistenEntriesChanged = cleanup;
            unlistenTemplateRecording = templateCleanup;
          }
        })
        .catch(() => undefined);
    }
  });

  onDestroy(() => {
    destroyed = true;
    document.removeEventListener("mouseover", handleTooltipMouseOver);
    document.removeEventListener("mouseout", handleTooltipMouseOut);
    document.removeEventListener("focusin", handleTooltipFocusIn);
    document.removeEventListener("focusout", handleTooltipFocusOut);
    document.removeEventListener("click", hideTooltip);
    document.removeEventListener("scroll", hideTooltip, true);
    window.removeEventListener("resize", hideTooltip);
    window.removeEventListener("blur", hideTooltip);
    hideTooltip();
    unlistenEntriesChanged?.();
    unlistenEntriesChanged = undefined;
    unlistenTemplateRecording?.();
    unlistenTemplateRecording = undefined;
    if (countdownTimer !== null) window.clearInterval(countdownTimer);
    if (browserStatusTimer !== null) window.clearTimeout(browserStatusTimer);
    if (templateExpiryTimer !== null) window.clearTimeout(templateExpiryTimer);
    for (const timer of revealTimers.values()) window.clearTimeout(timer);
    revealed = {};
    clearEditorInputs();
    clearRecoveryInputs();
    if (templateRecording) void api.cancelTemplateRecording(templateRecording.sessionId).catch(() => undefined);
    templateRecording = null;
  });
</script>

<svelte:head>
  <title>密码管理器 - 飞花 - PetalDesk</title>
</svelte:head>

<main class="password-tool" data-testid="password-tool">
  <header class="password-header" data-tauri-drag-region>
    <div class="title-block" data-tauri-drag-region>
      <span class="tool-mark" aria-hidden="true"><KeyRound size={19} /></span>
      <div data-tauri-drag-region>
        <h1 data-tauri-drag-region>密码管理器</h1>
        <p data-tauri-drag-region>本机加密保存，填充前始终需要确认</p>
      </div>
    </div>
    <div class="header-actions">
      <span class:connected={browser?.connection === "connected"} class="browser-status" title={browser?.message ?? "Firefox 扩展状态"}>
        <span class="status-dot" aria-hidden="true"></span>
        {browser ? browserLabel(browser.connection) : "Firefox 状态读取中"}
      </span>
      {#if status?.recoveryState === "ready" && status.protection !== "browser-demo"}
        <button class="icon-button" type="button" data-tooltip="修改全局恢复密码" data-tooltip-placement="bottom" aria-label="修改全局恢复密码" onclick={() => openRecovery("change")}>
          <KeyRound size={16} aria-hidden="true" />
        </button>
        <button class="icon-button" type="button" data-tooltip="锁定保险库" data-tooltip-placement="bottom" aria-label="锁定保险库" disabled={lockBusy} onclick={() => void lockVault()}>
          <Lock size={16} aria-hidden="true" />
        </button>
      {/if}
      <button class="icon-button close-button" type="button" data-tooltip="关闭" data-tooltip-placement="bottom" aria-label="关闭密码管理器" onclick={() => void closeWindow()}>
        <X size={17} aria-hidden="true" />
      </button>
    </div>
  </header>

  {#if browser && browser.connection !== "connected"}
    <div class="browser-banner" role="status">
      <div class="banner-icon" aria-hidden="true"><ShieldCheck size={17} /></div>
      <div class="banner-copy">
        <strong>{browserLabel(browser.connection)}</strong>
        <span>{browser.message ?? "安装 Firefox 扩展后可使用打开并填充；未安装时仍可复制凭据。"}</span>
      </div>
      {#if browser.installUrl}
        <button type="button" class="secondary-button" onclick={() => void openInstallPage()}><ExternalLink size={14} aria-hidden="true" />安装扩展</button>
      {/if}
    </div>
  {/if}

  {#if templateRecording}
    <div class="recording-banner" role="status">
      <div class="banner-icon" aria-hidden="true"><ScanSearch size={17} /></div>
      <div class="banner-copy">
        <strong>正在录制站点模板</strong>
        <span>{entries.find((entry) => entry.id === templateRecording?.entryId)?.siteName ?? templateRecording.origin}</span>
      </div>
      <button type="button" class="secondary-button" disabled={templateCancelBusy} onclick={() => void cancelTemplateRecording()}>
        {templateCancelBusy ? "取消中…" : "取消录制"}
      </button>
    </div>
  {/if}

  {#if status?.recoveredFromBackup}
    <div class="recovery-notice" role="status">
      <AlertTriangle size={16} aria-hidden="true" />密码保险库已从最近的完整备份恢复，请检查账户是否完整。
    </div>
  {/if}

  {#if browser?.connection === "connected" && (browser.capturePermission === "unknown" || !browser.capturePermission) && status?.captureEnabled}
    <div class="consent-banner permission-banner" role="alert">
      <div class="banner-icon" aria-hidden="true"><AlertTriangle size={17} /></div>
      <div class="banner-copy">
        <strong>Firefox 扩展通信异常</strong>
        {#if !browser?.stdioConnected}
          <span>未检测到 Firefox 扩展连接，请确认扩展已安装并启用。</span>
        {:else if !browser?.pipeConnected}
          <span>Firefox 扩展已连接，桌面密码通道未建立，正在自动重试。</span>
        {:else}
          <span>暂时无法读取密码权限状态；登录信息检测暂未运行，飞花会自动重试。</span>
        {/if}
      </div>
    </div>
  {:else if browser?.connection === "connected" && browser.capturePermission === "unavailable" && status?.captureEnabled}
    <div class="consent-banner permission-banner" role="alert">
      <div class="banner-icon" aria-hidden="true"><AlertTriangle size={17} /></div>
      <div class="banner-copy">
        <strong>当前扩展不支持密码权限</strong>
        <span>请更新 Firefox 扩展；登录信息检测暂未运行。</span>
      </div>
    </div>
  {/if}

  {#if error}
    <div class="error-banner" role="alert"><AlertTriangle size={16} aria-hidden="true" /><span>{error}</span><button type="button" class="clear-error" aria-label="关闭错误提示" onclick={() => (error = "")}><X size={14} aria-hidden="true" /></button></div>
  {/if}

  {#if loading}
    <div class="loading-state" aria-busy="true"><span class="spin"><RefreshCw size={20} aria-hidden="true" /></span><span>正在打开密码保险库…</span></div>
  {:else if status?.available === false || status?.recoveryState === "unavailable"}
    <section class="locked-state"><Lock size={30} aria-hidden="true" /><h2>密码保险库不可用</h2><p>{status?.message ?? "当前平台暂不支持密码保险库，首版仅支持 Windows。"}</p></section>
  {:else if status?.locked || status?.recoveryState === "password-required"}
    <section class="locked-state" data-testid="password-locked-state">
      <LockKeyhole size={32} aria-hidden="true" />
      <h2>密码保险库已锁定</h2>
      <p>使用恢复密码解锁后，账户列表才会在本机内存中打开。</p>
      <button type="button" class="primary-button" onclick={() => openRecovery("unlock")}><Unlock size={15} aria-hidden="true" />输入恢复密码</button>
    </section>
  {:else}
    <div class="password-toolbar">
      <label class="search-box">
        <Search size={16} aria-hidden="true" />
        <span class="sr-only">搜索密码账户</span>
        <input type="search" bind:value={searchText} placeholder="搜索站点、origin 或账号" aria-label="搜索密码账户" />
        {#if searchText}<button type="button" class="clear-search" aria-label="清除搜索" onclick={() => (searchText = "")}><X size={14} aria-hidden="true" /></button>{/if}
      </label>
      <button type="button" class="primary-button add-button" onclick={resetEditor}><Plus size={15} aria-hidden="true" />添加账户</button>
    </div>

    <div class="password-meta">
      <span>{filteredEntries.length} 个账户</span>
    </div>

    {#if browser}
      <details class="diagnostics-panel" data-testid="connection-diagnostics">
        <summary>连接诊断</summary>
        <dl class="diagnostics-grid">
          <div><dt>密码权限</dt><dd>{capturePermissionLabel(browser.capturePermission)}</dd></div>
          <div><dt>Firefox 扩展连接</dt><dd>{browser.stdioConnected ? "已连接" : "未连接"}</dd></div>
          <div><dt>桌面密码通道</dt><dd>{browser.pipeConnected ? "已建立" : "未建立"}</dd></div>
          <div><dt>最近请求结果</dt><dd>{browser.lastRequestOutcome ? lastRequestLabel(browser.lastRequestOutcome) : "暂无"}</dd></div>
          <div><dt>扩展版本</dt><dd>{browser.extensionVersion ?? "未知"}</dd></div>
          <div><dt>连接 ID</dt><dd>{browser.connectionId ? shortConnectionId(browser.connectionId) : "无"}</dd></div>
        </dl>
        <button type="button" class="secondary-button" onclick={() => void copyDiagnostics()}>复制诊断信息</button>
      </details>
    {/if}

    {#if filteredEntries.length === 0}
      <section class="empty-state"><KeyRound size={30} aria-hidden="true" /><h2>{searchText ? "没有匹配的账户" : "还没有保存账户"}</h2><p>{searchText ? "换一个站点名称、origin 或用户名试试。" : "添加第一个站点账户，之后可以从 Firefox 中确认填充。"}</p>{#if !searchText}<button type="button" class="secondary-button" onclick={resetEditor}><Plus size={14} aria-hidden="true" />添加账户</button>{/if}</section>
    {:else}
      <section class="entry-list" aria-label="密码账户列表">
        {#each filteredEntries as entry (entry.id)}
          {@const revealedEntry = revealFor(entry)}
          {@const canFill = browser?.connection === "connected"}
          {@const fillPermission = browser?.capturePermission}
          {@const fillStatusUnavailable = fillPermission === "unavailable" || fillPermission === "unknown" || !fillPermission}
          {@const fillActionLabel = !canFill || fillStatusUnavailable ? "打开站点" : "打开并填充"}
          <article class="entry-row" data-testid={`password-entry-${entry.id}`}>
            <div class="entry-site-mark" aria-hidden="true">{shortSiteName(entry).slice(0, 1).toUpperCase()}</div>
            <div class="entry-main">
              <div class="entry-heading"><strong>{shortSiteName(entry)}</strong>{#if entry.templateId}<span class="template-badge">模板</span>{/if}{#if isHttpUrl(entry.loginUrl)}<span class="http-badge" title="该账户通过未加密的 HTTP 连接填充">HTTP 不安全</span>{/if}</div>
              <span class="entry-origin">{displayOrigin(entry)}</span>
              <span class="entry-username">{entry.username || "未填写用户名"}</span>
              {#if entry.notes}<span class="entry-notes">{entry.notes}</span>{/if}
            </div>
            <div class="entry-secret" class:revealed={Boolean(revealedEntry)}>
              <span>{revealedEntry?.password ?? "••••••••••••"}</span>
              {#if revealedEntry}<small>{revealCountdown(entry)} 秒后隐藏</small>{/if}
            </div>
            <div class="entry-actions">
              <button type="button" class="icon-button" data-tooltip={canFill && !fillStatusUnavailable ? (isHttpUrl(entry.loginUrl) ? "通过 HTTP 打开并填充" : "打开并填充") : fillPermission === "unknown" ? "扩展通信异常，暂时仅打开站点" : "打开站点"} aria-label={`${fillActionLabel} ${entry.siteName}`} disabled={Boolean(fillBusy[entry.id])} onclick={() => void openAndFill(entry)}><ExternalLink size={15} aria-hidden="true" /></button>
              <button type="button" class="icon-button" data-tooltip="复制用户名" aria-label={`复制 ${entry.siteName} 用户名`} disabled={copyBusy?.[entry.id] === "username"} onclick={() => void copyUsername(entry)}><Clipboard size={15} aria-hidden="true" /></button>
              <button type="button" class="icon-button" data-tooltip={revealedEntry ? "隐藏密码" : "显示密码"} aria-label={revealedEntry ? "隐藏密码" : "显示密码"} disabled={Boolean(revealBusy[entry.id])} onclick={() => void revealPassword(entry)}>{#if revealedEntry}<EyeOff size={15} aria-hidden="true" />{:else}<Eye size={15} aria-hidden="true" />{/if}</button>
              <button type="button" class="icon-button" data-tooltip="复制密码" aria-label={`复制 ${entry.siteName} 密码`} disabled={copyBusy?.[entry.id] === "password"} onclick={() => void copyPassword(entry)}><Copy size={15} aria-hidden="true" /></button>
              <button type="button" class="icon-button" data-tooltip={!canFill ? "连接 Firefox 后录制模板" : fillPermission !== "granted" ? "Firefox 密码通道就绪后可录制" : "录制站点模板"} aria-label={`录制 ${entry.siteName} 模板`} disabled={!canFill || fillPermission !== "granted" || Boolean(templateRecording) || Boolean(templateBusy[entry.id])} onclick={() => void startTemplateRecording(entry)}><ScanSearch size={15} aria-hidden="true" /></button>
              <button type="button" class="icon-button" data-tooltip="编辑" aria-label={`编辑 ${entry.siteName}`} onclick={() => editEntry(entry)}><Sparkles size={15} aria-hidden="true" /></button>
              <button type="button" class="icon-button danger-icon" data-tooltip="删除" aria-label={`删除 ${entry.siteName}`} onclick={() => (pendingDelete = entry)}><Trash2 size={15} aria-hidden="true" /></button>
            </div>
          </article>
        {/each}
      </section>
    {/if}
  {/if}
</main>

{#if tooltipVisible}
  <div
    bind:this={tooltipElement}
    id={tooltipId}
    class="floating-tooltip"
    data-placement={tooltipPlacement}
    role="tooltip"
    style={`left: ${tooltipLeft}px; top: ${tooltipTop}px;`}
  >{tooltipText}</div>
{/if}

{#if editorMode}
  <div class="modal-backdrop" role="presentation">
    <button type="button" class="modal-dismiss" aria-label="关闭编辑窗口" onclick={() => closeEditor()}></button>
    <div class="editor-dialog" bind:this={editorDialog} role="dialog" aria-modal="true" aria-labelledby="password-editor-title">
      <div class="dialog-heading"><div><h2 id="password-editor-title">{editorMode === "add" ? "添加密码账户" : "编辑密码账户"}</h2><p>只保存精确 origin，不会读取浏览器密码数据库。</p></div><button type="button" class="icon-button" aria-label="关闭" disabled={editorBusy} onclick={() => closeEditor()}><X size={17} aria-hidden="true" /></button></div>
      <form class="editor-form" onsubmit={(event) => { event.preventDefault(); void saveEntry(); }}>
        <label><span>站点名称</span><input bind:this={editorFirstInput} bind:value={siteName} maxlength="120" autocomplete="off" placeholder="例如：Google Workspace" /></label>
        <label><span>登录网址</span><input type="url" bind:value={loginUrl} maxlength="2048" autocomplete="url" placeholder="https://example.com/login" /></label>
        <div class="origin-preview">origin：{originFor(loginUrl) || "等待有效网址"}</div>
        {#if isHttpOrigin}
          <div class="http-warning"><AlertTriangle size={15} aria-hidden="true" /><span>HTTP 连接不会加密传输。只有确认这是可信内网地址时才勾选允许。</span></div>
          <label class="check-row"><input type="checkbox" bind:checked={allowInsecureHttp} /><span>允许这个 HTTP origin（仅逐站点生效）</span></label>
        {/if}
        <label><span>用户名</span><input bind:value={username} maxlength="512" autocomplete="username" placeholder="账号或邮箱" /></label>
        <label><span>密码{#if editorMode === "edit"}<small>留空表示保持原密码</small>{/if}</span><div class="password-input"><input type={passwordVisible ? "text" : "password"} bind:value={password} maxlength="4096" autocomplete={editorMode === "add" ? "new-password" : "current-password"} /><button type="button" class="icon-button" aria-label={passwordVisible ? "隐藏密码" : "显示密码"} onclick={() => (passwordVisible = !passwordVisible)}>{#if passwordVisible}<EyeOff size={15} aria-hidden="true" />{:else}<Eye size={15} aria-hidden="true" />{/if}</button></div></label>
        <div class="generator-row"><button type="button" class="secondary-button" onclick={() => (generatorOpen = !generatorOpen)}><Sparkles size={14} aria-hidden="true" />密码生成器</button>{#if password}<span class="generated-note"><Check size={13} aria-hidden="true" />已填写密码</span>{/if}</div>
        {#if generatorOpen}
          <div class="generator-panel">
            <label><span>长度 <strong>{generatorLength}</strong></span><input type="range" min="8" max="64" bind:value={generatorLength} /></label>
            <div class="generator-checks"><label><input type="checkbox" bind:checked={generatorOptions.lowercase} />小写</label><label><input type="checkbox" bind:checked={generatorOptions.uppercase} />大写</label><label><input type="checkbox" bind:checked={generatorOptions.digits} />数字</label><label><input type="checkbox" bind:checked={generatorOptions.symbols} />符号</label><label><input type="checkbox" bind:checked={generatorOptions.excludeAmbiguous} />排除易混淆字符</label></div>
            <button type="button" class="primary-button" disabled={generatorBusy} onclick={() => void generateDraftPassword()}><Sparkles size={14} aria-hidden="true" />{generatorBusy ? "生成中…" : "生成并使用"}</button>
          </div>
        {/if}
        <label><span>备注 <small>可选</small></span><textarea bind:value={notes} maxlength="1000" rows="2" placeholder="例如：工作账户、登录提示"></textarea></label>
        {#if editorError}<div class="form-error" role="alert">{editorError}</div>{/if}
        <div class="dialog-actions"><button type="button" class="secondary-button" disabled={editorBusy} onclick={() => closeEditor()}>取消</button><button type="submit" class="primary-button" disabled={editorBusy}>{editorBusy ? "保存中…" : "保存账户"}</button></div>
      </form>
    </div>
  </div>
{/if}

{#if recoveryMode}
  {@const isSettingRecovery = recoveryMode === "setup"}
  {@const isReusingRecovery = recoveryMode === "reuse"}
  {@const isUnlockingRecovery = recoveryMode === "unlock"}
  {@const isChangingRecovery = recoveryMode === "change"}
  <div class="modal-backdrop" role="presentation">
    <button type="button" class="modal-dismiss" aria-label="关闭恢复密码窗口" disabled={!isChangingRecovery || recoveryBusy} onclick={closeRecovery}></button>
    <div class="recovery-dialog" role="dialog" aria-modal="true" aria-labelledby="password-recovery-title">
      <div class="dialog-heading"><div><h2 id="password-recovery-title">{isSettingRecovery ? "设置全局恢复密码" : isReusingRecovery ? "启用密码管理器" : isUnlockingRecovery ? "解锁密码保险库" : "修改全局恢复密码"}</h2><p>{isSettingRecovery ? "设置后，密码管理器和 MFA 验证器将共同使用此密码。" : isReusingRecovery ? "请输入 MFA 验证器已有的全局恢复密码；这不会创建第二套密码，也不会修改现有密码。" : isUnlockingRecovery ? "密码管理器和 MFA 验证器共用同一个全局恢复密码。" : "修改成功后，密码管理器和 MFA 验证器将同时改用新密码，旧密码失效。"}</p></div><button type="button" class="icon-button" aria-label={isChangingRecovery ? "关闭" : "关闭密码管理器"} disabled={recoveryBusy} onclick={closeRecovery}><X size={17} aria-hidden="true" /></button></div>
      <form class="editor-form" onsubmit={(event) => { event.preventDefault(); void submitRecovery(); }}>
        {#if isChangingRecovery}<label><span>原恢复密码</span><input bind:this={recoveryCurrentInput} type="password" bind:value={recoveryCurrent} autocomplete="current-password" aria-label="原恢复密码" /></label>{/if}
        <label><span>{isChangingRecovery ? "新恢复密码" : isReusingRecovery ? "全局恢复密码" : "恢复密码"}{#if isUnlockingRecovery || isReusingRecovery}<small>不会写入页面存储</small>{/if}</span><input bind:this={recoveryInput} type="password" bind:value={recoveryPassword} autocomplete={isUnlockingRecovery || isReusingRecovery ? "current-password" : "new-password"} aria-label={isChangingRecovery ? "新恢复密码" : isReusingRecovery ? "全局恢复密码" : "恢复密码"} /></label>
        {#if isSettingRecovery || isChangingRecovery}<label><span>{isChangingRecovery ? "确认新恢复密码" : "确认恢复密码"}</span><input type="password" bind:value={recoveryConfirm} autocomplete="new-password" aria-label={isChangingRecovery ? "确认新恢复密码" : "确认恢复密码"} /></label>{/if}
        {#if !isUnlockingRecovery}
          <div class="recovery-sharing-note" role="note">
            <KeyRound size={15} aria-hidden="true" />
            <span>
              <strong>用途：</strong>这是飞花的全局恢复密码，由密码管理器和 MFA 验证器共同使用。更换电脑、重装系统、切换系统用户或本机安全保护不可用时，需要用它迁移或恢复加密保险库；导出 MFA 账户时也会用于身份验证。当前设备日常使用通常无需输入。
              <strong>{isChangingRecovery ? "请务必牢记并妥善保管新密码：" : "请务必牢记并妥善保管："}</strong>飞花不保存可供找回的恢复密码副本，也无法帮你找回。一旦遗忘，恢复密码本身无法找回；在需要它时，你将无法迁移或恢复保险库，也无法导出 MFA 账户。
            </span>
          </div>
        {/if}
        {#if recoveryError}<div class="form-error" role="alert">{recoveryError}</div>{/if}
        <div class="dialog-actions">{#if isChangingRecovery}<button type="button" class="secondary-button" disabled={recoveryBusy} onclick={closeRecovery}>取消</button>{/if}<button type="submit" class="primary-button" disabled={recoveryBusy}>{recoveryBusy ? "处理中…" : isUnlockingRecovery ? "解锁" : isChangingRecovery ? "保存新密码" : isReusingRecovery ? "启用密码管理器" : "设置恢复密码"}</button></div>
      </form>
    </div>
  </div>
{/if}

<ConfirmDialog
  open={pendingDelete !== null}
  title="删除密码账户？"
  detail={pendingDelete ? `将删除“${pendingDelete.siteName} / ${pendingDelete.username}”。此操作无法从密码管理器恢复。` : ""}
  confirmLabel="删除"
  busy={deleting}
  onconfirm={() => void deleteEntry()}
  oncancel={() => { if (!deleting) pendingDelete = null; }}
/>

{#if toast}<div class="toast" role="status">{toast}</div>{/if}

<style>
  :global(body) { overflow: hidden; }
  .password-tool { display: flex; width: 100%; height: 100%; min-width: 0; min-height: 0; flex-direction: column; color: var(--app-fg); background: var(--app-bg); }
  :global(.password-tool .icon-button[data-tooltip]::after) { display: none !important; content: none !important; }
  .floating-tooltip { position: fixed; z-index: 3000; width: max-content; max-width: min(220px, calc(100vw - 16px)); padding: 5px 8px; color: #ffffff; font-size: 12px; line-height: 1.3; white-space: nowrap; pointer-events: none; background: #252525; border-radius: 4px; box-shadow: 0 2px 8px rgb(0 0 0 / 20%); }
  .password-header { display: flex; min-height: 58px; padding: 10px 16px; align-items: center; justify-content: space-between; gap: 14px; border-bottom: 1px solid var(--app-border); background: var(--app-surface); }
  .title-block, .header-actions, .banner-copy, .password-toolbar, .password-meta, .entry-heading, .entry-actions, .generator-row, .dialog-actions { display: flex; align-items: center; }
  .title-block { min-width: 0; gap: 10px; }
  .tool-mark { display: inline-grid; width: 34px; height: 34px; color: #ffffff; place-items: center; background: #1677b9; border-radius: 7px; }
  h1, h2, p { margin: 0; }
  h1 { font-size: 16px; font-weight: 650; }
  .title-block p { margin-top: 2px; color: var(--app-muted); font-size: 11px; }
  .header-actions { min-width: 0; gap: 8px; }
  .browser-status { display: inline-flex; max-width: 180px; padding: 4px 7px; align-items: center; gap: 6px; overflow: hidden; color: var(--app-muted); font-size: 11px; white-space: nowrap; text-overflow: ellipsis; border: 1px solid var(--app-border); border-radius: 4px; }
  .browser-status.connected { color: #247b4b; border-color: #a8d5b7; background: #f0faf3; }
  .status-dot { width: 6px; height: 6px; flex: 0 0 6px; background: #9b9b9b; border-radius: 50%; }
  .browser-status.connected .status-dot { background: #2d9a58; }
  .browser-banner, .consent-banner, .recording-banner { display: flex; padding: 10px 16px; align-items: center; gap: 10px; border-bottom: 1px solid #cbdbe8; background: #eff7fc; }
  .consent-banner { border-color: #d7d3ad; background: #fffbea; }
  .recording-banner { border-color: #b9d7c4; background: #f0faf3; }
  .banner-icon { display: inline-grid; width: 28px; height: 28px; flex: 0 0 28px; place-items: center; color: #16649b; background: #d8edf9; border-radius: 5px; }
  .consent-banner .banner-icon { color: #796515; background: #f6edb7; }
  .recording-banner .banner-icon { color: #247b4b; background: #dcefe2; }
  .banner-copy { min-width: 0; flex: 1; flex-direction: column; align-items: flex-start; gap: 2px; }
  .banner-copy strong { font-size: 12px; }
  .banner-copy span { color: var(--app-muted); font-size: 11px; line-height: 1.4; }
  .error-banner, .recovery-notice { display: flex; padding: 8px 16px; align-items: center; gap: 7px; color: #a4231a; font-size: 12px; background: #fff2f0; border-bottom: 1px solid #f2c4bf; }
  .error-banner > span { min-width: 0; flex: 1; }
  .recovery-notice { color: #78520f; background: #fff8e7; border-color: #ead3a3; }
  .clear-error { display: inline-grid; width: 24px; height: 24px; flex: 0 0 24px; padding: 0; place-items: center; color: currentColor; background: transparent; border: 0; border-radius: 3px; cursor: pointer; }
  .clear-error:hover { background: rgb(0 0 0 / 6%); }
  .password-toolbar { min-height: 54px; padding: 10px 16px 8px; justify-content: space-between; gap: 10px; }
  .search-box { display: flex; min-width: 0; width: min(430px, 100%); height: 32px; padding: 0 8px; align-items: center; gap: 7px; color: var(--app-muted); background: var(--app-surface); border: 1px solid var(--app-border); border-radius: 4px; }
  .search-box:focus-within { border-color: var(--app-focus); box-shadow: 0 0 0 1px var(--app-focus); }
  .search-box input { min-width: 0; flex: 1; height: 100%; padding: 0; color: var(--app-fg); background: transparent; border: 0; outline: 0; font-size: 12px; }
  .clear-search { display: inline-grid; width: 22px; height: 22px; padding: 0; place-items: center; color: var(--app-muted); background: transparent; border: 0; cursor: pointer; }
  .primary-button, .secondary-button { display: inline-flex; min-height: 30px; padding: 5px 10px; align-items: center; justify-content: center; gap: 6px; font-size: 12px; border-radius: 4px; cursor: pointer; }
  .primary-button { color: #ffffff; background: var(--app-accent); border: 1px solid #00589f; }
  .primary-button:hover { background: #005ba9; }
  .secondary-button { color: var(--app-fg); background: var(--app-surface); border: 1px solid var(--app-border-strong); }
  .secondary-button:hover { background: var(--app-surface-hover); }
  button:disabled { cursor: not-allowed; opacity: .58; }
  .add-button { flex: 0 0 auto; }
  .password-meta { min-height: 34px; padding: 0 16px 8px; gap: 12px; color: var(--app-muted); font-size: 11px; }
  .check-row input, .generator-checks input { accent-color: var(--app-accent); }
  .diagnostics-panel { margin: 0 16px 10px; padding: 8px 10px; color: var(--app-muted); font-size: 11px; background: var(--app-surface); border: 1px solid var(--app-border); border-radius: 6px; }
  .diagnostics-panel summary { color: var(--app-fg); font-size: 12px; cursor: pointer; }
  .diagnostics-grid { display: grid; margin: 8px 0; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 6px 14px; }
  .diagnostics-grid > div { display: flex; min-width: 0; gap: 6px; }
  .diagnostics-grid dt { flex: 0 0 auto; font-weight: 600; }
  .diagnostics-grid dd { min-width: 0; margin: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .diagnostics-panel .secondary-button { min-height: 26px; }
  .entry-list { display: flex; min-height: 0; flex: 1; padding: 0 16px 18px; flex-direction: column; gap: 7px; overflow: auto; }
  .entry-row { display: grid; min-width: 0; padding: 11px 10px; grid-template-columns: 34px minmax(150px, 1fr) minmax(120px, .9fr) auto; align-items: center; gap: 10px; background: var(--app-surface); border: 1px solid var(--app-border); border-radius: 6px; }
  .entry-row:hover { border-color: var(--app-border-strong); box-shadow: 0 1px 2px rgb(0 0 0 / 6%); }
  .entry-site-mark { display: grid; width: 32px; height: 32px; color: #1b5e87; place-items: center; background: #e1f0f8; border: 1px solid #c5e0ef; border-radius: 6px; font-size: 15px; font-weight: 650; }
  .entry-main { display: flex; min-width: 0; flex-direction: column; gap: 2px; }
  .entry-heading { min-width: 0; gap: 6px; }
  .entry-heading strong { max-width: 100%; overflow: hidden; font-size: 13px; text-overflow: ellipsis; white-space: nowrap; }
  .entry-origin, .entry-username, .entry-notes { max-width: 100%; overflow: hidden; color: var(--app-muted); font-size: 11px; line-height: 1.35; text-overflow: ellipsis; white-space: nowrap; }
  .entry-username { color: #3e3e3e; }
  .entry-notes { color: #7d7d7d; }
  .template-badge, .http-badge { flex: 0 0 auto; padding: 1px 4px; color: #226d51; font-size: 9px; border: 1px solid #b7d9c8; border-radius: 3px; }
  .http-badge { color: #a1491c; border-color: #ebc1a5; background: #fff7f1; }
  .entry-secret { display: flex; min-width: 0; flex-direction: column; gap: 2px; color: #737373; font-family: Consolas, monospace; font-size: 12px; }
  .entry-secret span { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .entry-secret.revealed { color: var(--app-fg); }
  .entry-secret small { color: #8b6c29; font-family: inherit; font-size: 9px; }
  .entry-actions { justify-content: flex-end; gap: 1px; }
  .danger-icon:hover { color: var(--app-danger); }
  .close-button:hover { color: #ffffff; background: #c42b1c; }
  .empty-state, .locked-state, .loading-state { display: flex; min-height: 0; flex: 1; padding: 42px 22px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: var(--app-muted); text-align: center; }
  .empty-state :global(svg), .locked-state :global(svg) { color: #8eafc2; }
  .empty-state h2, .locked-state h2 { color: var(--app-fg); font-size: 15px; }
  .empty-state p, .locked-state p { max-width: 360px; font-size: 12px; line-height: 1.5; }
  .loading-state { gap: 8px; font-size: 12px; }
  .spin { display: inline-flex; animation: spin 850ms linear infinite; }
  .modal-backdrop { position: fixed; z-index: 1000; inset: 0; display: grid; padding: 18px; place-items: center; background: rgb(0 0 0 / 28%); }
  .modal-dismiss { position: absolute; inset: 0; width: 100%; height: 100%; padding: 0; background: transparent; border: 0; }
  .editor-dialog, .recovery-dialog { position: relative; width: min(100%, 490px); max-height: calc(100vh - 32px); padding: 18px; overflow: auto; color: var(--app-fg); background: var(--app-surface); border: 1px solid var(--app-border); border-radius: 7px; box-shadow: var(--shadow-flyout); }
  .recovery-dialog { width: min(100%, 430px); }
  .recovery-sharing-note { display: flex; padding: 9px 10px; align-items: flex-start; gap: 7px; color: #76520b; font-size: 11px; line-height: 1.45; background: #fff8df; border: 1px solid #ead69a; border-radius: 6px; }
  .recovery-sharing-note :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .dialog-heading { min-width: 0; justify-content: space-between; align-items: flex-start; gap: 12px; }
  .dialog-heading h2 { font-size: 16px; }
  .dialog-heading p { margin-top: 4px; color: var(--app-muted); font-size: 11px; line-height: 1.45; }
  .editor-form { display: flex; margin-top: 16px; flex-direction: column; gap: 11px; }
  .editor-form label { display: flex; min-width: 0; flex-direction: column; gap: 4px; color: #464646; font-size: 11px; }
  .editor-form label > span { display: flex; align-items: baseline; gap: 5px; }
  .editor-form small { color: var(--app-muted); font-size: 10px; font-weight: 400; }
  .editor-form input:not([type="checkbox"]):not([type="range"]), .editor-form textarea { width: 100%; padding: 7px 8px; color: var(--app-fg); background: #ffffff; border: 1px solid var(--app-border); border-radius: 4px; outline: 0; font-size: 12px; }
  .editor-form input:not([type="checkbox"]):not([type="range"]):focus, .editor-form textarea:focus { border-color: var(--app-focus); box-shadow: 0 0 0 1px var(--app-focus); }
  .editor-form textarea { resize: vertical; }
  .origin-preview { color: var(--app-muted); font-family: Consolas, monospace; font-size: 10px; }
  .http-warning { display: flex; padding: 7px 8px; align-items: flex-start; gap: 6px; color: #8b4d18; line-height: 1.4; background: #fff6ea; border: 1px solid #ebcda8; border-radius: 4px; }
  .check-row { display: flex !important; padding: 6px 8px; flex-direction: row !important; align-items: center; gap: 7px; color: #575757 !important; background: #fafafa; border: 1px solid var(--app-border); border-radius: 4px; }
  .password-input { display: flex; min-width: 0; align-items: center; gap: 3px; }
  .password-input input { min-width: 0; flex: 1; }
  .password-input .icon-button { flex: 0 0 30px; }
  .generator-row { min-height: 30px; gap: 8px; }
  .generated-note { display: inline-flex; align-items: center; gap: 4px; color: #27794a; font-size: 11px; }
  .generator-panel { display: flex; padding: 10px; flex-direction: column; gap: 9px; background: #f7f9fa; border: 1px solid var(--app-border); border-radius: 5px; }
  .generator-panel label { gap: 4px; }
  .generator-panel input[type="range"] { width: 100%; accent-color: var(--app-accent); }
  .generator-checks { display: flex; flex-wrap: wrap; gap: 10px; color: #555555; font-size: 11px; }
  .generator-checks label { display: inline-flex; flex-direction: row; align-items: center; gap: 4px; }
  .form-error { padding: 7px 8px; color: #a4231a; font-size: 11px; line-height: 1.4; background: #fff2f0; border: 1px solid #f2c4bf; border-radius: 4px; }
  .dialog-actions { justify-content: flex-end; gap: 7px; }
  .toast { position: fixed; z-index: 2000; right: 16px; bottom: 16px; max-width: min(360px, calc(100vw - 32px)); padding: 9px 12px; color: #ffffff; background: #292929; border-radius: 5px; box-shadow: 0 6px 20px rgb(0 0 0 / 20%); font-size: 12px; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 700px) {
    .password-header { padding-right: 10px; padding-left: 10px; }
    .password-toolbar, .password-meta, .entry-list { padding-right: 10px; padding-left: 10px; }
    .diagnostics-panel { margin-right: 10px; margin-left: 10px; }
    .entry-row { grid-template-columns: 32px minmax(0, 1fr) auto; }
    .entry-secret { grid-column: 2 / -1; grid-row: 2; }
    .entry-actions { grid-column: 2 / -1; grid-row: 3; justify-content: flex-start; }
    .browser-banner, .consent-banner, .recording-banner { padding-right: 10px; padding-left: 10px; align-items: flex-start; flex-wrap: wrap; }
  }
</style>

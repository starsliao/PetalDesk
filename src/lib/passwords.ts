import { invoke } from "@tauri-apps/api/core";

/** Recovery state of the password vault; the global recovery flag is separate. */
export type PasswordRecoveryState = "setup-required" | "ready" | "password-required" | "unavailable";
export type PasswordProtection =
  | "windows-dpapi"
  | "windows-dpapi-recovery-password"
  | "macos-keychain"
  | "macos-keychain-recovery-password"
  | "browser-demo"
  | "unavailable";

export type PasswordBrowser = "firefox";
export type PasswordBrowserConnection = "connected" | "extension-missing" | "native-host-missing" | "unsupported" | "disconnected";
export type PasswordBrowserCapturePermission = "granted" | "unavailable" | "unknown";

export const FIREFOX_EXTENSION_INSTALL_URL = "https://starsliao.github.io/PetalDesk/firefox.html";

/** Non-secret diagnostic entry reported by the desktop bridge layers. */
export interface PasswordBrowserDiagEntry {
  atUnixMs: number;
  layer: string;
  event: string;
  detail: string;
}

export interface PasswordBrowserStatus {
  browser: PasswordBrowser;
  connection: PasswordBrowserConnection;
  extensionInstalled: boolean;
  nativeHostInstalled: boolean;
  extensionVersion?: string | null;
  installUrl?: string | null;
  capturePermission?: PasswordBrowserCapturePermission;
  authenticationConsent?: boolean;
  stdioConnected?: boolean;
  pipeConnected?: boolean;
  connectionId?: string | null;
  diagnostics?: PasswordBrowserDiagEntry[];
  lastRequestOutcome?: PasswordBrowserDiagEntry | null;
  /** Backend error code preserved when the status itself had to be synthesized. */
  errorCode?: string;
  message?: string | null;
}

export interface PasswordStatus {
  available: boolean;
  locked: boolean;
  entryCount: number;
  protection: PasswordProtection;
  recoveryState: PasswordRecoveryState;
  sharedRecoveryConfigured: boolean;
  captureEnabled: boolean;
  captureConfigured: boolean;
  browser: PasswordBrowserStatus;
  sessionEpoch?: number;
  recoveredFromBackup?: boolean;
  message?: string | null;
}

/** Listing data deliberately excludes the password. */
export interface PasswordEntrySummary {
  id: string;
  siteName: string;
  loginUrl: string;
  origin: string;
  username: string;
  notes: string;
  allowInsecureHttp: boolean;
  templateId?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface PasswordEntryInput {
  siteName: string;
  loginUrl: string;
  username: string;
  password: string;
  notes?: string;
  allowInsecureHttp?: boolean;
  templateId?: string | null;
}

export interface PasswordEntryUpdateRequest extends Omit<PasswordEntryInput, "password"> {
  id: string;
  /** An empty value means keep the current password when editing. */
  password?: string;
}

export interface PasswordRevealResult {
  id: string;
  password: string;
  expiresAt: number | string;
}

export interface PasswordFillTicket {
  sessionId: string;
  entryId: string;
  browser: PasswordBrowser;
  origin: string;
  expiresAt: number | string;
}

export interface PasswordTemplateRecordingTicket {
  sessionId: string;
  entryId: string;
  origin: string;
  state: "opening" | "recording" | "completed" | "cancelled" | "failed";
  expiresAt: number | string;
  message?: string | null;
}

export interface PasswordCaptureCandidate {
  origin: string;
  username: string;
  password: string;
  allowInsecureHttp?: boolean;
}

export interface PasswordCaptureAccount {
  entryId: string;
  siteName: string;
  username: string;
}

export type PasswordCaptureDecision =
  | { action: "disabled" | "no-prompt"; entryId?: string; origin?: string; insecureHttp?: boolean }
  | { action: "create" | "update"; entryId?: string; origin?: string; insecureHttp?: boolean }
  | {
    action: "select-account" | "username-required";
    entryId?: string;
    origin?: string;
    insecureHttp?: boolean;
    accountChoices?: PasswordCaptureAccount[];
  };

export interface PasswordGeneratorOptions {
  length?: number;
  lowercase?: boolean;
  uppercase?: boolean;
  digits?: boolean;
  symbols?: boolean;
  excludeAmbiguous?: boolean;
}

export interface PasswordApi {
  isDesktop(): boolean;
  getStatus(): Promise<PasswordStatus>;
  getBrowserStatus(): Promise<PasswordBrowserStatus>;
  list(): Promise<PasswordEntrySummary[]>;
  create(request: PasswordEntryInput): Promise<PasswordEntrySummary>;
  update(request: PasswordEntryUpdateRequest): Promise<PasswordEntrySummary>;
  delete(id: string): Promise<void>;
  reveal(id: string): Promise<PasswordRevealResult>;
  copyUsername(id: string): Promise<void>;
  copyPassword(id: string): Promise<void>;
  generatePassword(options: PasswordGeneratorOptions): Promise<string>;
  startFill(id: string): Promise<PasswordFillTicket>;
  cancelFill(sessionId: string): Promise<void>;
  startTemplateRecording(id: string): Promise<PasswordTemplateRecordingTicket>;
  cancelTemplateRecording(sessionId: string): Promise<void>;
  setCaptureEnabled(enabled: boolean): Promise<PasswordStatus>;
  evaluateCapture(candidate: PasswordCaptureCandidate): Promise<PasswordCaptureDecision>;
  configureRecoveryPassword(password: string, currentPassword?: string): Promise<void>;
  unlockWithRecoveryPassword(password: string): Promise<void>;
  lock(): Promise<void>;
}

interface BackendError {
  code?: string;
  message?: string;
  layer?: string;
  requestId?: string;
  connectionId?: string;
  details?: Record<string, unknown> | null;
}

/** Error thrown by desktop password commands; keeps the backend error code when present. */
export interface PasswordCommandError extends Error {
  code?: string;
  layer?: string;
  requestId?: string;
  connectionId?: string;
}

function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function backendError(error: unknown): BackendError | null {
  if (typeof error === "object" && error) return error as BackendError;
  if (typeof error === "string") {
    try {
      return JSON.parse(error) as BackendError;
    } catch {
      return null;
    }
  }
  return null;
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) {
    return String((error as BackendError).message || "密码管理器操作失败，请稍后重试。");
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as BackendError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "密码管理器操作失败，请稍后重试。";
}

function backendErrorField(backend: BackendError, key: "layer" | "requestId" | "connectionId"): string | undefined {
  const direct = backend[key];
  if (typeof direct === "string" && direct) return direct;
  const nested = backend.details?.[key];
  return typeof nested === "string" && nested ? nested : undefined;
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    const message = errorMessage(error);
    const backend = backendError(error);
    if (!backend) throw new Error(message);
    const enriched = new Error(message) as PasswordCommandError;
    if (typeof backend.code === "string" && backend.code) enriched.code = backend.code;
    const layer = backendErrorField(backend, "layer");
    const requestId = backendErrorField(backend, "requestId");
    const connectionId = backendErrorField(backend, "connectionId");
    if (layer) enriched.layer = layer;
    if (requestId) enriched.requestId = requestId;
    if (connectionId) enriched.connectionId = connectionId;
    throw enriched;
  }
}

function nowIso(): string {
  return new Date().toISOString();
}

function validUntilMilliseconds(value: number | string): number {
  if (typeof value === "string") {
    const numeric = Number(value);
    if (Number.isFinite(numeric)) return validUntilMilliseconds(numeric);
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  if (!Number.isFinite(value)) return 0;
  return value > 0 && value < 100_000_000_000 ? value * 1_000 : value;
}

export function secondsUntilPasswordHidden(validUntil: number | string, now = Date.now()): number {
  return Math.max(0, Math.ceil((validUntilMilliseconds(validUntil) - now) / 1_000));
}

export function passwordOrigin(url: string, fallback = ""): string {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return fallback;
    return parsed.origin;
  } catch {
    return fallback;
  }
}

export function isInsecurePasswordUrl(url: string): boolean {
  try {
    return new URL(url).protocol.toLowerCase() === "http:";
  } catch {
    return false;
  }
}

function normalizedEntry(value: Partial<PasswordEntrySummary> & Record<string, unknown>): PasswordEntrySummary {
  const loginUrl = String(value.loginUrl ?? value.url ?? "");
  return {
    id: String(value.id ?? crypto.randomUUID()),
    siteName: String(value.siteName ?? value.name ?? value.title ?? (passwordOrigin(loginUrl) || "未命名网站")),
    loginUrl,
    origin: String(value.origin ?? passwordOrigin(loginUrl)),
    username: String(value.username ?? value.accountName ?? ""),
    notes: String(value.notes ?? value.note ?? ""),
    allowInsecureHttp: Boolean(value.allowInsecureHttp ?? value.allowHttp ?? value.httpAllowed ?? false),
    templateId: value.templateId == null ? null : String(value.templateId),
    createdAt: String(value.createdAt ?? nowIso()),
    updatedAt: String(value.updatedAt ?? nowIso()),
  };
}

function normalizeDiagEntry(value: unknown): PasswordBrowserDiagEntry | null {
  if (typeof value !== "object" || !value) return null;
  const record = value as Record<string, unknown>;
  const atUnixMs = Number(record.atUnixMs ?? record.at_unix_ms ?? 0);
  return {
    atUnixMs: Number.isFinite(atUnixMs) ? atUnixMs : 0,
    layer: String(record.layer ?? ""),
    event: String(record.event ?? ""),
    detail: String(record.detail ?? ""),
  };
}

function normalizeBrowser(value: Partial<PasswordBrowserStatus> & Record<string, unknown>): PasswordBrowserStatus {
  const connection = String(value.connection ?? value.status ?? "disconnected") as PasswordBrowserConnection;
  const allowed: PasswordBrowserConnection[] = ["connected", "extension-missing", "native-host-missing", "unsupported", "disconnected"];
  const normalizedConnection = allowed.includes(connection) ? connection : "disconnected";
  const capturePermissionValue = String(value.capturePermission ?? value.capture_permission ?? "");
  const capturePermission = (["granted", "unavailable", "unknown"] as const)
    .find((candidate) => candidate === capturePermissionValue);
  const normalizedCapturePermission = capturePermission
    ?? (normalizedConnection === "connected" ? "unknown" : undefined);
  const diagnostics = Array.isArray(value.diagnostics)
    ? value.diagnostics.map(normalizeDiagEntry).filter((entry): entry is PasswordBrowserDiagEntry => entry !== null)
    : [];
  const lastRequestOutcome = normalizeDiagEntry(value.lastRequestOutcome ?? value.last_request_outcome);
  const connectionId = value.connectionId ?? value.connection_id;
  return {
    browser: "firefox",
    connection: normalizedConnection,
    extensionInstalled: Boolean(value.extensionInstalled ?? value.extension_installed ?? false),
    nativeHostInstalled: Boolean(value.nativeHostInstalled ?? value.native_host_installed ?? false),
    extensionVersion: value.extensionVersion == null ? null : String(value.extensionVersion),
    installUrl: value.installUrl == null
      ? normalizedConnection === "connected" || normalizedConnection === "unsupported"
        ? null
        : FIREFOX_EXTENSION_INSTALL_URL
      : String(value.installUrl),
    ...(normalizedCapturePermission ? { capturePermission: normalizedCapturePermission } : {}),
    authenticationConsent: Boolean(value.authenticationConsent ?? value.authentication_consent ?? false),
    stdioConnected: Boolean(value.stdioConnected ?? value.stdio_connected ?? false),
    pipeConnected: Boolean(value.pipeConnected ?? value.pipe_connected ?? false),
    connectionId: connectionId == null ? null : String(connectionId),
    diagnostics,
    lastRequestOutcome,
    message: value.message == null ? null : String(value.message),
  };
}

function normalizeFillTicket(value: Partial<PasswordFillTicket> & Record<string, unknown>): PasswordFillTicket {
  return {
    sessionId: String(value.sessionId ?? value.session_id ?? ""),
    entryId: String(value.entryId ?? value.entry_id ?? ""),
    browser: "firefox",
    origin: String(value.origin ?? ""),
    expiresAt: (value.expiresAt ?? value.expires_at ?? 0) as number | string,
  };
}

function normalizeTemplateRecordingTicket(
  value: Partial<PasswordTemplateRecordingTicket> & Record<string, unknown>,
): PasswordTemplateRecordingTicket {
  const state = String(value.state ?? "opening") as PasswordTemplateRecordingTicket["state"];
  return {
    sessionId: String(value.sessionId ?? value.session_id ?? ""),
    entryId: String(value.entryId ?? value.entry_id ?? ""),
    origin: String(value.origin ?? ""),
    state: ["opening", "recording", "completed", "cancelled", "failed"].includes(state) ? state : "opening",
    expiresAt: (value.expiresAt ?? value.expires_at ?? 0) as number | string,
    ...(value.message == null ? {} : { message: String(value.message) }),
  };
}

function normalizeStatus(value: Partial<PasswordStatus> & Record<string, unknown>): PasswordStatus {
  const recovery = String(value.recoveryState ?? value.recovery_state ?? "ready") as PasswordRecoveryState;
  const allowed: PasswordRecoveryState[] = ["setup-required", "ready", "password-required", "unavailable"];
  return {
    available: Boolean(value.available ?? true),
    locked: Boolean(value.locked ?? false),
    entryCount: Math.max(0, Number(value.entryCount ?? value.entry_count ?? 0)),
    protection: (value.protection as PasswordProtection) || "unavailable",
    recoveryState: allowed.includes(recovery) ? recovery : "unavailable",
    sharedRecoveryConfigured: Boolean(
      value.sharedRecoveryConfigured ?? value.shared_recovery_configured
      ?? (recovery === "ready" || recovery === "password-required"),
    ),
    captureEnabled: Boolean(value.captureEnabled ?? value.capture_enabled ?? false),
    captureConfigured: Boolean(value.captureConfigured ?? value.capture_configured ?? false),
    browser: normalizeBrowser((value.browser ?? {}) as Record<string, unknown>),
    recoveredFromBackup: Boolean(value.recoveredFromBackup ?? value.recovered_from_backup ?? false),
    sessionEpoch: Number(value.sessionEpoch ?? value.session_epoch ?? 0) || undefined,
    message: value.message == null ? null : String(value.message),
  };
}

function randomIndex(max: number): number {
  if (max <= 1) return 0;
  if (typeof crypto !== "undefined" && crypto.getRandomValues) {
    const values = new Uint32Array(1);
    crypto.getRandomValues(values);
    return values[0] % max;
  }
  return Math.floor(Math.random() * max);
}

const LOWERCASE = "abcdefghijkmnopqrstuvwxyz";
const UPPERCASE = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGITS = "23456789";
const SYMBOLS = "!#$%&*+-=?@_";

/** Generate a strong password without relying on a persistent browser store. */
export function generatePassword(options: PasswordGeneratorOptions): string {
  const length = Math.max(8, Math.min(64, Math.trunc(Number(options.length) || 20)));
  const excludeAmbiguous = options.excludeAmbiguous !== false;
  const groups = [
    options.lowercase !== false ? (excludeAmbiguous ? LOWERCASE : "abcdefghijklmnopqrstuvwxyz") : "",
    options.uppercase !== false ? (excludeAmbiguous ? UPPERCASE : "ABCDEFGHIJKLMNOPQRSTUVWXYZ") : "",
    options.digits !== false ? (excludeAmbiguous ? DIGITS : "0123456789") : "",
    options.symbols !== false ? SYMBOLS : "",
  ].filter(Boolean);
  if (groups.length === 0) throw new Error("请至少选择一种密码字符类型。");
  const all = groups.join("");
  const result = groups.map((group) => group[randomIndex(group.length)]);
  while (result.length < length) result.push(all[randomIndex(all.length)]);
  for (let index = result.length - 1; index > 0; index -= 1) {
    const swap = randomIndex(index + 1);
    [result[index], result[swap]] = [result[swap], result[index]];
  }
  return result.join("");
}

function demoBrowserStatus(): PasswordBrowserStatus {
  return {
    browser: "firefox",
    connection: "extension-missing",
    extensionInstalled: false,
    nativeHostInstalled: false,
    installUrl: FIREFOX_EXTENSION_INSTALL_URL,
    capturePermission: "unavailable",
    message: "浏览器预览不会连接真实 Firefox 扩展。",
  };
}

/**
 * Browser-only preview API. State lives in this object for the lifetime of the
 * page and is intentionally never persisted to localStorage or IndexedDB.
 */
export function createBrowserPasswordApi(): PasswordApi {
  const timestamp = nowIso();
  let entries: PasswordEntrySummary[] = [
    {
      id: "browser-demo-google-work",
      siteName: "Google Workspace",
      loginUrl: "https://accounts.google.com/",
      origin: "https://accounts.google.com",
      username: "demo@example.com",
      notes: "办公云服务示例账户",
      allowInsecureHttp: false,
      templateId: "google",
      createdAt: timestamp,
      updatedAt: timestamp,
    },
    {
      id: "browser-demo-google-personal",
      siteName: "Google Workspace",
      loginUrl: "https://accounts.google.com/",
      origin: "https://accounts.google.com",
      username: "personal@example.com",
      notes: "同站点多账户示例",
      allowInsecureHttp: false,
      templateId: "google",
      createdAt: timestamp,
      updatedAt: timestamp,
    },
    {
      id: "browser-demo-microsoft",
      siteName: "Microsoft 工作账户",
      loginUrl: "https://login.microsoftonline.com/",
      origin: "https://login.microsoftonline.com",
      username: "demo@contoso.example",
      notes: "两步登录模板示例",
      allowInsecureHttp: false,
      templateId: "microsoft-work",
      createdAt: timestamp,
      updatedAt: timestamp,
    },
  ];
  const secrets: Record<string, string> = {
    "browser-demo-google-work": "Demo-Google-2026!",
    "browser-demo-google-personal": "Demo-Personal-2026!",
    "browser-demo-microsoft": "Demo-Microsoft-2026!",
  };
  let captureEnabled = false;
  let captureConfigured = false;

  return {
    isDesktop: () => false,
    async getStatus() {
      return {
        available: true,
        locked: false,
        entryCount: entries.length,
        protection: "browser-demo",
        recoveryState: "ready",
        sharedRecoveryConfigured: false,
        captureEnabled,
        captureConfigured,
        browser: demoBrowserStatus(),
        message: "浏览器预览使用模拟账户，数据只保存在当前页面内。",
      } satisfies PasswordStatus;
    },
    async getBrowserStatus() {
      return demoBrowserStatus();
    },
    async list() {
      return structuredClone(entries);
    },
    async create(request) {
      const loginUrl = request.loginUrl.trim();
      const saved = normalizedEntry({
        id: crypto.randomUUID(),
        ...request,
        siteName: request.siteName.trim() || passwordOrigin(loginUrl) || "未命名网站",
        loginUrl,
        origin: passwordOrigin(loginUrl),
        username: request.username.trim(),
        notes: request.notes?.trim() || "",
        allowInsecureHttp: Boolean(request.allowInsecureHttp),
        createdAt: nowIso(),
        updatedAt: nowIso(),
      });
      entries = [saved, ...entries];
      secrets[saved.id] = request.password;
      return structuredClone(saved);
    },
    async update(request) {
      const index = entries.findIndex((entry) => entry.id === request.id);
      if (index < 0) throw new Error("没有找到这个密码账户。");
      const current = entries[index];
      const loginUrl = request.loginUrl.trim();
      const saved = normalizedEntry({
        ...current,
        ...request,
        siteName: request.siteName.trim() || current.siteName,
        loginUrl,
        origin: passwordOrigin(loginUrl, current.origin),
        username: request.username.trim(),
        notes: request.notes?.trim() || "",
        allowInsecureHttp: Boolean(request.allowInsecureHttp),
        updatedAt: nowIso(),
      });
      entries[index] = saved;
      if (request.password) secrets[saved.id] = request.password;
      return structuredClone(saved);
    },
    async delete(id) {
      if (!entries.some((entry) => entry.id === id)) throw new Error("没有找到这个密码账户。");
      entries = entries.filter((entry) => entry.id !== id);
      delete secrets[id];
    },
    async reveal(id) {
      if (!entries.some((entry) => entry.id === id)) throw new Error("没有找到这个密码账户。");
      return { id, password: secrets[id] || "Demo-Password-2026!", expiresAt: Date.now() + 15_000 };
    },
    async copyUsername(id) {
      const entry = entries.find((item) => item.id === id);
      if (!entry) throw new Error("没有找到这个密码账户。");
      if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(entry.username);
    },
    async copyPassword(id) {
      if (!entries.some((entry) => entry.id === id)) throw new Error("没有找到这个密码账户。");
      if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(secrets[id] || "Demo-Password-2026!");
    },
    async generatePassword(options) {
      return generatePassword(options);
    },
    async startFill(id) {
      const entry = entries.find((item) => item.id === id);
      if (!entry) throw new Error("没有找到这个密码账户。");
      throw new Error("浏览器预览没有连接 Firefox 扩展，请在桌面客户端安装并启用扩展。");
    },
    async cancelFill() {},
    async startTemplateRecording() {
      throw new Error("浏览器预览没有连接 Firefox 扩展，无法录制站点模板。");
    },
    async cancelTemplateRecording() {},
    async setCaptureEnabled(enabled) {
      captureEnabled = enabled;
      captureConfigured = true;
      return this.getStatus();
    },
    async evaluateCapture() {
      return { action: "no-prompt" };
    },
    async configureRecoveryPassword() {},
    async unlockWithRecoveryPassword() {},
    async lock() {},
  };
}

const browserApi = createBrowserPasswordApi();

export const passwordApi: PasswordApi = {
  isDesktop: isDesktopRuntime,
  async getStatus() {
    if (!isDesktopRuntime()) return browserApi.getStatus();
    const status = await command<PasswordStatus & Record<string, unknown>>("get_password_status");
    return normalizeStatus(status);
  },
  async getBrowserStatus() {
    if (!isDesktopRuntime()) return browserApi.getBrowserStatus();
    try {
      return normalizeBrowser(await command<PasswordBrowserStatus & Record<string, unknown>>("get_password_browser_status"));
    } catch (reason) {
      const detail = reason instanceof Error && reason.message
        && reason.message !== "密码管理器操作失败，请稍后重试。" ? reason.message : "";
      const code = (reason as PasswordCommandError | null)?.code;
      return {
        browser: "firefox",
        connection: "disconnected",
        extensionInstalled: false,
        nativeHostInstalled: false,
        capturePermission: "unknown",
        ...(code ? { errorCode: code } : {}),
        message: detail
          ? `Firefox 扩展通信异常，无法读取密码权限状态（${detail}）。`
          : "Firefox 扩展通信异常，无法读取密码权限状态。",
      };
    }
  },
  async list() {
    if (!isDesktopRuntime()) return browserApi.list();
    const entries = await command<Array<Partial<PasswordEntrySummary> & Record<string, unknown>>>("list_password_entries");
    return (entries ?? []).map(normalizedEntry);
  },
  async create(request) {
    if (!isDesktopRuntime()) return browserApi.create(request);
    const result = await command<Partial<PasswordEntrySummary> & Record<string, unknown>>("create_password_entry", { input: request });
    return normalizedEntry(result);
  },
  async update(request) {
    if (!isDesktopRuntime()) return browserApi.update(request);
    const result = await command<Partial<PasswordEntrySummary> & Record<string, unknown>>("update_password_entry", { input: request });
    return normalizedEntry(result);
  },
  async delete(id) {
    if (!isDesktopRuntime()) return browserApi.delete(id);
    await command<void>("delete_password_entry", { entryId: id });
  },
  async reveal(id) {
    if (!isDesktopRuntime()) return browserApi.reveal(id);
    return command<PasswordRevealResult>("reveal_password", { entryId: id });
  },
  async copyUsername(id) {
    if (!isDesktopRuntime()) return browserApi.copyUsername(id);
    await command<void>("copy_password_username", { entryId: id });
  },
  async copyPassword(id) {
    if (!isDesktopRuntime()) return browserApi.copyPassword(id);
    await command<void>("copy_password_secret", { entryId: id });
  },
  async generatePassword(options) {
    if (!isDesktopRuntime()) return browserApi.generatePassword(options);
    const result = await command<{ password: string } | string>("generate_password", { options });
    return typeof result === "string" ? result : result.password;
  },
  async startFill(id) {
    if (!isDesktopRuntime()) return browserApi.startFill(id);
    return normalizeFillTicket(await command<Partial<PasswordFillTicket> & Record<string, unknown>>(
      "start_password_fill",
      { entryId: id },
    ));
  },
  async cancelFill(sessionId) {
    if (!isDesktopRuntime()) return browserApi.cancelFill(sessionId);
    await command<void>("cancel_password_fill", { sessionId });
  },
  async startTemplateRecording(id) {
    if (!isDesktopRuntime()) return browserApi.startTemplateRecording(id);
    const ticket = await command<Partial<PasswordTemplateRecordingTicket> & Record<string, unknown>>(
      "start_password_template_recording",
      { entryId: id },
    );
    return normalizeTemplateRecordingTicket(ticket);
  },
  async cancelTemplateRecording(sessionId) {
    if (!isDesktopRuntime()) return browserApi.cancelTemplateRecording(sessionId);
    await command<void>("cancel_password_template_recording", { sessionId });
  },
  async setCaptureEnabled(enabled) {
    if (!isDesktopRuntime()) return browserApi.setCaptureEnabled(enabled);
    const status = await command<PasswordStatus & Record<string, unknown>>("set_password_capture_enabled", { enabled });
    return normalizeStatus(status);
  },
  async evaluateCapture(candidate) {
    if (!isDesktopRuntime()) return browserApi.evaluateCapture(candidate);
    return command<PasswordCaptureDecision>("evaluate_password_capture", { candidate });
  },
  async configureRecoveryPassword(password, currentPassword) {
    if (!isDesktopRuntime()) return browserApi.configureRecoveryPassword(password, currentPassword);
    const request = currentPassword === undefined ? { password } : { password, currentPassword };
    await command<void>("configure_password_recovery_password", request);
  },
  async unlockWithRecoveryPassword(password) {
    if (!isDesktopRuntime()) return browserApi.unlockWithRecoveryPassword(password);
    await command<void>("unlock_passwords_with_recovery_password", { password });
  },
  async lock() {
    if (!isDesktopRuntime()) return browserApi.lock();
    await command<void>("lock_password_vault");
  },
};

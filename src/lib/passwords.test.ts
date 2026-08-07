import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createBrowserPasswordApi,
  generatePassword,
  passwordOrigin,
  passwordApi,
  type PasswordEntrySummary,
} from "./passwords";

const backendInvoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: backendInvoke }));

interface TauriTestWindow extends Window {
  __TAURI_INTERNALS__?: object;
}

afterEach(() => {
  backendInvoke.mockReset();
  delete (window as TauriTestWindow).__TAURI_INTERNALS__;
  vi.restoreAllMocks();
});

describe("password generator", () => {
  it("honors length and selected character classes", () => {
    const value = generatePassword({
      length: 24,
      lowercase: true,
      uppercase: true,
      digits: true,
      symbols: true,
    });
    expect(value).toHaveLength(24);
    expect(value).toMatch(/[a-z]/);
    expect(value).toMatch(/[A-Z]/);
    expect(value).toMatch(/[0-9]/);
    expect(value).toMatch(/[!#$%&*+\-=\?@_]/);
  });

  it("rejects a generator with no enabled classes", () => {
    expect(() => generatePassword({ length: 16, lowercase: false, uppercase: false, digits: false, symbols: false })).toThrow();
  });
});

describe("password URL validation", () => {
  it("accepts only exact HTTP and HTTPS origins", () => {
    expect(passwordOrigin("https://example.com/login?next=%2Fhome")).toBe("https://example.com");
    expect(passwordOrigin("http://intranet.example:8080/login")).toBe("http://intranet.example:8080");
    expect(passwordOrigin("ftp://example.com/secret")).toBe("");
    expect(passwordOrigin("javascript:alert(1)")).toBe("");
  });
});

describe("browser password preview", () => {
  it("keeps multi-account demo data in memory and never lists secrets", async () => {
    const api = createBrowserPasswordApi();
    const initial = await api.list();
    expect(initial.filter((item) => item.origin === "https://accounts.google.com")).toHaveLength(2);
    expect(initial[0]).not.toHaveProperty("password");

    const created = await api.create({
      siteName: "内网演示",
      loginUrl: "http://intranet.example/login",
      username: "worker",
      password: "secret-value",
      allowInsecureHttp: true,
      notes: "测试数据",
    });
    expect(created.allowInsecureHttp).toBe(true);
    expect((await api.reveal(created.id)).password).toBe("secret-value");
    await api.delete(created.id);
    await expect(api.reveal(created.id)).rejects.toThrow();
  });

  it("updates an account without replacing its password when the edit is blank", async () => {
    const api = createBrowserPasswordApi();
    const entry = (await api.list())[0];
    await api.update({
      id: entry.id,
      siteName: entry.siteName,
      loginUrl: entry.loginUrl,
      username: "renamed@example.com",
      notes: entry.notes,
      allowInsecureHttp: entry.allowInsecureHttp,
    });
    expect((await api.list()).find((item) => item.id === entry.id)?.username).toBe("renamed@example.com");
    expect((await api.reveal(entry.id)).password).toBe("Demo-Google-2026!");
  });

  it("preserves, sets, and clears MFA links without exposing MFA secrets", async () => {
    const api = createBrowserPasswordApi();
    const entry = (await api.list())[0];
    expect(entry.mfaLink).toEqual({ entryId: "browser-demo-mfa-work", allowedOrigins: [] });
    const candidates = await api.listMfaCandidates();
    expect(candidates[0]).toEqual({
      id: "browser-demo-mfa-work",
      name: "Google Workspace",
      issuer: "Google",
      accountName: "demo@example.com",
    });
    expect(candidates[0]).not.toHaveProperty("secret");
    expect(candidates[0]).not.toHaveProperty("code");

    await api.update({
      id: entry.id,
      siteName: entry.siteName,
      loginUrl: entry.loginUrl,
      username: entry.username,
    });
    expect((await api.list()).find((item) => item.id === entry.id)?.mfaLink).toEqual(entry.mfaLink);

    await api.update({
      id: entry.id,
      siteName: entry.siteName,
      loginUrl: entry.loginUrl,
      username: entry.username,
      mfaLink: null,
    });
    expect((await api.list()).find((item) => item.id === entry.id)?.mfaLink).toBeNull();
  });
});

describe("desktop password command contract", () => {
  it("lists only normalized public MFA association metadata", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValueOnce([{
      id: "legacy-id",
      name: "Work",
      issuer: "Example",
      account_name: "alice@example.com",
      secret: "must-not-pass-through",
      code: "123456",
    }]);

    await expect(passwordApi.listMfaCandidates()).resolves.toEqual([{
      id: "legacy-id",
      name: "Work",
      issuer: "Example",
      accountName: "alice@example.com",
    }]);
    expect(backendInvoke).toHaveBeenCalledWith("list_password_mfa_candidates", undefined);
  });

  it("normalizes the shared recovery flag from desktop status payloads", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValueOnce({
      available: true,
      locked: false,
      entry_count: 0,
      protection: "windows-dpapi",
      recovery_state: "setup-required",
      shared_recovery_configured: true,
      capture_enabled: false,
      capture_configured: false,
      browser: {
        browser: "firefox",
        connection: "disconnected",
        extension_installed: false,
        native_host_installed: false,
      },
    });

    await expect(passwordApi.getStatus()).resolves.toMatchObject({
      recoveryState: "setup-required",
      sharedRecoveryConfigured: true,
    });
  });

  it("uses direct Rust request payloads and exact command names", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    const entry: PasswordEntrySummary = {
      id: "entry-1",
      siteName: "Example",
      loginUrl: "https://example.com/login",
      origin: "https://example.com",
      username: "person",
      notes: "",
      allowInsecureHttp: false,
      templateId: null,
      createdAt: "2026-08-04T00:00:00Z",
      updatedAt: "2026-08-04T00:00:00Z",
    };
    backendInvoke
      .mockResolvedValueOnce(entry)
      .mockResolvedValueOnce({ id: entry.id, password: "secret", expiresAt: Date.now() + 15_000 })
      .mockResolvedValueOnce({ password: "generated" })
      .mockResolvedValue(undefined);

    await passwordApi.create({ siteName: "Example", loginUrl: entry.loginUrl, username: "person", password: "secret" });
    await passwordApi.reveal(entry.id);
    await passwordApi.generatePassword({ length: 18, symbols: true });
    await passwordApi.unlockWithRecoveryPassword("correct horse battery");

    expect(backendInvoke).toHaveBeenNthCalledWith(1, "create_password_entry", {
      input: {
        siteName: "Example",
        loginUrl: entry.loginUrl,
        username: "person",
        password: "secret",
      },
    });
    expect(backendInvoke).toHaveBeenNthCalledWith(2, "reveal_password", { entryId: entry.id });
    expect(backendInvoke).toHaveBeenNthCalledWith(3, "generate_password", { options: { length: 18, symbols: true } });
    expect(backendInvoke).toHaveBeenNthCalledWith(4, "unlock_passwords_with_recovery_password", { password: "correct horse battery" });
  });

  it("normalizes template recording tickets and uses the matching cancel command", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke
      .mockResolvedValueOnce({
        session_id: "recording-1",
        entry_id: "entry-1",
        origin: "https://example.com",
        state: "recording",
        expires_at: 1_800_000_000_000,
      })
      .mockResolvedValueOnce(undefined);

    await expect(passwordApi.startTemplateRecording("entry-1")).resolves.toEqual({
      sessionId: "recording-1",
      entryId: "entry-1",
      origin: "https://example.com",
      state: "recording",
      expiresAt: 1_800_000_000_000,
    });
    await passwordApi.cancelTemplateRecording("recording-1");

    expect(backendInvoke).toHaveBeenNthCalledWith(1, "start_password_template_recording", { entryId: "entry-1" });
    expect(backendInvoke).toHaveBeenNthCalledWith(2, "cancel_password_template_recording", { sessionId: "recording-1" });
  });

  it("normalizes fill tickets without any extra authorization step", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue({
      sessionId: "fill-1",
      entryId: "entry-1",
      browser: "firefox",
      origin: "https://example.com",
      expiresAt: 1_800_000_000_000,
    });

    const ticket = await passwordApi.startFill("entry-1");
    expect(ticket).toEqual({
      sessionId: "fill-1",
      entryId: "entry-1",
      browser: "firefox",
      origin: "https://example.com",
      expiresAt: 1_800_000_000_000,
    });
    expect(ticket).not.toHaveProperty("actionRequired");
    expect(backendInvoke).toHaveBeenCalledWith("start_password_fill", { entryId: "entry-1" });
  });

  it("keeps the backend error code and routing fields on command failures", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockRejectedValue({
      code: "password_fill_start_failed",
      message: "Firefox 扩展尚未连接。",
      layer: "request",
      requestId: "request-1",
      connectionId: "connection-a",
    });

    const failure = await passwordApi.startFill("entry-1").catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(Error);
    expect((failure as Error).message).toBe("Firefox 扩展尚未连接。");
    expect(failure).toMatchObject({
      code: "password_fill_start_failed",
      layer: "request",
      requestId: "request-1",
      connectionId: "connection-a",
    });
  });

  it("provides the Firefox install page when a disconnected backend omits it", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue({
      browser: "firefox",
      connection: "disconnected",
      extensionInstalled: false,
      nativeHostInstalled: true,
      installUrl: null,
    });

    await expect(passwordApi.getBrowserStatus()).resolves.toMatchObject({
      connection: "disconnected",
      installUrl: "https://starsliao.github.io/PetalDesk/firefox.html",
    });
  });

  it("normalizes a connected browser with no permission result as unknown", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue({
      browser: "firefox",
      connection: "connected",
      extensionInstalled: true,
      nativeHostInstalled: true,
    });

    await expect(passwordApi.getBrowserStatus()).resolves.toMatchObject({
      connection: "connected",
      capturePermission: "unknown",
    });
  });

  it("normalizes layered connection and diagnostic fields from the backend", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockResolvedValue({
      browser: "firefox",
      connection: "connected",
      extension_installed: true,
      native_host_installed: true,
      extension_version: "1.2.0",
      capture_permission: "granted",
      authentication_consent: true,
      stdio_connected: true,
      pipe_connected: false,
      connection_id: "connection-a",
      diagnostics: [
        { at_unix_ms: 1_800_000_000_000, layer: "stdio", event: "connected", detail: "connection-a" },
        { atUnixMs: 1_800_000_001_000, layer: "request", event: "timeout", detail: "command=password.getStatus requestId=abc12345" },
      ],
      last_request_outcome: {
        at_unix_ms: 1_800_000_001_000,
        layer: "request",
        event: "failed",
        detail: "command=password.getStatus requestId=abc12345 outcome=timeout",
      },
    });

    await expect(passwordApi.getBrowserStatus()).resolves.toMatchObject({
      connection: "connected",
      capturePermission: "granted",
      authenticationConsent: true,
      stdioConnected: true,
      pipeConnected: false,
      connectionId: "connection-a",
      diagnostics: [
        { atUnixMs: 1_800_000_000_000, layer: "stdio", event: "connected", detail: "connection-a" },
        { atUnixMs: 1_800_000_001_000, layer: "request", event: "timeout", detail: "command=password.getStatus requestId=abc12345" },
      ],
      lastRequestOutcome: {
        atUnixMs: 1_800_000_001_000,
        layer: "request",
        event: "failed",
        detail: "command=password.getStatus requestId=abc12345 outcome=timeout",
      },
    });
  });

  it("reports desktop browser-status command failures as communication errors", async () => {
    (window as TauriTestWindow).__TAURI_INTERNALS__ = {};
    backendInvoke.mockRejectedValue({
      code: "password_browser_status_failed",
      message: "native bridge timed out",
    });

    await expect(passwordApi.getBrowserStatus()).resolves.toMatchObject({
      capturePermission: "unknown",
      errorCode: "password_browser_status_failed",
      message: expect.stringContaining("通信异常"),
    });
    await expect(passwordApi.getBrowserStatus()).resolves.toMatchObject({
      message: expect.stringContaining("native bridge timed out"),
    });
  });
});

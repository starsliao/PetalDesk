import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createBrowserMfaApi,
  formatOtpCode,
  maskedOtpCode,
  secondsUntil,
  validUntilMilliseconds,
} from "./mfa";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("MFA presentation helpers", () => {
  it("normalizes backend expiry timestamps and computes a bounded countdown", () => {
    expect(validUntilMilliseconds(1_800_000_000)).toBe(1_800_000_000_000);
    expect(validUntilMilliseconds(1_800_000_000_000)).toBe(1_800_000_000_000);
    expect(validUntilMilliseconds("2026-08-01T12:00:30.000Z")).toBe(Date.parse("2026-08-01T12:00:30.000Z"));
    expect(secondsUntil(1_800_000_030_000, 1_800_000_000_000)).toBe(30);
    expect(secondsUntil(1_800_000_000_000, 1_800_000_001_000)).toBe(0);
  });

  it("groups visible and hidden codes without exposing placeholder digits", () => {
    expect(formatOtpCode("123456", 6)).toBe("123 456");
    expect(formatOtpCode("1234567", 7)).toBe("123 4567");
    expect(formatOtpCode("12345678", 8)).toBe("1234 5678");
    expect(maskedOtpCode(6)).toBe("••• •••");
    expect(maskedOtpCode(8)).toBe("•••• ••••");
  });
});

describe("browser MFA demonstration", () => {
  it("uses only simulated in-memory records and never writes a supplied secret to storage", async () => {
    const write = vi.spyOn(Storage.prototype, "setItem");
    const api = createBrowserMfaApi();
    const secret = "JBSWY3DPEHPK3PXP";

    const previews = await api.previewManual({
      name: "Private account",
      issuer: "Example",
      accountName: "person@example.com",
      secret,
      iconEmoji: "🔐",
      algorithm: "sha1",
      digits: 6,
      period: 30,
    });
    const saved = await api.commitImport(previews[0].sessionId, "🛡️");
    const entries = await api.list();
    const revealed = await api.reveal(saved.id);

    expect(entries.find((entry) => entry.id === saved.id)).toMatchObject({
      name: "Private account",
      issuer: "Example",
      accountName: "person@example.com",
    });
    expect(JSON.stringify(entries)).not.toContain(secret);
    expect(revealed.code).toMatch(/^\d{6}$/);
    expect(write).not.toHaveBeenCalled();
  });

  it("rejects non-TOTP and migration links in the browser demo", async () => {
    const api = createBrowserMfaApi();
    await expect(api.previewUri("otpauth://hotp/Example:person?secret=AAAA")).rejects.toThrow("只支持标准");
    await expect(api.previewUri("otpauth-migration://offline?data=AAAA")).rejects.toThrow("只支持标准");
  });
});

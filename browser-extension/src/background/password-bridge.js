(function exposePasswordBridge(root) {
  "use strict";

  const templates = root.PetalDeskPasswordTemplates;
  if (!templates) {
    throw new Error("PetalDesk password templates were not loaded");
  }

  const CONTENT_MESSAGE_TYPE = "petaldesk.password.command";
  const CONTENT_MESSAGE_PREFIX = "petaldesk.password.";
  const POPUP_MESSAGE_PREFIX = "petaldesk.popup.";
  const SESSION_TTL_MS = 5 * 60 * 1_000;
  const RECORDING_TTL_MS = 5 * 60 * 1_000;
  const CANDIDATE_TTL_MS = 30_000;
  const USERNAME_STAGE_TTL_MS = 2 * 60 * 1_000;
  const BADGE_ACCOUNT_LIMIT = 16;
  const CAPABILITY_GROUPS = Object.freeze(["password-fill", "password-capture"]);
  const COMMANDS = new Set([
    "password.open",
    "password.offerFill",
    "password.offerFillDirect",
    "password.provideCredentials",
    "password.cancelFill",
    "password.requestConsent",
    "password.setCaptureEnabled",
    "password.captureMatch",
    "password.saveResult",
    "password.resolveCapture",
    "password.startTemplateRecording",
    "password.cancelTemplateRecording",
    "password.getStatus",
    "password.updateBadge",
  ]);
  const SAVE_ACTIONS = new Set(["new", "update", "replace", "ignore"]);

  function bridgeError(code, message) {
    const error = new Error(message);
    error.code = code;
    return error;
  }

  function requiredString(value, name, maxLength = 256) {
    const normalized = String(value == null ? "" : value).trim();
    if (!normalized || normalized.length > maxLength) {
      throw bridgeError("PASSWORD_PROTOCOL_INVALID", `${name} is required`);
    }
    return normalized;
  }

  function optionalRoutingId(value, name) {
    if (value == null) return null;
    const parsed = Number(value);
    if (!Number.isInteger(parsed) || parsed < 0) {
      throw bridgeError("PASSWORD_PROTOCOL_INVALID", `${name} must be a non-negative integer`);
    }
    return parsed;
  }

  function secureRandomId(prefix) {
    if (root.crypto && typeof root.crypto.randomUUID === "function") {
      return `${prefix}-${root.crypto.randomUUID()}`;
    }
    const bytes = new Uint8Array(16);
    if (root.crypto && typeof root.crypto.getRandomValues === "function") {
      root.crypto.getRandomValues(bytes);
      return `${prefix}-${Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("")}`;
    }
    throw bridgeError("PASSWORD_RANDOM_UNAVAILABLE", "Secure random values are unavailable");
  }

  function senderBinding(sender) {
    const tabId = sender && sender.tab ? optionalRoutingId(sender.tab.id, "tabId") : null;
    const frameId = optionalRoutingId(sender && sender.frameId, "frameId") ?? 0;
    if (tabId == null) {
      throw bridgeError("PASSWORD_TARGET_INVALID", "The message is not associated with a browser tab");
    }
    const senderUrl = String(sender.url || sender.tab && sender.tab.url || "");
    return {
      documentId: sender.documentId ? String(sender.documentId) : null,
      frameId,
      origin: templates.exactOrigin(senderUrl),
      tabId,
    };
  }

  function createPasswordBridge({ api, postToNative, protocolVersion }) {
    if (!api || api.browserFamily !== "firefox") {
      throw new Error("The password bridge is only available in Firefox v1");
    }
    const sessions = new Map();
    const candidates = new Map();
    const captureUsernames = new Map();
    const recordings = new Map();
    const tabContexts = new Map();
    const tabAccounts = new Map();
    const diagnostics = {
      lastCommandAt: null,
      lastCommandErrorCode: null,
      lastCommandOk: null,
      nativeConnected: false,
      secretConnected: false,
    };
    let captureEnabled = false;
    let captureInsecureOrigins = new Set();

    function setBadgeText(tabId, text) {
      if (!api.action || typeof api.action.setBadgeText !== "function") return;
      try {
        const result = api.action.setBadgeText({ tabId, text });
        if (result && typeof result.catch === "function") void result.catch(() => {});
      } catch {
        // Badge updates are best-effort and must not interrupt command handling.
      }
    }

    function clearTabAccounts(tabId) {
      tabAccounts.delete(tabId);
      setBadgeText(tabId, "");
    }

    function postEvent(event, payload) {
      const result = postToNative({
        protocolVersion,
        type: "extension.event",
        event,
        payload,
      });
      return result !== false;
    }

    function clearSession(sessionId) {
      const session = sessions.get(sessionId);
      if (!session) return;
      if (session.timer != null) clearTimeout(session.timer);
      sessions.delete(sessionId);
    }

    function renewSession(session) {
      if (session.timer != null) clearTimeout(session.timer);
      session.expiresAt = Date.now() + SESSION_TTL_MS;
      session.timer = setTimeout(() => {
        if (sessions.get(session.sessionId) !== session) return;
        clearSession(session.sessionId);
        postEvent("fillResult", {
          sessionId: session.sessionId,
          tabId: session.tabId,
          frameId: session.frameId,
          origin: session.origin,
          status: "expired",
          submitted: false,
        });
      }, SESSION_TTL_MS);
      if (session.timer && typeof session.timer.unref === "function") session.timer.unref();
    }

    function clearCandidate(candidateId) {
      const record = candidates.get(candidateId);
      if (!record) return;
      if (record.timer != null) clearTimeout(record.timer);
      record.password = "";
      record.username = "";
      candidates.delete(candidateId);
    }

    function renewCandidate(record, ttlMs = CANDIDATE_TTL_MS) {
      if (!record || !record.candidateId) return;
      if (record.timer != null) clearTimeout(record.timer);
      record.expiresAt = Date.now() + ttlMs;
      record.timer = setTimeout(() => {
        if (candidates.get(record.candidateId) !== record) return;
        clearCandidate(record.candidateId);
      }, ttlMs);
      if (record.timer && typeof record.timer.unref === "function") record.timer.unref();
    }

    function usernameStageKey(tabId, origin) {
      return `${tabId}:${origin}`;
    }

    function clearUsernameStage(key) {
      const stage = captureUsernames.get(key);
      if (!stage) return;
      if (stage.timer != null) clearTimeout(stage.timer);
      stage.username = "";
      captureUsernames.delete(key);
    }

    function clearRecording(sessionId) {
      const recording = recordings.get(sessionId);
      if (!recording) return;
      if (recording.timer != null) clearTimeout(recording.timer);
      recordings.delete(sessionId);
    }

    function clearCaptureState() {
      for (const candidateId of Array.from(candidates.keys())) clearCandidate(candidateId);
      for (const key of Array.from(captureUsernames.keys())) clearUsernameStage(key);
      for (const sessionId of Array.from(recordings.keys())) clearRecording(sessionId);
    }

    function clearTabState(tabId, documentId = null, { preserveUsernameStage = false } = {}) {
      for (const [sessionId, session] of sessions.entries()) {
        if (session.tabId !== tabId) continue;
        if (documentId && session.documentId && session.documentId !== documentId) continue;
        clearSession(sessionId);
      }
      for (const [candidateId, record] of candidates.entries()) {
        if (record.tabId !== tabId) continue;
        if (documentId && record.documentId && record.documentId !== documentId) continue;
        clearCandidate(candidateId);
      }
      for (const [sessionId, recording] of recordings.entries()) {
        if (recording.tabId !== tabId) continue;
        if (documentId && recording.documentId && recording.documentId !== documentId) continue;
        clearRecording(sessionId);
      }
      if (!preserveUsernameStage) {
        const prefix = `${tabId}:`;
        for (const key of Array.from(captureUsernames.keys())) {
          if (key.startsWith(prefix)) clearUsernameStage(key);
        }
      }
    }

    function onPageClosed(sender) {
      const tabId = optionalRoutingId(sender && sender.tab && sender.tab.id, "tabId");
      const documentId = sender && sender.documentId ? String(sender.documentId) : null;
      clearTabState(tabId, documentId, { preserveUsernameStage: true });
      postEvent("pageClosed", { documentId, tabId });
      return { cleared: true, tabId, documentId };
    }

    function recordingResult(recording, status, extra = {}) {
      postEvent("templateRecordingResult", {
        documentId: recording.documentId,
        entryId: recording.entryId,
        frameId: recording.frameId,
        origin: recording.origin,
        sessionId: recording.sessionId,
        status,
        tabId: recording.tabId,
        ...extra,
      });
    }

    function renewRecording(recording) {
      if (recording.timer != null) clearTimeout(recording.timer);
      recording.expiresAt = Date.now() + RECORDING_TTL_MS;
      recording.timer = setTimeout(() => {
        if (recordings.get(recording.sessionId) !== recording) return;
        recordingResult(recording, "failed", {
          error: {
            code: "PASSWORD_TEMPLATE_RECORDING_EXPIRED",
            message: "The template recording session expired",
          },
        });
        clearRecording(recording.sessionId);
      }, RECORDING_TTL_MS);
      if (recording.timer && typeof recording.timer.unref === "function") recording.timer.unref();
    }

    function normalizeInsecureOrigins(value) {
      if (!Array.isArray(value)) return new Set();
      const origins = value.map((item) => templates.exactOrigin(item));
      if (origins.some((origin) => !origin.startsWith("http://"))) {
        throw bridgeError(
          "PASSWORD_PROTOCOL_INVALID",
          "insecureOrigins may contain only explicitly allowed HTTP origins",
        );
      }
      return new Set(origins);
    }

    function sessionForPayload(payload) {
      const sessionId = requiredString(payload.sessionId, "sessionId", 160);
      const session = sessions.get(sessionId);
      if (!session || session.expiresAt <= Date.now()) {
        clearSession(sessionId);
        throw bridgeError("PASSWORD_SESSION_EXPIRED", "The password fill session has expired");
      }
      return session;
    }

    async function validateLiveTab(session) {
      const tab = await api.getTab(session.tabId);
      const origin = templates.exactOrigin(tab && tab.url);
      if (origin !== session.origin || !session.allowedOrigins.has(origin)) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The browser tab origin changed");
      }
      return origin;
    }

    async function sendContent(session, command, payload) {
      await validateLiveTab(session);
      const response = await api.sendTabMessage(
        session.tabId,
        { type: CONTENT_MESSAGE_TYPE, command, payload },
        { frameId: session.frameId },
      );
      if (!response || response.ok !== true) {
        throw bridgeError(
          "PASSWORD_CONTENT_FAILED",
          response && response.error
            ? response.error.message || String(response.error)
            : "The password content script did not respond",
        );
      }
      return response.result || {};
    }

    async function openPasswordTab(payload) {
      const sessionId = requiredString(payload.sessionId, "sessionId", 160);
      if (sessions.has(sessionId)) {
        throw bridgeError("PASSWORD_SESSION_EXISTS", "The password fill session already exists");
      }
      const url = new URL(requiredString(payload.url, "url", 4_096));
      const origin = templates.exactOrigin(payload.origin || url.href);
      if (url.origin !== origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login URL and origin do not match");
      }
      const allowInsecureHttp = payload.allowInsecureHttp === true;
      if (origin.startsWith("http://") && !allowInsecureHttp) {
        throw bridgeError(
          "PASSWORD_INSECURE_ORIGIN",
          "HTTP password filling requires an explicit per-origin opt-in",
        );
      }
      const allowedOrigins = new Set([origin]);
      if (Array.isArray(payload.allowedOrigins)) {
        for (const value of payload.allowedOrigins) {
          const allowed = templates.exactOrigin(value);
          if (allowed.startsWith("http://") && !allowInsecureHttp) {
            throw bridgeError("PASSWORD_INSECURE_ORIGIN", "An HTTP redirect origin was not allowed");
          }
          allowedOrigins.add(allowed);
        }
      }
      const tab = await api.createTab({ active: true, url: url.href });
      if (!tab || !Number.isInteger(tab.id)) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "Firefox did not create a login tab");
      }
      const session = {
        allowInsecureHttp,
        allowedOrigins,
        documentId: null,
        entryId: requiredString(payload.entryId, "entryId", 160),
        expiresAt: 0,
        frameId: 0,
        offerId: null,
        origin,
        sessionId,
        state: "opening",
        tabId: tab.id,
        timer: null,
      };
      sessions.set(sessionId, session);
      renewSession(session);
      return {
        actionRequired: null,
        authenticationConsent: true,
        sessionId,
        tabId: tab.id,
        frameId: 0,
        origin,
        state: "opening",
      };
    }

    function rejectFillSecrets(payload) {
      if (
        Object.prototype.hasOwnProperty.call(payload, "password")
        || Object.prototype.hasOwnProperty.call(payload, "secret")
        || Object.prototype.hasOwnProperty.call(payload, "credentials")
      ) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "A fill offer cannot contain a password");
      }
    }

    async function dispatchFillOffer(session, payload) {
      if (session.state !== "ready") {
        throw bridgeError("PASSWORD_SESSION_STATE", `The fill session is ${session.state}`);
      }
      const requestedTabId = optionalRoutingId(payload.tabId, "tabId") ?? session.tabId;
      const requestedFrameId = optionalRoutingId(payload.frameId, "frameId") ?? session.frameId;
      const origin = templates.exactOrigin(payload.origin);
      if (
        requestedTabId !== session.tabId
        || requestedFrameId !== session.frameId
        || origin !== session.origin
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The fill offer target does not match its session");
      }
      const offerId = requiredString(payload.offerId || secureRandomId("offer"), "offerId", 160);
      const username = String(payload.username == null ? "" : payload.username);
      if (username.length > 1_024) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "username is too long");
      }
      const result = await sendContent(session, "fillOffer", {
        allowInsecureHttp: session.allowInsecureHttp,
        entryId: session.entryId,
        offerId,
        origin,
        sessionId: session.sessionId,
        userTemplate: payload.userTemplate || null,
        username,
      });
      session.offerId = offerId;
      session.state = "awaiting-confirmation";
      renewSession(session);
      return { ...result, tabId: session.tabId, frameId: session.frameId };
    }

    async function offerFill(payload) {
      rejectFillSecrets(payload);
      const session = sessionForPayload(payload);
      return dispatchFillOffer(session, payload);
    }

    async function offerFillDirect(payload) {
      rejectFillSecrets(payload);
      const sessionId = requiredString(payload.sessionId, "sessionId", 160);
      if (sessions.has(sessionId)) {
        throw bridgeError("PASSWORD_SESSION_EXISTS", "The password fill session already exists");
      }
      const tabId = optionalRoutingId(payload.tabId, "tabId");
      if (tabId == null) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "A fill target tab is required");
      }
      const frameId = optionalRoutingId(payload.frameId, "frameId") ?? 0;
      const origin = templates.exactOrigin(payload.origin);
      const allowInsecureHttp = payload.allowInsecureHttp === true;
      if (origin.startsWith("http://") && !allowInsecureHttp) {
        throw bridgeError(
          "PASSWORD_INSECURE_ORIGIN",
          "HTTP password filling requires an explicit per-origin opt-in",
        );
      }
      const tab = await api.getTab(tabId).catch(() => null);
      let tabOrigin = "";
      try {
        tabOrigin = templates.exactOrigin(tab && tab.url);
      } catch (_error) {
        tabOrigin = "";
      }
      if (tabOrigin !== origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The fill target tab is not on the requested origin");
      }
      const session = {
        allowInsecureHttp,
        allowedOrigins: new Set([origin]),
        documentId: payload.documentId ? String(payload.documentId) : null,
        entryId: requiredString(payload.entryId, "entryId", 160),
        expiresAt: 0,
        frameId,
        offerId: null,
        origin,
        sessionId,
        state: "ready",
        tabId,
        timer: null,
      };
      sessions.set(sessionId, session);
      renewSession(session);
      return dispatchFillOffer(session, payload);
    }

    async function provideCredentials(payload) {
      const session = sessionForPayload(payload);
      if (session.state !== "confirmed") {
        throw bridgeError("PASSWORD_SESSION_STATE", "The user has not confirmed this fill request");
      }
      const offerId = requiredString(payload.offerId, "offerId", 160);
      if (offerId !== session.offerId) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The fill offer does not match its session");
      }
      const origin = templates.exactOrigin(payload.origin);
      if (origin !== session.origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The credential origin does not match the page");
      }
      let password = String(payload.password == null ? "" : payload.password);
      let username = payload.username == null ? null : String(payload.username);
      if (!password || password.length > 4_096 || username && username.length > 1_024) {
        password = "";
        username = null;
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The supplied credentials are invalid");
      }
      let result;
      try {
        result = await sendContent(session, "fillSecret", {
          offerId,
          origin,
          password,
          sessionId: session.sessionId,
          username,
        });
      } finally {
        password = "";
        username = null;
        payload.password = "";
        if (Object.prototype.hasOwnProperty.call(payload, "username")) payload.username = "";
      }
      postEvent("fillResult", {
        ...result,
        frameId: session.frameId,
        origin: session.origin,
        sessionId: session.sessionId,
        status: "filled",
        submitted: false,
        tabId: session.tabId,
      });
      if (result.needsNextStep) {
        session.offerId = null;
        session.state = "ready";
        renewSession(session);
      } else {
        clearSession(session.sessionId);
      }
      return { ...result, tabId: session.tabId, frameId: session.frameId };
    }

    async function cancelFill(payload) {
      const session = sessionForPayload(payload);
      try {
        await sendContent(session, "fillCancel", {
          offerId: session.offerId,
          origin: session.origin,
          sessionId: session.sessionId,
        });
      } catch (_error) {
        // Navigation can remove the content script; cancellation still clears the session.
      }
      postEvent("fillResult", {
        sessionId: session.sessionId,
        tabId: session.tabId,
        frameId: session.frameId,
        origin: session.origin,
        status: "cancelled",
        submitted: false,
      });
      clearSession(session.sessionId);
      return { cancelled: true };
    }

    async function startTemplateRecording(payload) {
      const sessionId = requiredString(payload.sessionId || payload.recordingId, "sessionId", 160);
      if (recordings.has(sessionId)) {
        throw bridgeError(
          "PASSWORD_TEMPLATE_RECORDING_EXISTS",
          "The template recording session already exists",
        );
      }
      const entryId = requiredString(payload.entryId, "entryId", 160);
      const url = new URL(requiredString(payload.url, "url", 4_096));
      const origin = templates.exactOrigin(payload.origin || url.href);
      if (url.origin !== origin) {
        throw bridgeError(
          "PASSWORD_ORIGIN_MISMATCH",
          "The template recording URL and origin do not match",
        );
      }
      const allowInsecureHttp = payload.allowInsecureHttp === true;
      if (origin.startsWith("http://") && !allowInsecureHttp) {
        throw bridgeError(
          "PASSWORD_INSECURE_ORIGIN",
          "HTTP template recording requires an explicit per-origin opt-in",
        );
      }
      const tab = await api.createTab({ active: true, url: url.href });
      if (!tab || !Number.isInteger(tab.id)) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "Firefox did not create a template recording tab");
      }
      const recording = {
        allowInsecureHttp,
        documentId: null,
        entryId,
        expiresAt: 0,
        frameId: 0,
        origin,
        passwordDocumentId: null,
        passwordSelectors: [],
        sessionId,
        state: "opening",
        tabId: tab.id,
        timer: null,
        usernameDocumentId: null,
        usernameSelectors: [],
      };
      recordings.set(sessionId, recording);
      renewRecording(recording);
      return {
        entryId,
        frameId: 0,
        origin,
        sessionId,
        state: "opening",
        tabId: tab.id,
      };
    }

    async function cancelTemplateRecording(payload) {
      const sessionId = requiredString(payload.sessionId || payload.recordingId, "sessionId", 160);
      const recording = recordings.get(sessionId);
      if (!recording) {
        return { cancelled: false, sessionId };
      }
      await api.sendTabMessage(
        recording.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "templateRecordCancel",
          payload: { origin: recording.origin, sessionId },
        },
        { frameId: recording.frameId },
      ).catch(() => {});
      postEvent("templateRecordingCancelled", {
        documentId: recording.documentId,
        entryId: recording.entryId,
        origin: recording.origin,
        sessionId,
        tabId: recording.tabId,
      });
      clearRecording(sessionId);
      return { cancelled: true, sessionId };
    }

    async function broadcastCaptureState() {
      const tabs = await api.queryAllTabs({});
      await Promise.all((tabs || []).map(async (tab) => {
        if (!tab || !Number.isInteger(tab.id) || !tab.url) return;
        let origin;
        try {
          origin = templates.exactOrigin(tab.url);
        } catch (_error) {
          return;
        }
        const allowed = origin.startsWith("https://") || captureInsecureOrigins.has(origin);
        const command = captureEnabled && allowed ? "captureEnable" : "captureDisable";
        await api.sendTabMessage(
          tab.id,
          {
            type: CONTENT_MESSAGE_TYPE,
            command,
            payload: { insecureOrigins: Array.from(captureInsecureOrigins) },
          },
          { frameId: 0 },
        ).catch(() => {});
      }));
    }

    async function setCaptureEnabled(payload) {
      const enabled = payload.enabled === true;
      captureInsecureOrigins = normalizeInsecureOrigins(payload.insecureOrigins);
      captureEnabled = enabled;
      if (!enabled) clearCaptureState();
      await broadcastCaptureState();
      return {
        enabled: captureEnabled,
        insecureOrigins: Array.from(captureInsecureOrigins),
      };
    }

    async function requestConsent() {
      // Authentication access is now a required install-time permission, so the
      // desktop's legacy consent probe always reports an already granted state.
      return { actionRequired: null, granted: true, userGestureRequired: false };
    }

    async function updateBadge(payload) {
      const tabId = optionalRoutingId(payload.tabId, "tabId");
      if (tabId == null) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "A badge target tab is required");
      }
      let origin = "";
      if (payload.origin != null && String(payload.origin).trim() !== "") {
        origin = templates.exactOrigin(payload.origin);
      }
      const locked = payload.locked === true;
      const accounts = normalizeAccountChoices(payload.accounts, BADGE_ACCOUNT_LIMIT);
      tabAccounts.set(tabId, { accounts, locked, origin });
      setBadgeText(tabId, locked || accounts.length === 0 ? "" : String(accounts.length));
      return { applied: true, tabId };
    }

    async function captureMatch(payload) {
      const candidateId = requiredString(payload.candidateId, "candidateId", 160);
      const record = candidates.get(candidateId);
      if (!record || record.expiresAt <= Date.now()) {
        clearCandidate(candidateId);
        throw bridgeError("PASSWORD_CANDIDATE_EXPIRED", "The login candidate has expired");
      }
      const action = String(payload.action || "");
      if (!new Set(["new", "update", "same", "select", "username-required", "locked"]).has(action)) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The capture match action is invalid");
      }
      if (!record.promoted) {
        throw bridgeError("PASSWORD_CANDIDATE_PENDING", "The login candidate is still awaiting a success signal");
      }
      record.matchedAction = action;
      record.accountChoices = normalizeAccountChoices(payload.accounts);
      await api.sendTabMessage(
        record.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "captureMatch",
          payload: {
            action,
            candidateId,
            confidence: record.confidence,
            expiresInMs: Math.max(0, record.expiresAt - Date.now()),
            origin: record.origin,
            username: record.username,
            accounts: record.accountChoices,
          },
        },
        { frameId: record.frameId },
      ).catch(() => {});
      if (action === "same" || action === "username-required" || action === "locked") {
        clearCandidate(candidateId);
      }
      return { action, candidateId };
    }

    function normalizeAccountChoices(value, limit = 32) {
      if (!Array.isArray(value)) return [];
      return value.slice(0, limit).flatMap((item) => {
        if (!item || typeof item !== "object") return [];
        const entryId = String(item.entryId == null ? "" : item.entryId).trim();
        const username = String(item.username == null ? "" : item.username);
        const siteName = String(item.siteName == null ? "" : item.siteName);
        if (!entryId || entryId.length > 160 || username.length > 1_024 || siteName.length > 512) {
          return [];
        }
        return [{ entryId, username, siteName }];
      });
    }

    function resolveCapture(payload) {
      const candidateId = requiredString(payload.candidateId, "candidateId", 160);
      const existed = candidates.has(candidateId);
      clearCandidate(candidateId);
      return { candidateId, cleared: existed };
    }

    async function getStatus() {
      return {
        authenticationConsent: true,
        consentActionRequired: null,
        consentArmed: false,
        captureEnabled,
        pendingCandidates: candidates.size,
        pendingUsernameStages: captureUsernames.size,
        pendingFillSessions: sessions.size,
        pendingTemplateRecordings: recordings.size,
      };
    }

    function routeCommand(request) {
      const command = String(request.command || "");
      const payload = request.payload && typeof request.payload === "object" ? request.payload : {};
      switch (command) {
        case "password.open": return openPasswordTab(payload);
        case "password.offerFill": return offerFill(payload);
        case "password.offerFillDirect": return offerFillDirect(payload);
        case "password.provideCredentials": return provideCredentials(payload);
        case "password.cancelFill": return cancelFill(payload);
        case "password.requestConsent": return requestConsent();
        case "password.setCaptureEnabled": return setCaptureEnabled(payload);
        case "password.captureMatch": return captureMatch(payload);
        case "password.saveResult": return saveResult(payload);
        case "password.resolveCapture": return resolveCapture(payload);
        case "password.startTemplateRecording": return startTemplateRecording(payload);
        case "password.cancelTemplateRecording": return cancelTemplateRecording(payload);
        case "password.getStatus": return getStatus();
        case "password.updateBadge": return updateBadge(payload);
        default:
          throw bridgeError("PASSWORD_COMMAND_UNSUPPORTED", `Unsupported password command: ${command || "<empty>"}`);
      }
    }

    async function route(request) {
      diagnostics.lastCommandAt = Date.now();
      try {
        const result = await routeCommand(request);
        diagnostics.lastCommandOk = true;
        diagnostics.lastCommandErrorCode = null;
        return result;
      } catch (error) {
        diagnostics.lastCommandOk = false;
        diagnostics.lastCommandErrorCode = error && error.code
          ? String(error.code)
          : "PASSWORD_COMMAND_FAILED";
        throw error;
      }
    }

    function promoteCandidate(record, confidence, promptBinding) {
      if (record.promoted) return;
      record.promoted = true;
      record.confidence = confidence === "high" ? "high" : "low";
      record.promptBinding = promptBinding || {
        documentId: record.documentId,
        frameId: record.frameId,
        origin: record.origin,
        tabId: record.tabId,
      };
      const nativeCandidate = {
        candidateId: record.candidateId,
        confidence: record.confidence,
        documentId: record.promptBinding.documentId,
        frameId: record.promptBinding.frameId,
        origin: record.origin,
        password: record.password,
        source: record.source,
        stage: record.stage,
        tabId: record.promptBinding.tabId,
        username: record.username,
      };
      try {
        postEvent("captureCandidate", nativeCandidate);
      } finally {
        nativeCandidate.password = "";
        nativeCandidate.username = "";
        record.password = "";
      }
    }

    function recordingForContent(message, sender) {
      const sessionId = requiredString(message.sessionId || message.recordingId, "sessionId", 160);
      const recording = recordings.get(sessionId);
      if (!recording || recording.expiresAt <= Date.now()) {
        clearRecording(sessionId);
        throw bridgeError(
          "PASSWORD_TEMPLATE_RECORDING_EXPIRED",
          "The template recording session has expired",
        );
      }
      const binding = senderBinding(sender);
      const origin = templates.exactOrigin(message.origin);
      if (
        binding.tabId !== recording.tabId
        || binding.frameId !== recording.frameId
        || binding.origin !== recording.origin
        || origin !== recording.origin
        || recording.documentId && binding.documentId !== recording.documentId
      ) {
        throw bridgeError(
          "PASSWORD_TARGET_MISMATCH",
          "The template recording message came from another page",
        );
      }
      return { binding, recording };
    }

    function onTemplateRecordingProgress(message, sender) {
      const { binding, recording } = recordingForContent(message, sender);
      const field = String(message.field || "");
      if (field !== "username" && field !== "password") {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The recorded field type is invalid");
      }
      let selector;
      try {
        selector = templates.normalizeRecordedSelector(message.selector);
      } catch (error) {
        throw bridgeError(
          "PASSWORD_TEMPLATE_SELECTOR_INVALID",
          error instanceof Error ? error.message : String(error),
        );
      }
      recording[`${field}Selectors`] = [selector];
      recording[`${field}DocumentId`] = binding.documentId;
      recording.documentId = binding.documentId;
      recording.state = "recording";
      renewRecording(recording);
      postEvent("templateRecordingProgress", {
        entryId: recording.entryId,
        field,
        origin: recording.origin,
        sessionId: recording.sessionId,
        tabId: recording.tabId,
      });
      if (recording.usernameSelectors.length === 0 || recording.passwordSelectors.length === 0) {
        return {
          completed: false,
          field,
          sessionId: recording.sessionId,
        };
      }

      const mode = recording.usernameDocumentId
        && recording.passwordDocumentId
        && recording.usernameDocumentId !== recording.passwordDocumentId
        ? "two-step"
        : "password";
      const normalized = templates.normalizeUserTemplate({
        id: `recorded-${recording.entryId}`,
        label: "用户录制模板",
        mode,
        origin: recording.origin,
        passwordSelectors: recording.passwordSelectors,
        usernameSelectors: recording.usernameSelectors,
        version: 1,
      }, recording.origin);
      const template = {
        id: normalized.id,
        label: normalized.label,
        mode: normalized.mode,
        origin: recording.origin,
        passwordSelectors: Array.from(normalized.passwordSelectors),
        usernameSelectors: Array.from(normalized.usernameSelectors),
        version: normalized.version,
      };
      recording.state = "completed";
      recordingResult(recording, "completed", { template });
      clearRecording(recording.sessionId);
      return { completed: true, sessionId: recording.sessionId, template };
    }

    function onTemplateRecordingCancelled(message, sender) {
      const { recording } = recordingForContent(message, sender);
      postEvent("templateRecordingCancelled", {
        documentId: recording.documentId,
        entryId: recording.entryId,
        origin: recording.origin,
        sessionId: recording.sessionId,
        tabId: recording.tabId,
      });
      clearRecording(sessionId);
      return { cancelled: true, sessionId: recording.sessionId };
    }

    async function onCaptureSuccess(message, sender) {
      const binding = senderBinding(sender);
      const candidateId = requiredString(message.candidateId, "candidateId", 160);
      const record = candidates.get(candidateId);
      if (!record || record.expiresAt <= Date.now()) {
        clearCandidate(candidateId);
        throw bridgeError("PASSWORD_CANDIDATE_EXPIRED", "The login candidate has expired");
      }
      if (
        binding.tabId !== record.tabId
        || binding.frameId !== record.frameId
        || binding.origin !== record.origin
        || record.documentId && binding.documentId && binding.documentId !== record.documentId
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The success signal came from another page");
      }
      promoteCandidate(record, message.confidence === "high" ? "high" : "low", binding);
      return { promoted: true, candidateId };
    }

    async function onTabReady(message, sender) {
      const binding = senderBinding(sender);
      let announcedOrigin = "";
      try {
        announcedOrigin = templates.exactOrigin(message.origin);
      } catch (_error) {
        announcedOrigin = "";
      }
      if (binding.frameId === 0 && !announcedOrigin) {
        // A top-level page without a valid HTTP(S) origin has no accounts; drop
        // stale badge state and report the tab as having no active origin.
        tabContexts.set(binding.tabId, { documentId: binding.documentId, origin: "" });
        clearTabAccounts(binding.tabId);
        postEvent("originActive", { origin: "", tabId: binding.tabId });
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The ready page did not match its browser frame");
      }
      if (announcedOrigin !== binding.origin || binding.frameId !== 0) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The ready page did not match its browser frame");
      }
      tabContexts.set(binding.tabId, { documentId: binding.documentId, origin: binding.origin });
      postEvent("originActive", { origin: binding.origin, tabId: binding.tabId });
      const cachedAccounts = tabAccounts.get(binding.tabId);
      if (cachedAccounts && cachedAccounts.origin !== binding.origin) {
        // The desktop pushes badge accounts per origin; a navigation makes any
        // previously cached list stale until the next password.updateBadge.
        clearTabAccounts(binding.tabId);
      }
      for (const recording of Array.from(recordings.values())) {
        if (recording.tabId !== binding.tabId) continue;
        recording.frameId = binding.frameId;
        if (binding.origin !== recording.origin) {
          recordingResult(recording, "failed", {
            error: {
              code: "PASSWORD_ORIGIN_MISMATCH",
              message: "The template recording tab left its exact origin",
            },
          });
          clearRecording(recording.sessionId);
          continue;
        }
        recording.documentId = binding.documentId;
        const response = await api.sendTabMessage(
          recording.tabId,
          {
            type: CONTENT_MESSAGE_TYPE,
            command: "templateRecordStart",
            payload: {
              allowInsecureHttp: recording.allowInsecureHttp,
              origin: recording.origin,
              passwordSelectors: recording.passwordSelectors,
              sessionId: recording.sessionId,
              usernameSelectors: recording.usernameSelectors,
            },
          },
          { frameId: recording.frameId },
        ).catch((error) => ({
          ok: false,
          error: { message: error instanceof Error ? error.message : String(error) },
        }));
        if (!response || response.ok !== true) {
          recordingResult(recording, "failed", {
            error: {
              code: "PASSWORD_CONTENT_FAILED",
              message: response && response.error
                ? response.error.message || String(response.error)
                : "The template recording content script did not respond",
            },
          });
          clearRecording(recording.sessionId);
          continue;
        }
        recording.state = "recording";
        renewRecording(recording);
        postEvent("templateRecordingReady", {
          documentId: binding.documentId,
          entryId: recording.entryId,
          frameId: binding.frameId,
          origin: recording.origin,
          sessionId: recording.sessionId,
          state: "recording",
          tabId: binding.tabId,
        });
      }
      for (const session of sessions.values()) {
        if (session.tabId !== binding.tabId) continue;
        if (!session.allowedOrigins.has(binding.origin)) {
          postEvent("fillResult", {
            sessionId: session.sessionId,
            tabId: session.tabId,
            frameId: binding.frameId,
            origin: binding.origin,
            status: "origin-rejected",
            submitted: false,
          });
          clearSession(session.sessionId);
          continue;
        }
        if (
          session.documentId
          && binding.documentId
          && session.documentId !== binding.documentId
          && ["awaiting-confirmation", "confirmed"].includes(session.state)
        ) {
          session.offerId = null;
        }
        session.documentId = binding.documentId;
        session.frameId = binding.frameId;
        session.origin = binding.origin;
        session.state = "ready";
        renewSession(session);
        postEvent("tabReady", {
          documentId: binding.documentId,
          entryId: session.entryId,
          frameId: binding.frameId,
          origin: binding.origin,
          sessionId: session.sessionId,
          tabId: binding.tabId,
        });
      }
      for (const record of candidates.values()) {
        if (
          record.promoted
          || record.tabId !== binding.tabId
          || record.frameId !== binding.frameId
          || record.origin !== binding.origin
          || record.documentId && binding.documentId && record.documentId === binding.documentId
        ) continue;
        // A navigation after a submitted form is useful evidence, but not a
        // definitive success signal. The page prompt remains low confidence.
        promoteCandidate(record, "low", binding);
      }
      const originCaptureAllowed = binding.origin.startsWith("https://")
        || captureInsecureOrigins.has(binding.origin);
      return {
        captureEnabled: Boolean(captureEnabled && originCaptureAllowed),
        insecureOrigins: Array.from(captureInsecureOrigins),
      };
    }

    function sessionForContent(message, sender) {
      const session = sessionForPayload(message);
      const binding = senderBinding(sender);
      const offerId = requiredString(message.offerId, "offerId", 160);
      const origin = templates.exactOrigin(message.origin);
      if (
        binding.tabId !== session.tabId
        || binding.frameId !== session.frameId
        || binding.origin !== session.origin
        || origin !== session.origin
        || offerId !== session.offerId
        || session.documentId && binding.documentId && binding.documentId !== session.documentId
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The page message does not match its fill session");
      }
      return { binding, session };
    }

    function onFillConfirm(message, sender) {
      const { binding, session } = sessionForContent(message, sender);
      if (session.state !== "awaiting-confirmation") {
        throw bridgeError("PASSWORD_SESSION_STATE", "The fill offer is not awaiting confirmation");
      }
      session.state = "confirmed";
      renewSession(session);
      postEvent("fillConfirm", {
        documentId: binding.documentId,
        entryId: session.entryId,
        frameId: binding.frameId,
        offerId: session.offerId,
        origin: session.origin,
        sessionId: session.sessionId,
        tabId: session.tabId,
      });
      return { confirmed: true };
    }

    function onFillCancel(message, sender) {
      const { session } = sessionForContent(message, sender);
      postEvent("fillResult", {
        frameId: session.frameId,
        origin: session.origin,
        sessionId: session.sessionId,
        status: "cancelled",
        submitted: false,
        tabId: session.tabId,
      });
      clearSession(session.sessionId);
      return { cancelled: true };
    }

    async function onCaptureUsernameStage(message, sender) {
      if (!captureEnabled) {
        throw bridgeError("PASSWORD_CAPTURE_DISABLED", "Login detection is disabled");
      }
      const binding = senderBinding(sender);
      if (binding.frameId !== 0) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "Cross-frame login detection is not supported");
      }
      const origin = templates.exactOrigin(message.origin);
      if (origin !== binding.origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The username-stage origin is invalid");
      }
      if (origin.startsWith("http://") && !captureInsecureOrigins.has(origin)) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "HTTP login detection was not enabled for this origin");
      }
      let username = String(message.username == null ? "" : message.username);
      if (!username || username.length > 1_024) {
        username = "";
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The username stage is invalid");
      }
      const key = usernameStageKey(binding.tabId, origin);
      clearUsernameStage(key);
      const stage = {
        expiresAt: Date.now() + USERNAME_STAGE_TTL_MS,
        timer: null,
        username,
      };
      stage.timer = setTimeout(() => clearUsernameStage(key), USERNAME_STAGE_TTL_MS);
      if (stage.timer && typeof stage.timer.unref === "function") stage.timer.unref();
      captureUsernames.set(key, stage);
      username = "";
      message.username = "";
      return { accepted: true, expiresInMs: USERNAME_STAGE_TTL_MS };
    }

    async function onCaptureSubmitted(message, sender) {
      if (!captureEnabled) {
        throw bridgeError("PASSWORD_CAPTURE_DISABLED", "Login detection is disabled");
      }
      const binding = senderBinding(sender);
      const value = message.candidate && typeof message.candidate === "object"
        ? message.candidate
        : {};
      const candidateId = requiredString(value.candidateId, "candidateId", 160);
      const origin = templates.exactOrigin(value.origin);
      if (origin !== binding.origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login candidate origin is invalid");
      }
      if (origin.startsWith("http://") && !captureInsecureOrigins.has(origin)) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "HTTP login detection was not enabled for this origin");
      }
      let username = String(value.username == null ? "" : value.username);
      let password = String(value.password == null ? "" : value.password);
      const stagedKey = usernameStageKey(binding.tabId, origin);
      const staged = captureUsernames.get(stagedKey);
      if (!username && staged && staged.expiresAt > Date.now()) {
        username = staged.username;
      }
      clearUsernameStage(stagedKey);
      if (!password || password.length > 4_096 || username.length > 1_024) {
        password = "";
        username = "";
        value.password = "";
        value.username = "";
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The login candidate is invalid");
      }
      clearCandidate(candidateId);
      const record = {
        candidateId,
        documentId: binding.documentId,
        expiresAt: Date.now() + CANDIDATE_TTL_MS,
        frameId: binding.frameId,
        origin,
        password,
        promoted: false,
        promptBinding: null,
        matchedAction: null,
        accountChoices: [],
        savePending: false,
        confidence: "low",
        source: String(value.source || "generic"),
        stage: String(value.stage || "single-page"),
        tabId: binding.tabId,
        timer: null,
        username,
      };
      candidates.set(candidateId, record);
      renewCandidate(record);
      password = "";
      username = "";
      value.password = "";
      return { accepted: true, candidateId, expiresInMs: CANDIDATE_TTL_MS };
    }

    function onSaveDecision(message, sender) {
      const candidateId = requiredString(message.candidateId, "candidateId", 160);
      const record = candidates.get(candidateId);
      if (!record || record.expiresAt <= Date.now()) {
        clearCandidate(candidateId);
        throw bridgeError("PASSWORD_CANDIDATE_EXPIRED", "The login candidate has expired");
      }
      const binding = senderBinding(sender);
      const promptBinding = record.promptBinding || {
        documentId: record.documentId,
        frameId: record.frameId,
        origin: record.origin,
        tabId: record.tabId,
      };
      if (
        !record.promoted
        || binding.tabId !== promptBinding.tabId
        || binding.frameId !== promptBinding.frameId
        || binding.origin !== promptBinding.origin
        || promptBinding.documentId && binding.documentId && binding.documentId !== promptBinding.documentId
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The save decision came from another page");
      }
      const action = String(message.action || "");
      if (!SAVE_ACTIONS.has(action)) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The save decision is invalid");
      }
      const entryId = message.entryId == null ? "" : String(message.entryId).trim();
      if (action === "replace") {
        const replaceAllowed = record.matchedAction === "select" || record.matchedAction === "new";
        if (!replaceAllowed || !record.accountChoices.some((choice) => choice.entryId === entryId)) {
          throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The selected account is not available for this candidate");
        }
      } else if (action !== "ignore" && action !== record.matchedAction) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The save decision does not match PetalDesk's account match");
      }
      if (action === "ignore") {
        clearCandidate(candidateId);
        postEvent("saveDecision", {
          action,
          candidateId,
          documentId: binding.documentId,
          frameId: binding.frameId,
          origin: record.origin,
          promptOrigin: binding.origin,
          tabId: binding.tabId,
        });
        return { accepted: true, action, candidateId };
      }
      if (record.savePending) {
        throw bridgeError("PASSWORD_SAVE_BUSY", "The login candidate is already being saved");
      }
      record.savePending = true;
      renewCandidate(record, CANDIDATE_TTL_MS);
      const posted = postEvent("saveDecision", {
        action,
        candidateId,
        documentId: binding.documentId,
        frameId: binding.frameId,
        origin: record.origin,
        promptOrigin: binding.origin,
        tabId: binding.tabId,
        ...(action === "replace" ? { entryId } : {}),
      });
      if (!posted) {
        record.savePending = false;
        throw bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected");
      }
      return { accepted: true, action, candidateId };
    }

    async function saveResult(payload) {
      const candidateId = requiredString(payload.candidateId, "candidateId", 160);
      const record = candidates.get(candidateId);
      if (!record || record.expiresAt <= Date.now()) {
        clearCandidate(candidateId);
        return { candidateId, cleared: true, expired: true };
      }
      if (!record.savePending) {
        return { candidateId, cleared: false, ignored: true };
      }
      const success = payload.success === true;
      const action = String(payload.action || record.matchedAction || "");
      if (!success) {
        record.savePending = false;
        renewCandidate(record, CANDIDATE_TTL_MS);
      }
      const expiresInMs = success
        ? 0
        : Math.max(0, record.expiresAt - Date.now());
      await api.sendTabMessage(
        record.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "captureSaveResult",
          payload: {
            action,
            candidateId,
            entryId: payload.entryId == null ? null : String(payload.entryId),
            origin: record.origin,
            success,
            error: payload.error && typeof payload.error === "object"
              ? {
                code: String(payload.error.code || "PASSWORD_SAVE_FAILED"),
                message: String(payload.error.message || "保存到飞花失败"),
              }
              : null,
            expiresInMs,
          },
        },
        { frameId: record.frameId },
      ).catch((error) => {
        clearCandidate(candidateId);
        throw error;
      });
      if (success) {
        clearCandidate(candidateId);
      }
      return { candidateId, success, cleared: success };
    }

    async function activeTabContext() {
      if (typeof api.queryActiveTab !== "function") {
        return { documentId: null, origin: "", tabId: null };
      }
      const tab = await api.queryActiveTab().catch(() => null);
      const tabId = tab && Number.isInteger(tab.id) ? tab.id : null;
      if (tabId == null) {
        return { documentId: null, origin: "", tabId: null };
      }
      const context = tabContexts.get(tabId);
      let origin = context ? context.origin : "";
      if (!origin) {
        try {
          origin = templates.exactOrigin(tab.url);
        } catch (_error) {
          origin = "";
        }
      }
      return {
        documentId: context ? context.documentId : null,
        origin,
        tabId,
      };
    }

    async function popupGetState() {
      const active = await activeTabContext();
      const cached = active.tabId == null ? null : tabAccounts.get(active.tabId);
      return {
        diagnostics: {
          captureEnabled,
          extensionVersion: String(api.extensionVersion || ""),
          lastCommandAt: diagnostics.lastCommandAt,
          lastCommandErrorCode: diagnostics.lastCommandErrorCode,
          lastCommandOk: diagnostics.lastCommandOk,
          nativeConnected: diagnostics.nativeConnected,
          secretConnected: diagnostics.secretConnected,
        },
        tab: {
          accounts: cached ? cached.accounts.map((account) => ({ ...account })) : [],
          locked: cached ? cached.locked : false,
          origin: active.origin || (cached ? cached.origin : ""),
        },
      };
    }

    async function popupFill(message) {
      const entryId = requiredString(message.entryId, "entryId", 160);
      const active = await activeTabContext();
      if (active.tabId == null) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "No active browser tab is available");
      }
      const cached = tabAccounts.get(active.tabId);
      if (!cached || cached.locked || !cached.accounts.some((account) => account.entryId === entryId)) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The requested account is not available on this tab");
      }
      const posted = postEvent("fillRequest", {
        documentId: active.documentId,
        entryId,
        origin: active.origin,
        tabId: active.tabId,
      });
      if (!posted) {
        throw bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected");
      }
      return { accepted: true };
    }

    function popupOpenManager() {
      const posted = postEvent("openPasswordManager", {});
      if (!posted) {
        throw bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected");
      }
      return { accepted: true };
    }

    function onPopupMessage(message, sender) {
      // The action popup is an extension page without a tab; content scripts
      // and other extensions must never reach these handlers.
      if (!sender || sender.id !== api.runtime.id || sender.tab) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "The popup message sender is not trusted");
      }
      switch (message.type) {
        case "petaldesk.popup.getState": return popupGetState();
        case "petaldesk.popup.fill": return popupFill(message);
        case "petaldesk.popup.openManager": return popupOpenManager();
        default: return null;
      }
    }

    async function onContentMessage(message, sender) {
      switch (message.type) {
        case "petaldesk.password.page-closed": return onPageClosed(sender);
        case "petaldesk.password.tab-ready": return onTabReady(message, sender);
        case "petaldesk.password.fill-confirm": return onFillConfirm(message, sender);
        case "petaldesk.password.fill-cancel": return onFillCancel(message, sender);
        case "petaldesk.password.capture-username-stage": return onCaptureUsernameStage(message, sender);
        case "petaldesk.password.capture-submitted": return onCaptureSubmitted(message, sender);
        case "petaldesk.password.capture-success": return onCaptureSuccess(message, sender);
        case "petaldesk.password.save-decision": return onSaveDecision(message, sender);
        case "petaldesk.password.template-recording-progress": return onTemplateRecordingProgress(message, sender);
        case "petaldesk.password.template-recording-cancelled": return onTemplateRecordingCancelled(message, sender);
        default: return null;
      }
    }

    api.runtime.onMessage.addListener((message, sender, sendResponse) => {
      if (!message || typeof message.type !== "string") return false;
      const isContentMessage = message.type.startsWith(CONTENT_MESSAGE_PREFIX);
      const isPopupMessage = message.type.startsWith(POPUP_MESSAGE_PREFIX);
      if (!isContentMessage && !isPopupMessage) return false;
      const handler = isPopupMessage
        ? () => onPopupMessage(message, sender)
        : () => onContentMessage(message, sender);
      Promise.resolve().then(handler).then(
        (result) => sendResponse(result || { ok: true }),
        (error) => sendResponse({
          ok: false,
          error: {
            code: error && error.code || "PASSWORD_EVENT_FAILED",
            message: error instanceof Error ? error.message : String(error),
          },
        }),
      );
      return true;
    });

    if (typeof api.onTabRemoved === "function") {
      api.onTabRemoved((tabId) => {
        tabContexts.delete(tabId);
        tabAccounts.delete(tabId);
        clearTabState(tabId);
        postEvent("pageClosed", { documentId: null, tabId });
      });
    }

    if (typeof api.onActivated === "function") {
      api.onActivated((activeInfo) => {
        const tabId = activeInfo && Number.isInteger(activeInfo.tabId) ? activeInfo.tabId : null;
        if (tabId == null) return;
        const context = tabContexts.get(tabId);
        if (context && context.origin) {
          postEvent("originActive", { origin: context.origin, tabId });
          return;
        }
        // No content script has announced this tab, so a badge left over from
        // a previous page must not survive the switch.
        clearTabAccounts(tabId);
      });
    }

    async function onSecretConnected() {
      diagnostics.secretConnected = true;
      const active = await activeTabContext();
      if (active.tabId == null) return;
      if (!tabContexts.has(active.tabId)) {
        tabContexts.set(active.tabId, { documentId: null, origin: active.origin });
      }
      if (!active.origin) clearTabAccounts(active.tabId);
      postEvent("originActive", { origin: active.origin, tabId: active.tabId });
    }

    function disconnect() {
      diagnostics.secretConnected = false;
      captureEnabled = false;
      void broadcastCaptureState().catch(() => {});
      for (const sessionId of Array.from(sessions.keys())) clearSession(sessionId);
      clearCaptureState();
    }

    return Object.freeze({
      capabilities: Object.freeze([...CAPABILITY_GROUPS, ...COMMANDS]),
      commands: COMMANDS,
      disconnect,
      onSecretConnected,
      route,
      setNativeConnected(connected) {
        diagnostics.nativeConnected = connected === true;
      },
      supportsCommand(command) {
        return COMMANDS.has(String(command || ""));
      },
    });
  }

  root.PetalDeskPasswordBridge = Object.freeze({ createPasswordBridge });
})(typeof globalThis !== "undefined" ? globalThis : this);

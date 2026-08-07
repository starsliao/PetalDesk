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
  const SECOND_FACTOR_TTL_MS = 5 * 60 * 1_000;
  const SECOND_FACTOR_CHALLENGE_TTL_MS = 30_000;
  const SECOND_FACTOR_SCAN_SETTLE_MS = 50;
  const MFA_COPY_RESULT_TTL_MS = 20_000;
  const DELETE_RESULT_TTL_MS = 8_000;
  const BADGE_ACCOUNT_LIMIT = 16;
  const CAPABILITY_GROUPS = Object.freeze([
    "password-fill",
    "password-capture",
    "second-factor-fill-v1",
  ]);
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
    "password.armSecondFactor",
    "password.offerSecondFactor",
    "password.provideSecondFactor",
    "password.cancelSecondFactor",
    "password.confirmMfaCopy",
    "password.copyMfaResult",
    "password.deleteResult",
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
    const tabFrames = new Map();
    const tabAccounts = new Map();
    const secondFactors = new Map();
    const secondFactorByTab = new Map();
    const pendingMfaCopies = new Map();
    const pendingDeletes = new Map();
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

    function setFrameContext(binding, fields = {}) {
      let frames = tabFrames.get(binding.tabId);
      if (!frames) {
        frames = new Map();
        tabFrames.set(binding.tabId, frames);
      }
      frames.set(binding.frameId, {
        documentId: binding.documentId,
        hasPassword: fields.hasPassword === true,
        hasUsername: fields.hasUsername === true,
        origin: binding.origin,
        updatedAt: Date.now(),
      });
    }

    function clearFrameContext(tabId, frameId = null, documentId = null) {
      const frames = tabFrames.get(tabId);
      if (!frames) return;
      if (frameId == null) {
        tabFrames.delete(tabId);
        return;
      }
      const current = frames.get(frameId);
      if (!current || !documentId || !current.documentId || current.documentId === documentId) {
        frames.delete(frameId);
      }
      if (frames.size === 0) tabFrames.delete(tabId);
    }

    async function topLevelOrigin(binding, sender) {
      // MessageSender.tab.url is the top-level tab URL even when the message
      // came from a child frame. Prefer a live tabs.get result when available,
      // then fall back to the sender/context snapshot for older Firefox builds.
      const urls = [];
      if (typeof api.getTab === "function") {
        const tab = await api.getTab(binding.tabId).catch(() => null);
        if (tab && tab.url) urls.push(tab.url);
      }
      if (sender && sender.tab && sender.tab.url) urls.push(sender.tab.url);
      const context = tabContexts.get(binding.tabId);
      if (context && context.origin) urls.push(context.origin);
      for (const value of urls) {
        try {
          return templates.exactOrigin(value);
        } catch (_error) {
          // Try the next source; non-web tabs are not valid capture targets.
        }
      }
      return "";
    }

    // Runtime messages from a child frame carry that frame's URL in
    // `sender.url`, while `sender.tab.url` remains the top-level tab URL.
    // Keep a synchronous snapshot for fill-confirm/cancel validation (those
    // handlers intentionally do not await a browser API call).  Firefox 140
    // does not expose Location.ancestorOrigins, so the content script may
    // report the child origin as its `origin` field.
    function topLevelOriginSnapshot(binding, sender) {
      const values = [];
      if (sender && sender.tab && sender.tab.url) values.push(sender.tab.url);
      const context = tabContexts.get(binding.tabId);
      if (context && context.origin) values.push(context.origin);
      for (const value of values) {
        try {
          return templates.exactOrigin(value);
        } catch (_error) {
          // Continue with the next trusted snapshot.
        }
      }
      return "";
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
          frameOrigin: session.frameOrigin,
          origin: session.origin,
          status: "expired",
          submitted: false,
        });
      }, SESSION_TTL_MS);
      if (session.timer && typeof session.timer.unref === "function") session.timer.unref();
    }

    function clearSecondFactorChallenge(journey) {
      if (!journey) return;
      if (journey.reportTimer != null) clearTimeout(journey.reportTimer);
      journey.reportTimer = null;
      if (journey.challenge && journey.challenge.timer != null) {
        clearTimeout(journey.challenge.timer);
      }
      journey.challenge = null;
      journey.reports.clear();
    }

    function removeSecondFactorJourney(journey, { notify = false, reason = "cancelled" } = {}) {
      if (!journey || secondFactors.get(journey.flowId) !== journey) return;
      if (journey.timer != null) clearTimeout(journey.timer);
      clearSecondFactorChallenge(journey);
      secondFactors.delete(journey.flowId);
      if (secondFactorByTab.get(journey.tabId) === journey.flowId) {
        secondFactorByTab.delete(journey.tabId);
      }
      void api.sendTabMessage(
        journey.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "cancelSecondFactor",
          payload: { flowId: journey.flowId },
        },
      ).catch(() => {});
      if (notify) {
        postEvent("cancelSecondFactor", {
          flowId: journey.flowId,
          reason: String(reason || "cancelled").slice(0, 80),
          tabId: journey.tabId,
        });
      }
    }

    async function rearmSecondFactorDocument(journey, challenge) {
      try {
        await api.sendTabMessage(
          journey.tabId,
          {
            type: CONTENT_MESSAGE_TYPE,
            command: "cancelSecondFactor",
            payload: { flowId: journey.flowId },
          },
          { documentId: challenge.documentId, frameId: challenge.frameId },
        );
      } catch (_error) {
        // Navigation may already have destroyed the challenged document. The
        // current document still gets a fresh arm below when the journey lives.
      }
      if (
        secondFactors.get(journey.flowId) !== journey
        || journey.expiresAt <= Date.now()
      ) return;
      await api.sendTabMessage(
        journey.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "armSecondFactor",
          payload: { expiresAt: journey.expiresAt, flowId: journey.flowId },
        },
        { frameId: challenge.frameId },
      ).catch(() => {});
    }

    function expireSecondFactorChallenge(journey, challenge) {
      if (!journey || journey.challenge !== challenge) return;
      clearSecondFactorChallenge(journey);
      // A 30-second field challenge is disposable; the five-minute login
      // journey is not. Reset the page-side references and let a still-visible
      // field produce a freshly bound, single-use challenge.
      void rearmSecondFactorDocument(journey, challenge);
    }

    async function settleSecondFactorReports(journey) {
      if (!journey || secondFactors.get(journey.flowId) !== journey) return;
      journey.reportTimer = null;
      if (journey.expiresAt <= Date.now()) {
        removeSecondFactorJourney(journey, { notify: true, reason: "expired" });
        return;
      }
      const reports = Array.from(journey.reports.values()).filter((report) => report.count > 0);
      if (reports.length === 0) return;
      const totalCount = reports.reduce((sum, report) => sum + report.count, 0);
      const preferred = reports.find((report) => report.confidence === "high") || reports[0];
      const challenge = {
        challengeId: secureRandomId("mfa-challenge"),
        confidence: totalCount === 1 && preferred.confidence === "high" ? "high" : "low",
        confirmed: false,
        consumed: false,
        count: totalCount,
        digits: preferred.digits,
        documentId: preferred.documentId,
        expiresAt: Math.min(
          journey.expiresAt,
          Date.now() + SECOND_FACTOR_CHALLENGE_TTL_MS,
        ),
        frameId: preferred.frameId,
        frameOrigin: preferred.frameOrigin,
        offered: false,
        originConfirmed: preferred.originPreauthorized,
        originPreauthorized: preferred.originPreauthorized,
        requiresOriginConfirmation: false,
        timer: null,
        topOrigin: preferred.topOrigin,
      };
      if (journey.challenge && journey.challenge.timer != null) {
        clearTimeout(journey.challenge.timer);
      }
      journey.challenge = challenge;
      challenge.timer = setTimeout(
        () => expireSecondFactorChallenge(journey, challenge),
        Math.max(0, challenge.expiresAt - Date.now()),
      );
      if (challenge.timer && typeof challenge.timer.unref === "function") challenge.timer.unref();
      let response;
      try {
        response = await api.sendTabMessage(
          journey.tabId,
          {
            type: CONTENT_MESSAGE_TYPE,
            command: "bindSecondFactor",
            payload: {
              challengeId: challenge.challengeId,
              expiresAt: challenge.expiresAt,
              flowId: journey.flowId,
            },
          },
          {
            documentId: challenge.documentId,
            frameId: challenge.frameId,
          },
        );
      } catch (_error) {
        response = null;
      }
      if (journey.challenge !== challenge) return;
      if (!response || response.ok !== true || !response.result || response.result.bound !== true) {
        clearSecondFactorChallenge(journey);
        void api.sendTabMessage(
          journey.tabId,
          {
            type: CONTENT_MESSAGE_TYPE,
            command: "armSecondFactor",
            payload: { expiresAt: journey.expiresAt, flowId: journey.flowId },
          },
          { frameId: challenge.frameId },
        ).catch(() => {});
        return;
      }
      postEvent("secondFactorOffer", {
        challengeId: challenge.challengeId,
        confidence: challenge.confidence,
        count: challenge.count,
        digits: challenge.digits,
        documentId: challenge.documentId,
        flowId: journey.flowId,
        frameId: challenge.frameId,
        frameOrigin: challenge.frameOrigin,
        tabId: journey.tabId,
        topOrigin: challenge.topOrigin,
      });
    }

    function scheduleSecondFactorReportSettle(journey) {
      if (journey.reportTimer != null) clearTimeout(journey.reportTimer);
      journey.reportTimer = setTimeout(
        () => { void settleSecondFactorReports(journey); },
        SECOND_FACTOR_SCAN_SETTLE_MS,
      );
      if (journey.reportTimer && typeof journey.reportTimer.unref === "function") {
        journey.reportTimer.unref();
      }
    }

    function rejectPendingMfaCopy(requestId, error) {
      const pending = pendingMfaCopies.get(requestId);
      if (!pending) return false;
      if (pending.timer != null) clearTimeout(pending.timer);
      pendingMfaCopies.delete(requestId);
      pending.reject(error);
      return true;
    }

    function clearPendingMfaCopies(code, message) {
      for (const requestId of Array.from(pendingMfaCopies.keys())) {
        rejectPendingMfaCopy(requestId, bridgeError(code, message));
      }
    }

    function rejectPendingDelete(requestId, error) {
      const pending = pendingDeletes.get(requestId);
      if (!pending) return false;
      if (pending.timer != null) clearTimeout(pending.timer);
      pendingDeletes.delete(requestId);
      pending.reject(error);
      return true;
    }

    function clearPendingDeletes(code, message) {
      for (const requestId of Array.from(pendingDeletes.keys())) {
        rejectPendingDelete(requestId, bridgeError(code, message));
      }
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
      const frameId = optionalRoutingId(sender && sender.frameId, "frameId") ?? 0;
      // A top-level navigation invalidates every document in the tab. Child
      // frame notifications remain document-scoped so an unrelated frame does
      // not retire another frame's pending secret.
      clearTabState(tabId, frameId === 0 ? null : documentId, { preserveUsernameStage: true });
      if (tabId != null) {
        clearFrameContext(tabId, frameId === 0 ? null : frameId, documentId);
        if (frameId === 0) {
          clearTabAccounts(tabId);
          for (const [requestId, pending] of pendingMfaCopies.entries()) {
            if (pending.tabId === tabId) {
              rejectPendingMfaCopy(
                requestId,
                bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA copy page changed"),
              );
            }
          }
          for (const [requestId, pending] of pendingDeletes.entries()) {
            if (pending.tabId === tabId) {
              rejectPendingDelete(
                requestId,
                bridgeError("PASSWORD_TARGET_MISMATCH", "The delete request page changed"),
              );
            }
          }
          postEvent("originActive", { origin: "", tabId });
        }
        const journey = secondFactors.get(secondFactorByTab.get(tabId));
        if (journey) {
          for (const [key, report] of journey.reports.entries()) {
            if (frameId === 0 || report.frameId === frameId) {
              if (!documentId || !report.documentId || report.documentId === documentId) {
                journey.reports.delete(key);
              }
            }
          }
          const challenge = journey.challenge;
          if (
            challenge
            && (frameId === 0 || challenge.frameId === frameId)
            && (frameId !== 0
              ? (!documentId || !challenge.documentId || challenge.documentId === documentId)
              : true)
          ) {
            clearSecondFactorChallenge(journey);
          }
        }
      }
      postEvent("pageClosed", { documentId, frameId, tabId });
      return { cleared: true, tabId, documentId, frameId };
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

    async function sendContent(session, command, payload, { broadcast = false, frameId = null } = {}) {
      await validateLiveTab(session);
      const targetFrameId = frameId == null ? session.frameId : frameId;
      const response = await api.sendTabMessage(
        session.tabId,
        { type: CONTENT_MESSAGE_TYPE, command, payload },
        broadcast ? undefined : { frameId: targetFrameId },
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

    function fillFrameCandidates(session) {
      const frames = tabFrames.get(session.tabId);
      if (!frames || frames.size === 0) return [];
      const candidates = Array.from(frames.entries())
        .filter(([frameId, frame]) => {
          if (!Number.isInteger(frameId) || frameId < 0) return false;
          if (!templates.frameOriginAllowed(session.origin, frame.origin)) return false;
          if (session.documentId && frame.documentId && frameId === session.frameId
            && frame.documentId !== session.documentId) return false;
          return true;
        })
        .sort(([leftId, left], [rightId, right]) => {
          // Prefer a frame that advertises a password field, then a username
          // field, and only then the top-level frame. This avoids a username
          // shell claiming a direct fill before the real iframe responds.
          const score = (frame, frameId) => (frame.hasPassword ? 30 : 0)
            + (frame.hasUsername ? 10 : 0)
            + (frameId === 0 ? 0 : 1);
          return score(right, rightId) - score(left, leftId);
        })
        .map(([frameId]) => frameId);
      if (Number.isInteger(session.frameId) && !candidates.includes(session.frameId)) {
        candidates.unshift(session.frameId);
      }
      return candidates;
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
        frameOrigin: origin,
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
      const origin = templates.exactOrigin(payload.origin);
      // The offer is broadcast to every frame of the tab, so a requested
      // frameId is advisory only: the binding to a concrete frame (possibly an
      // explicitly trusted login iframe) is established by fill-confirm.
      if (requestedTabId !== session.tabId || origin !== session.origin) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The fill offer target does not match its session");
      }
      const offerId = requiredString(payload.offerId || secureRandomId("offer"), "offerId", 160);
      const username = String(payload.username == null ? "" : payload.username);
      if (username.length > 1_024) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "username is too long");
      }
      // Bind the offer before delivery: a direct fill confirms itself from the
      // page, and that fill-confirm can arrive before sendContent resolves.
      session.offerId = offerId;
      session.state = "awaiting-confirmation";
      let result;
      try {
        const offerPayload = {
          allowInsecureHttp: session.allowInsecureHttp,
          direct: true,
          entryId: session.entryId,
          offerId,
          origin,
          sessionId: session.sessionId,
          userTemplate: payload.userTemplate || null,
          username,
        };
        const frameIds = fillFrameCandidates(session);
        let lastError = null;
        for (const frameId of frameIds) {
          try {
            const candidate = await sendContent(session, "fillOffer", offerPayload, { frameId });
            if (!candidate.ignored) {
              result = candidate;
              break;
            }
          } catch (error) {
            lastError = error;
          }
        }
        if (!result) {
          // A frame may have been created after tab-ready (common for SPA
          // login widgets). Keep the broadcast fallback for that case; the
          // confirmation event is still the authoritative frame binding.
          try {
            result = await sendContent(session, "fillOffer", offerPayload, { broadcast: true });
          } catch (error) {
            if (lastError) throw lastError;
            throw error;
          }
        }
      } catch (error) {
        session.offerId = null;
        session.state = "ready";
        throw error;
      }
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
        frameOrigin: origin,
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
        frameOrigin: session.frameOrigin,
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
        }, { broadcast: true });
      } catch (_error) {
        // Navigation can remove the content script; cancellation still clears the session.
      }
      postEvent("fillResult", {
        sessionId: session.sessionId,
        tabId: session.tabId,
        frameId: session.frameId,
        frameOrigin: session.frameOrigin,
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
        // No frameId: every frame of the tab toggles its own login detection,
        // so explicitly trusted login frames can report credentials too.
        await api.sendTabMessage(
          tab.id,
          {
            type: CONTENT_MESSAGE_TYPE,
            command,
            payload: {
              insecureOrigins: Array.from(captureInsecureOrigins),
              topLevelOrigin: origin,
            },
          },
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

    function exactHttpsOrigin(value, name) {
      const origin = templates.exactOrigin(value);
      if (!origin.startsWith("https://")) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", `${name} must be an exact HTTPS origin`);
      }
      return origin;
    }

    async function armSecondFactor(payload) {
      const flowId = requiredString(payload.flowId, "flowId", 160);
      const tabId = optionalRoutingId(payload.tabId, "tabId");
      if (tabId == null) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "A second-factor target tab is required");
      }
      if (secondFactors.has(flowId)) {
        throw bridgeError("PASSWORD_SESSION_EXISTS", "The second-factor journey already exists");
      }
      const topOrigin = exactHttpsOrigin(payload.topOrigin, "topOrigin");
      const tab = await api.getTab(tabId).catch(() => null);
      let liveOrigin = "";
      try {
        liveOrigin = templates.exactOrigin(tab && tab.url);
      } catch (_error) {
        liveOrigin = "";
      }
      if (!liveOrigin.startsWith("https://")) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "The second-factor tab is not on HTTPS");
      }
      const allowedOrigins = new Set([topOrigin]);
      if (payload.allowedOrigins != null && !Array.isArray(payload.allowedOrigins)) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "allowedOrigins must be an array");
      }
      for (const value of payload.allowedOrigins || []) {
        allowedOrigins.add(exactHttpsOrigin(value, "allowedOrigins"));
      }
      if (Array.from(allowedOrigins).filter((origin) => origin !== topOrigin).length > 8) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "At most eight additional MFA origins are allowed");
      }
      const requestedExpiry = Number(payload.expiresAt);
      if (!Number.isFinite(requestedExpiry) || requestedExpiry <= Date.now()) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The second-factor journey expiry is invalid");
      }
      const existingFlowId = secondFactorByTab.get(tabId);
      if (existingFlowId) {
        removeSecondFactorJourney(secondFactors.get(existingFlowId), {
          notify: true,
          reason: "replaced",
        });
      }
      const journey = {
        allowedOrigins,
        challenge: null,
        expiresAt: Math.min(requestedExpiry, Date.now() + SECOND_FACTOR_TTL_MS),
        flowId,
        loginOrigin: topOrigin,
        reportTimer: null,
        reports: new Map(),
        tabId,
        timer: null,
      };
      journey.timer = setTimeout(() => {
        removeSecondFactorJourney(journey, { notify: true, reason: "expired" });
      }, Math.max(0, journey.expiresAt - Date.now()));
      if (journey.timer && typeof journey.timer.unref === "function") journey.timer.unref();
      secondFactors.set(flowId, journey);
      secondFactorByTab.set(tabId, flowId);
      await api.sendTabMessage(
        tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "armSecondFactor",
          payload: { expiresAt: journey.expiresAt, flowId },
        },
      ).catch(() => {});
      return {
        armed: true,
        expiresAt: journey.expiresAt,
        flowId,
        tabId,
      };
    }

    function secondFactorJourney(payload) {
      const flowId = requiredString(payload.flowId, "flowId", 160);
      const journey = secondFactors.get(flowId);
      if (!journey || journey.expiresAt <= Date.now()) {
        if (journey) removeSecondFactorJourney(journey, { notify: true, reason: "expired" });
        throw bridgeError("PASSWORD_SESSION_EXPIRED", "The second-factor journey has expired");
      }
      return journey;
    }

    async function liveSecondFactorOrigin(journey) {
      const tab = await api.getTab(journey.tabId).catch(() => null);
      let origin = "";
      try {
        origin = exactHttpsOrigin(tab && tab.url, "topOrigin");
      } catch (_error) {
        origin = "";
      }
      if (!origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The second-factor tab is not on HTTPS");
      }
      return origin;
    }

    function secondFactorChallenge(payload) {
      const journey = secondFactorJourney(payload);
      const challengeId = requiredString(payload.challengeId, "challengeId", 160);
      const challenge = journey.challenge;
      if (
        !challenge
        || challenge.challengeId !== challengeId
        || challenge.expiresAt <= Date.now()
        || challenge.consumed
      ) {
        throw bridgeError("PASSWORD_CANDIDATE_EXPIRED", "The second-factor challenge has expired");
      }
      const tabId = optionalRoutingId(payload.tabId, "tabId");
      const frameId = optionalRoutingId(payload.frameId, "frameId");
      const documentId = requiredString(payload.documentId, "documentId", 256);
      const topOrigin = exactHttpsOrigin(payload.topOrigin, "topOrigin");
      if (
        tabId !== journey.tabId
        || frameId !== challenge.frameId
        || documentId !== challenge.documentId
        || topOrigin !== challenge.topOrigin
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The second-factor target binding changed");
      }
      return { challenge, journey };
    }

    async function offerSecondFactor(payload) {
      const { challenge, journey } = secondFactorChallenge(payload);
      const liveOrigin = await liveSecondFactorOrigin(journey);
      if (liveOrigin !== challenge.topOrigin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The second-factor tab origin changed");
      }
      const requiresOriginConfirmation = payload.requiresOriginConfirmation === true;
      if (!challenge.originPreauthorized && !requiresOriginConfirmation) {
        throw bridgeError(
          "PASSWORD_ORIGIN_CONFIRMATION_REQUIRED",
          "The exact cross-origin MFA site requires confirmation",
        );
      }
      const response = await api.sendTabMessage(
        journey.tabId,
        {
          type: CONTENT_MESSAGE_TYPE,
          command: "offerSecondFactor",
          payload: {
            challengeId: challenge.challengeId,
            expiresAt: challenge.expiresAt,
            flowId: journey.flowId,
            requiresOriginConfirmation,
            topOrigin: challenge.topOrigin,
          },
        },
        {
          documentId: challenge.documentId,
          frameId: challenge.frameId,
        },
      );
      if (!response || response.ok !== true) {
        throw bridgeError(
          "PASSWORD_CONTENT_FAILED",
          response && response.error
            ? response.error.message || String(response.error)
            : "The second-factor page did not respond",
        );
      }
      challenge.offered = true;
      challenge.requiresOriginConfirmation = requiresOriginConfirmation;
      return {
        challengeId: challenge.challengeId,
        flowId: journey.flowId,
        offered: true,
      };
    }

    async function provideSecondFactor(payload) {
      let code = String(payload.code == null ? "" : payload.code);
      try {
        const { challenge, journey } = secondFactorChallenge(payload);
        if (challenge.confidence !== "high" && !challenge.confirmed) {
          throw bridgeError("PASSWORD_CONFIRMATION_REQUIRED", "The MFA field requires user confirmation");
        }
        if (!challenge.originPreauthorized && !challenge.originConfirmed) {
          throw bridgeError(
            "PASSWORD_ORIGIN_CONFIRMATION_REQUIRED",
            "The exact cross-origin MFA site was not confirmed",
          );
        }
        const liveOrigin = await liveSecondFactorOrigin(journey);
        if (liveOrigin !== challenge.topOrigin) {
          throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The second-factor tab origin changed");
        }
        if (!/^\d{6,8}$/.test(code) || challenge.digits && code.length !== challenge.digits) {
          throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The MFA verification code is invalid");
        }
        challenge.consumed = true;
        try {
          const response = await api.sendTabMessage(
            journey.tabId,
            {
              type: CONTENT_MESSAGE_TYPE,
              command: "provideSecondFactor",
              payload: {
                challengeId: challenge.challengeId,
                code,
                flowId: journey.flowId,
              },
            },
            {
              documentId: challenge.documentId,
              frameId: challenge.frameId,
            },
          );
          if (!response || response.ok !== true) {
            throw bridgeError(
              "PASSWORD_CONTENT_FAILED",
              response && response.error
                ? response.error.message || String(response.error)
                : "The second-factor page did not respond",
            );
          }
          const result = response.result || {};
          if (result.filled !== true || result.submitted !== false) {
            throw bridgeError("PASSWORD_CONTENT_FAILED", "The second-factor page did not confirm a safe fill");
          }
          postEvent("secondFactorResult", {
            challengeId: challenge.challengeId,
            digits: Number(result.digits || code.length),
            documentId: challenge.documentId,
            fields: Number(result.fields || 1),
            flowId: journey.flowId,
            frameId: challenge.frameId,
            frameOrigin: challenge.frameOrigin,
            segmented: result.segmented === true,
            status: "filled",
            submitted: false,
            tabId: journey.tabId,
            topOrigin: challenge.topOrigin,
          });
          removeSecondFactorJourney(journey);
          return {
            ...result,
            challengeId: challenge.challengeId,
            flowId: journey.flowId,
            frameId: challenge.frameId,
            tabId: journey.tabId,
          };
        } catch (error) {
          postEvent("secondFactorResult", {
            challengeId: challenge.challengeId,
            documentId: challenge.documentId,
            flowId: journey.flowId,
            frameId: challenge.frameId,
            frameOrigin: challenge.frameOrigin,
            status: "failed",
            submitted: false,
            tabId: journey.tabId,
            topOrigin: challenge.topOrigin,
          });
          clearSecondFactorChallenge(journey);
          void rearmSecondFactorDocument(journey, challenge);
          throw error;
        }
      } finally {
        code = "";
        if (Object.prototype.hasOwnProperty.call(payload, "code")) payload.code = "";
      }
    }

    async function cancelSecondFactor(payload) {
      let journey = null;
      if (payload.flowId != null && String(payload.flowId).trim() !== "") {
        journey = secondFactors.get(requiredString(payload.flowId, "flowId", 160)) || null;
      } else {
        const tabId = optionalRoutingId(payload.tabId, "tabId");
        if (tabId != null) journey = secondFactors.get(secondFactorByTab.get(tabId)) || null;
      }
      if (!journey) return { cancelled: false };
      const requestedTabId = optionalRoutingId(payload.tabId, "tabId");
      if (requestedTabId != null && requestedTabId !== journey.tabId) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The second-factor cancel target changed");
      }
      if (payload.preserveJourney === true) {
        const challengeId = requiredString(payload.challengeId, "challengeId", 160);
        const challenge = journey.challenge;
        if (
          !challenge
          || challenge.challengeId !== challengeId
          || challenge.expiresAt <= Date.now()
          || challenge.consumed
        ) {
          throw bridgeError(
            "PASSWORD_CANDIDATE_EXPIRED",
            "The second-factor challenge is no longer current",
          );
        }
        clearSecondFactorChallenge(journey);
        await rearmSecondFactorDocument(journey, challenge);
        return {
          cancelled: true,
          challengeId,
          flowId: journey.flowId,
          preserved: true,
          tabId: journey.tabId,
        };
      }
      removeSecondFactorJourney(journey, {
        notify: true,
        reason: String(payload.reason || "cancelled"),
      });
      return { cancelled: true, flowId: journey.flowId, tabId: journey.tabId };
    }

    function copyMfaResult(payload) {
      const requestId = requiredString(payload.requestId, "requestId", 160);
      const pending = pendingMfaCopies.get(requestId);
      if (!pending) {
        throw bridgeError("PASSWORD_REQUEST_EXPIRED", "The MFA copy request is no longer pending");
      }
      if (pending.timer != null) clearTimeout(pending.timer);
      pendingMfaCopies.delete(requestId);
      if (payload.success === true) {
        pending.resolve({ accepted: true });
      } else {
        const errorValue = payload.error && typeof payload.error === "object" ? payload.error : {};
        pending.reject(bridgeError(
          String(errorValue.code || "PASSWORD_MFA_COPY_FAILED").slice(0, 80),
          String(errorValue.message || "MFA 验证码复制失败").slice(0, 512),
        ));
      }
      return { requestId, resolved: true };
    }

    async function confirmMfaCopy(payload) {
      const requestId = requiredString(payload.requestId, "requestId", 160);
      const pending = pendingMfaCopies.get(requestId);
      if (!pending) {
        throw bridgeError("PASSWORD_REQUEST_EXPIRED", "The MFA copy request is no longer pending");
      }
      const tabId = optionalRoutingId(payload.tabId, "tabId");
      const origin = exactHttpsOrigin(payload.origin, "origin");
      if (tabId !== pending.tabId || origin !== pending.origin) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA copy target changed");
      }
      const active = await activeTabContext();
      const cached = tabAccounts.get(pending.tabId);
      const account = cached && cached.accounts.find((item) => item.entryId === pending.entryId);
      if (
        active.tabId !== pending.tabId
        || active.origin !== pending.origin
        || !cached
        || cached.locked
        || cached.origin !== pending.origin
        || !account
        || account.hasMfa !== true
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA copy page or account changed");
      }
      return { confirmed: true, requestId, tabId: pending.tabId };
    }

    function deleteResult(payload) {
      const requestId = requiredString(payload.requestId, "requestId", 160);
      const pending = pendingDeletes.get(requestId);
      if (!pending) {
        throw bridgeError("PASSWORD_REQUEST_EXPIRED", "The delete request is no longer pending");
      }
      if (pending.timer != null) clearTimeout(pending.timer);
      pendingDeletes.delete(requestId);
      if (payload.success === true) {
        pending.resolve({ accepted: true });
      } else {
        const errorValue = payload.error && typeof payload.error === "object" ? payload.error : {};
        pending.reject(bridgeError(
          String(errorValue.code || "PASSWORD_DELETE_FAILED").slice(0, 80),
          String(errorValue.message || "删除账户失败").slice(0, 512),
        ));
      }
      return { requestId, resolved: true };
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
        return [{ entryId, hasMfa: item.hasMfa === true, username, siteName }];
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
        pendingSecondFactorSessions: secondFactors.size,
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
        case "password.armSecondFactor": return armSecondFactor(payload);
        case "password.offerSecondFactor": return offerSecondFactor(payload);
        case "password.provideSecondFactor": return provideSecondFactor(payload);
        case "password.cancelSecondFactor": return cancelSecondFactor(payload);
        case "password.confirmMfaCopy": return confirmMfaCopy(payload);
        case "password.copyMfaResult": return copyMfaResult(payload);
        case "password.deleteResult": return deleteResult(payload);
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
        frameOrigin: record.frameOrigin,
        origin: record.origin,
        // New desktop builds also accept a trusted frame origin here, but
        // the top-level value keeps the event compatible with older builds
        // that required promptOrigin to equal the vault origin.
        promptOrigin: record.origin,
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
      const sameFrame = binding.frameId === record.frameId && binding.origin === record.origin;
      const trustedFrame = binding.frameId === record.frameId
        && Boolean(record.frameOrigin)
        && binding.origin === record.frameOrigin
        && templates.frameOriginAllowed(record.origin, record.frameOrigin);
      if (
        binding.tabId !== record.tabId
        || !sameFrame && !trustedFrame
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
      if (binding.frameId !== 0) {
        // Non-top frames announce themselves only to learn the capture state;
        // tab bookkeeping and originActive remain top-frame responsibilities.
        if (!announcedOrigin || announcedOrigin !== binding.origin) {
          throw bridgeError("PASSWORD_TARGET_MISMATCH", "The ready page did not match its browser frame");
        }
        setFrameContext(binding, message);
        const frameTopOrigin = await topLevelOrigin(binding, sender);
        const trustedFrame = Boolean(frameTopOrigin)
          && templates.frameOriginAllowed(frameTopOrigin, binding.origin);
        const frameCaptureAllowed = trustedFrame && (
          binding.origin.startsWith("https://")
          || captureInsecureOrigins.has(binding.origin)
        );
        const secondFactorArm = await secondFactorArmForBinding(binding, sender);
        return {
          captureEnabled: Boolean(captureEnabled && frameCaptureAllowed),
          insecureOrigins: Array.from(captureInsecureOrigins),
          secondFactorArm,
          topLevelOrigin: frameTopOrigin,
        };
      }
      if (!announcedOrigin) {
        // A top-level page without a valid HTTP(S) origin has no accounts; drop
        // stale badge state and report the tab as having no active origin.
        tabContexts.set(binding.tabId, { documentId: binding.documentId, origin: "" });
        clearTabAccounts(binding.tabId);
        postEvent("originActive", { origin: "", tabId: binding.tabId });
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The ready page did not match its browser frame");
      }
      if (announcedOrigin !== binding.origin) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The ready page did not match its browser frame");
      }
      const previousContext = tabContexts.get(binding.tabId);
      // A child frame can announce itself before the top document's content
      // script finishes (especially when a login iframe is inserted during
      // parsing). Do not discard that fresh capability snapshot merely
      // because the top-frame context has not been recorded yet; clear the
      // map only when an established top document actually changes.
      if (previousContext && previousContext.documentId !== binding.documentId) {
        clearFrameContext(binding.tabId);
        const journey = secondFactors.get(secondFactorByTab.get(binding.tabId));
        if (journey) clearSecondFactorChallenge(journey);
      }
      setFrameContext(binding, message);
      tabContexts.set(binding.tabId, { documentId: binding.documentId, origin: binding.origin });
      postEvent("originActive", { origin: binding.origin, tabId: binding.tabId });
      const cachedAccounts = tabAccounts.get(binding.tabId);
      if (cachedAccounts && cachedAccounts.origin !== binding.origin) {
        // The desktop pushes badge accounts per origin; a navigation makes any
        // previously cached list stale until the next password.updateBadge.
        clearTabAccounts(binding.tabId);
      } else if (cachedAccounts) {
        // Same-origin refresh: replay the cached badge at once so the count
        // does not flash empty while the desktop re-pushes its account list.
        setBadgeText(
          binding.tabId,
          cachedAccounts.locked || cachedAccounts.accounts.length === 0
            ? ""
            : String(cachedAccounts.accounts.length),
        );
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
            frameOrigin: binding.origin,
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
      const originCaptureAllowed = binding.origin.startsWith("https://")
        || captureInsecureOrigins.has(binding.origin);
      const secondFactorArm = await secondFactorArmForBinding(binding, sender);
      return {
        captureEnabled: Boolean(captureEnabled && originCaptureAllowed),
        insecureOrigins: Array.from(captureInsecureOrigins),
        secondFactorArm,
        topLevelOrigin: binding.origin,
      };
    }

    async function onFrameState(message, sender) {
      const binding = senderBinding(sender);
      const origin = templates.exactOrigin(message.origin);
      if (origin !== binding.origin) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The frame state origin did not match its browser frame");
      }
      setFrameContext(binding, message);
      return { accepted: true, frameId: binding.frameId };
    }

    async function secondFactorArmForBinding(binding, sender) {
      const journey = secondFactors.get(secondFactorByTab.get(binding.tabId));
      if (!journey) return null;
      if (journey.expiresAt <= Date.now()) {
        removeSecondFactorJourney(journey, { notify: true, reason: "expired" });
        return null;
      }
      const topOrigin = await topLevelOrigin(binding, sender);
      if (!topOrigin || !topOrigin.startsWith("https://")) return null;
      // Same-origin iframes may own segmented OTP controls. Cross-origin OTP
      // iframes are rejected even when their parent page is allowed.
      if (binding.frameId > 0 && binding.origin !== topOrigin) return null;
      return { expiresAt: journey.expiresAt, flowId: journey.flowId };
    }

    async function onSecondFactorCandidates(message, sender) {
      const allowedKeys = new Set(["type", "count", "digits", "confidence"]);
      if (Object.keys(message).some((key) => !allowedKeys.has(key))) {
        throw bridgeError(
          "PASSWORD_PROTOCOL_INVALID",
          "Second-factor candidates may contain only field-shape metadata",
        );
      }
      const binding = senderBinding(sender);
      const journey = secondFactors.get(secondFactorByTab.get(binding.tabId));
      if (!journey || journey.expiresAt <= Date.now()) {
        if (journey) removeSecondFactorJourney(journey, { notify: true, reason: "expired" });
        throw bridgeError("PASSWORD_SESSION_EXPIRED", "No second-factor journey is active for this tab");
      }
      if (!binding.documentId) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "The second-factor document identity is unavailable");
      }
      if (binding.tabId !== journey.tabId) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The second-factor candidate came from another tab");
      }
      const topOrigin = await topLevelOrigin(binding, sender);
      if (!topOrigin || !topOrigin.startsWith("https://")) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "MFA filling is available only on HTTPS");
      }
      if (binding.frameId === 0) {
        if (binding.origin !== topOrigin) {
          throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The second-factor top origin is invalid");
        }
      } else if (binding.origin !== topOrigin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "Cross-origin MFA iframes are not allowed");
      }
      const count = Number(message.count);
      const digits = Number(message.digits);
      if (!Number.isInteger(count) || count < 0 || count > 8) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The MFA candidate count is invalid");
      }
      if (![0, 6, 7, 8].includes(digits) || count > 0 && digits === 0) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The MFA candidate digit count is invalid");
      }
      const confidence = message.confidence === "high" && count === 1 ? "high" : "low";
      const reportKey = `${binding.frameId}:${binding.documentId}`;
      if (count === 0) {
        journey.reports.delete(reportKey);
        const challenge = journey.challenge;
        if (
          challenge
          && challenge.frameId === binding.frameId
          && challenge.documentId === binding.documentId
        ) {
          clearSecondFactorChallenge(journey);
        }
        return { accepted: true, count: 0 };
      }
      if (journey.challenge) {
        clearSecondFactorChallenge(journey);
      }
      journey.reports.set(reportKey, {
        confidence,
        count,
        digits,
        documentId: binding.documentId,
        frameId: binding.frameId,
        frameOrigin: binding.origin,
        originPreauthorized: journey.allowedOrigins.has(topOrigin),
        topOrigin,
      });
      scheduleSecondFactorReportSettle(journey);
      return { accepted: true, count };
    }

    async function onSecondFactorConfirm(message, sender) {
      const allowedKeys = new Set(["type", "flowId", "challengeId", "originConfirmed"]);
      if (Object.keys(message).some((key) => !allowedKeys.has(key))) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The second-factor confirmation is invalid");
      }
      const journey = secondFactorJourney(message);
      const challengeId = requiredString(message.challengeId, "challengeId", 160);
      const challenge = journey.challenge;
      if (
        !challenge
        || challenge.challengeId !== challengeId
        || challenge.expiresAt <= Date.now()
        || challenge.consumed
      ) {
        throw bridgeError("PASSWORD_CANDIDATE_EXPIRED", "The second-factor challenge has expired");
      }
      const binding = senderBinding(sender);
      if (
        binding.tabId !== journey.tabId
        || binding.frameId !== challenge.frameId
        || binding.documentId !== challenge.documentId
        || binding.origin !== challenge.frameOrigin
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA confirmation came from another page");
      }
      const liveOrigin = await liveSecondFactorOrigin(journey);
      if (liveOrigin !== challenge.topOrigin || !challenge.offered) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA confirmation target changed");
      }
      const originConfirmed = message.originConfirmed === true;
      if (challenge.requiresOriginConfirmation && !originConfirmed) {
        throw bridgeError(
          "PASSWORD_ORIGIN_CONFIRMATION_REQUIRED",
          "The exact cross-origin MFA site was not confirmed",
        );
      }
      challenge.confirmed = true;
      challenge.originConfirmed = challenge.originPreauthorized || originConfirmed;
      postEvent("secondFactorConfirm", {
        challengeId: challenge.challengeId,
        documentId: challenge.documentId,
        flowId: journey.flowId,
        frameId: challenge.frameId,
        frameOrigin: challenge.frameOrigin,
        originConfirmed,
        tabId: journey.tabId,
        topOrigin: challenge.topOrigin,
      });
      return { confirmed: true };
    }

    async function onSecondFactorCancel(message, sender) {
      const allowedKeys = new Set(["type", "flowId", "challengeId"]);
      if (Object.keys(message).some((key) => !allowedKeys.has(key))) {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The second-factor cancellation is invalid");
      }
      const journey = secondFactorJourney(message);
      const challengeId = requiredString(message.challengeId, "challengeId", 160);
      const challenge = journey.challenge;
      const binding = senderBinding(sender);
      if (
        !challenge
        || challenge.challengeId !== challengeId
        || binding.tabId !== journey.tabId
        || binding.frameId !== challenge.frameId
        || binding.documentId !== challenge.documentId
        || binding.origin !== challenge.frameOrigin
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA cancellation came from another page");
      }
      const liveOrigin = await liveSecondFactorOrigin(journey);
      if (liveOrigin !== challenge.topOrigin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The second-factor tab origin changed");
      }
      removeSecondFactorJourney(journey, { notify: true, reason: "user-cancelled" });
      return { cancelled: true };
    }

    function sessionForContent(message, sender) {
      const session = sessionForPayload(message);
      const binding = senderBinding(sender);
      const offerId = requiredString(message.offerId, "offerId", 160);
      const origin = templates.exactOrigin(message.origin);
      // frameOrigin is the sending frame's own origin; it must agree with the
      // browser-reported sender URL so a frame cannot claim another identity.
      const frameOrigin = message.frameOrigin == null
        ? binding.origin
        : templates.exactOrigin(message.frameOrigin);
      const frameMatches = binding.frameId === session.frameId;
      const topOrigin = topLevelOriginSnapshot(binding, sender);
      const trustedFrame = binding.frameId > 0
        && templates.frameOriginAllowed(session.origin, binding.origin)
        // A child frame is trusted only when Firefox's Tab.url (or the
        // validated top-frame context) still names the session origin.
        && topOrigin === session.origin;
      // Firefox 140 does not expose location.ancestorOrigins. In that case a
      // child frame reports its own origin in `message.origin`; accept either
      // the requested top-level origin or the verified child origin, but only
      // for an explicitly trusted frame during the initial confirmation.
      const claimedOriginAllowed = origin === session.origin
        || trustedFrame && origin === binding.origin;
      const frameAllowed = frameMatches
        || session.state === "awaiting-confirmation" && trustedFrame;
      if (
        session.state === "awaiting-confirmation"
        && !frameMatches
        && !trustedFrame
        && frameOrigin === binding.origin
      ) {
        // Preserve a distinct diagnostic for a cross-site frame attempting to
        // claim an offer; callers use this to retire the pending session.
        throw bridgeError(
          "PASSWORD_CROSS_SITE_FRAME",
          "The page frame is not trusted for the fill session",
        );
      }
      if (
        binding.tabId !== session.tabId
        || topOrigin !== session.origin
        || !claimedOriginAllowed
        || offerId !== session.offerId
        || frameOrigin !== binding.origin
        || !frameAllowed
        || frameMatches && session.documentId && binding.documentId && binding.documentId !== session.documentId
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The page message does not match its fill session");
      }
      return { binding, frameOrigin, session };
    }

    function onFillConfirm(message, sender) {
      let context;
      try {
        context = sessionForContent(message, sender);
      } catch (error) {
        if (error && error.code === "PASSWORD_CROSS_SITE_FRAME") {
          // Fail closed: a cross-site frame tried to confirm this fill.
          clearSession(String(message.sessionId || ""));
        }
        throw error;
      }
      const { binding, frameOrigin, session } = context;
      if (session.state !== "awaiting-confirmation") {
        throw bridgeError("PASSWORD_SESSION_STATE", "The fill offer is not awaiting confirmation");
      }
      if (binding.frameId !== session.frameId) {
        // The login form lives in a trusted iframe: bind the session to the
        // confirming frame so credentials are delivered only there.
        session.frameId = binding.frameId;
        // A child document has its own Firefox document id. Retain that id for
        // subsequent frame-bound messages such as fill-cancel.
        session.documentId = binding.documentId;
      }
      session.frameOrigin = frameOrigin;
      session.state = "confirmed";
      renewSession(session);
      postEvent("fillConfirm", {
        documentId: binding.documentId,
        entryId: session.entryId,
        frameId: binding.frameId,
        frameOrigin,
        offerId: session.offerId,
        origin: session.origin,
        sessionId: session.sessionId,
        tabId: session.tabId,
      });
      return { confirmed: true };
    }

    function onFillCancel(message, sender) {
      const { frameOrigin, session } = sessionForContent(message, sender);
      postEvent("fillResult", {
        frameId: session.frameId,
        frameOrigin,
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
      const origin = await topLevelOrigin(binding, sender);
      if (!origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The username-stage tab has no HTTP origin");
      }
      const claimedOrigin = templates.exactOrigin(message.origin);
      if (binding.frameId === 0 && claimedOrigin !== origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The username-stage origin is invalid");
      }
      if (binding.frameId > 0 && !templates.frameOriginAllowed(origin, binding.origin)) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The username-stage frame is cross-site");
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
      const claimedOrigin = templates.exactOrigin(value.origin);
      // origin is the top-level origin; frameOrigin is the submitting frame's
      // own origin and must agree with the browser-reported sender URL.
      const frameOrigin = value.frameOrigin == null
        ? binding.origin
        : templates.exactOrigin(value.frameOrigin);
      if (frameOrigin !== binding.origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login candidate frame origin is invalid");
      }
      const origin = await topLevelOrigin(binding, sender);
      if (!origin) {
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login candidate tab has no HTTP origin");
      }
      if (binding.frameId === 0) {
        if (claimedOrigin !== binding.origin || claimedOrigin !== origin) {
          throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login candidate origin is invalid");
        }
      } else if (!templates.frameOriginAllowed(origin, frameOrigin)) {
        // A cross-site iframe must never attach credentials to the top site.
        throw bridgeError("PASSWORD_ORIGIN_MISMATCH", "The login candidate came from a cross-site frame");
      }
      if (origin.startsWith("http://") && !captureInsecureOrigins.has(origin)) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "HTTP login detection was not enabled for this origin");
      }
      if (frameOrigin.startsWith("http://") && !captureInsecureOrigins.has(frameOrigin)) {
        throw bridgeError("PASSWORD_INSECURE_ORIGIN", "HTTP login detection was not enabled for this frame");
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
        frameOrigin,
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
      return { accepted: true, candidateId, expiresInMs: CANDIDATE_TTL_MS, origin };
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
          // Keep promptOrigin compatible with desktop builds that require it
          // to match the top-level vault origin. The exact prompt frame is
          // independently bound by tabId/frameId/documentId above.
          promptOrigin: record.origin,
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
        promptOrigin: record.origin,
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
      let origin = "";
      try {
        origin = templates.exactOrigin(tab.url);
      } catch (_error) {
        origin = "";
      }
      if (!origin) {
        origin = context ? context.origin : "";
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
      const active = await cachedPopupAccount(entryId);
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

    async function cachedPopupAccount(entryId) {
      const active = await activeTabContext();
      if (active.tabId == null) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "No active browser tab is available");
      }
      const cached = tabAccounts.get(active.tabId);
      const account = cached && cached.accounts.find((item) => item.entryId === entryId);
      if (
        !cached
        || cached.locked
        || !account
        || !active.origin
        || cached.origin !== active.origin
      ) {
        throw bridgeError("PASSWORD_TARGET_MISMATCH", "The requested account is not available on this tab");
      }
      return { ...active, account };
    }

    async function popupCopySecret(message) {
      const entryId = requiredString(message.entryId, "entryId", 160);
      const field = String(message.field || "");
      if (field !== "username" && field !== "password" && field !== "mfa") {
        throw bridgeError("PASSWORD_PROTOCOL_INVALID", "The copy field is invalid");
      }
      const active = await cachedPopupAccount(entryId);
      if (field === "mfa") {
        if (active.account.hasMfa !== true) {
          throw bridgeError("PASSWORD_TARGET_MISMATCH", "The requested account has no linked MFA entry");
        }
        const requestId = secureRandomId("mfa-copy");
        const result = new Promise((resolve, reject) => {
          const pending = {
            entryId,
            origin: active.origin,
            reject,
            resolve,
            tabId: active.tabId,
            timer: null,
          };
          pending.timer = setTimeout(() => {
            rejectPendingMfaCopy(
              requestId,
              bridgeError("PASSWORD_REQUEST_EXPIRED", "MFA 验证码复制请求超时"),
            );
          }, MFA_COPY_RESULT_TTL_MS);
          if (pending.timer && typeof pending.timer.unref === "function") pending.timer.unref();
          pendingMfaCopies.set(requestId, pending);
        });
        const posted = postEvent("copyMfaCode", {
          entryId,
          origin: active.origin,
          requestId,
          tabId: active.tabId,
        });
        if (!posted) {
          rejectPendingMfaCopy(
            requestId,
            bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected"),
          );
        }
        return result;
      }
      // The desktop writes the clipboard; credentials never pass the extension.
      const posted = postEvent("copySecret", { entryId, field });
      if (!posted) {
        throw bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected");
      }
      return { accepted: true };
    }

    async function popupDeleteEntry(message) {
      const entryId = requiredString(message.entryId, "entryId", 160);
      const active = await cachedPopupAccount(entryId);
      const requestId = secureRandomId("delete");
      const result = new Promise((resolve, reject) => {
        const pending = {
          entryId,
          origin: active.origin,
          reject,
          resolve,
          tabId: active.tabId,
          timer: null,
        };
        pending.timer = setTimeout(() => {
          rejectPendingDelete(
            requestId,
            bridgeError("PASSWORD_REQUEST_EXPIRED", "删除账户请求超时"),
          );
        }, DELETE_RESULT_TTL_MS);
        if (pending.timer && typeof pending.timer.unref === "function") pending.timer.unref();
        pendingDeletes.set(requestId, pending);
      });
      // The desktop replies only after the vault mutation and badge refresh
      // complete, so the popup cannot mistake event delivery for deletion.
      const posted = postEvent("deleteEntry", {
        entryId,
        origin: active.origin,
        requestId,
        tabId: active.tabId,
      });
      if (!posted) {
        rejectPendingDelete(
          requestId,
          bridgeError("PASSWORD_NATIVE_DISCONNECTED", "PetalDesk native host is disconnected"),
        );
      }
      return result;
    }

    function onPopupMessage(message, sender) {
      // The action popup is an extension page without a tab; content scripts
      // and other extensions must never reach these handlers.
      let expectedPopupUrl = "";
      try {
        expectedPopupUrl = api.runtime && typeof api.runtime.getURL === "function"
          ? new URL(api.runtime.getURL("popup/popup.html")).href
          : "";
      } catch (_error) {
        expectedPopupUrl = "";
      }
      let senderUrl = "";
      try {
        senderUrl = sender && sender.url ? new URL(sender.url).href : "";
      } catch (_error) {
        senderUrl = "";
      }
      if (
        !sender
        || sender.id !== api.runtime.id
        || sender.tab
        || !expectedPopupUrl
        || senderUrl !== expectedPopupUrl
      ) {
        throw bridgeError("PASSWORD_TARGET_INVALID", "The popup message sender is not trusted");
      }
      switch (message.type) {
        case "petaldesk.popup.getState": return popupGetState();
        case "petaldesk.popup.fill": return popupFill(message);
        case "petaldesk.popup.openManager": return popupOpenManager();
        case "petaldesk.popup.copySecret": return popupCopySecret(message);
        case "petaldesk.popup.deleteEntry": return popupDeleteEntry(message);
        default: return null;
      }
    }

    async function onContentMessage(message, sender) {
      switch (message.type) {
        case "petaldesk.password.page-closed": return onPageClosed(sender);
        case "petaldesk.password.tab-ready": return onTabReady(message, sender);
        case "petaldesk.password.frame-state": return onFrameState(message, sender);
        case "petaldesk.password.fill-confirm": return onFillConfirm(message, sender);
        case "petaldesk.password.fill-cancel": return onFillCancel(message, sender);
        case "petaldesk.password.capture-username-stage": return onCaptureUsernameStage(message, sender);
        case "petaldesk.password.capture-submitted": return onCaptureSubmitted(message, sender);
        case "petaldesk.password.capture-success": return onCaptureSuccess(message, sender);
        case "petaldesk.password.save-decision": return onSaveDecision(message, sender);
        case "petaldesk.password.template-recording-progress": return onTemplateRecordingProgress(message, sender);
        case "petaldesk.password.template-recording-cancelled": return onTemplateRecordingCancelled(message, sender);
        case "petaldesk.password.second-factor-candidates": return onSecondFactorCandidates(message, sender);
        case "petaldesk.password.second-factor-confirm": return onSecondFactorConfirm(message, sender);
        case "petaldesk.password.second-factor-cancel": return onSecondFactorCancel(message, sender);
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
        tabFrames.delete(tabId);
        tabAccounts.delete(tabId);
        clearTabState(tabId);
        const journey = secondFactors.get(secondFactorByTab.get(tabId));
        if (journey) removeSecondFactorJourney(journey, { notify: true, reason: "tab-closed" });
        for (const [requestId, pending] of pendingMfaCopies.entries()) {
          if (pending.tabId === tabId) {
            rejectPendingMfaCopy(
              requestId,
              bridgeError("PASSWORD_TARGET_MISMATCH", "The MFA copy tab was closed"),
            );
          }
        }
        for (const [requestId, pending] of pendingDeletes.entries()) {
          if (pending.tabId === tabId) {
            rejectPendingDelete(
              requestId,
              bridgeError("PASSWORD_TARGET_MISMATCH", "The delete request tab was closed"),
            );
          }
        }
        postEvent("pageClosed", { documentId: null, tabId });
      });
    }

    if (typeof api.onActivated === "function") {
      api.onActivated((activeInfo) => {
        const tabId = activeInfo && Number.isInteger(activeInfo.tabId) ? activeInfo.tabId : null;
        if (tabId == null) return;
        // Always trust the live tab URL: the cached origin can be stale when a
        // navigation happened while the desktop channel was down, or when the
        // page moved without a full reload (SPA).
        void liveTabOrigin(tabId).then((origin) => {
          if (origin) {
            const context = tabContexts.get(tabId);
            tabContexts.set(tabId, { documentId: context ? context.documentId : null, origin });
            postEvent("originActive", { origin, tabId });
            return;
          }
          // No fillable origin on this tab, so a badge left over from
          // a previous page must not survive the switch. The desktop drops
          // its tracking entry on an empty origin.
          postEvent("originActive", { origin: "", tabId });
          clearTabAccounts(tabId);
        });
      });
    }

    async function liveTabOrigin(tabId) {
      if (typeof api.getTab === "function") {
        const tab = await api.getTab(tabId).catch(() => null);
        if (tab && tab.url) {
          try {
            return templates.exactOrigin(tab.url);
          } catch (_error) {
            return "";
          }
        }
      }
      const context = tabContexts.get(tabId);
      return context ? context.origin : "";
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
      for (const journey of Array.from(secondFactors.values())) {
        removeSecondFactorJourney(journey, { notify: true, reason: "native-disconnected" });
      }
      clearPendingMfaCopies(
        "PASSWORD_NATIVE_DISCONNECTED",
        "PetalDesk native host is disconnected",
      );
      clearPendingDeletes(
        "PASSWORD_NATIVE_DISCONNECTED",
        "PetalDesk native host is disconnected",
      );
      tabFrames.clear();
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

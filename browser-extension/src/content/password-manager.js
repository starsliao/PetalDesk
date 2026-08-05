(function installPasswordManager(root) {
  "use strict";

  const templates = root.PetalDeskPasswordTemplates;
  const extensionApi = root.browser || root.chrome;
  if (!templates || !extensionApi || !extensionApi.runtime || !root.document) {
    return;
  }

  const MESSAGE_TYPE = "petaldesk.password.command";
  const READY_MESSAGE = "petaldesk.password.tab-ready";
  const FILL_CONFIRM_MESSAGE = "petaldesk.password.fill-confirm";
  const FILL_CANCEL_MESSAGE = "petaldesk.password.fill-cancel";
  const CAPTURE_SUBMITTED_MESSAGE = "petaldesk.password.capture-submitted";
  const CAPTURE_SUCCESS_MESSAGE = "petaldesk.password.capture-success";
  const PAGE_CLOSED_MESSAGE = "petaldesk.password.page-closed";
  const CAPTURE_USERNAME_STAGE_MESSAGE = "petaldesk.password.capture-username-stage";
  const SAVE_DECISION_MESSAGE = "petaldesk.password.save-decision";
  const TEMPLATE_PROGRESS_MESSAGE = "petaldesk.password.template-recording-progress";
  const TEMPLATE_CANCEL_MESSAGE = "petaldesk.password.template-recording-cancelled";
  const CANDIDATE_TTL_MS = 30_000;
  const SUCCESS_SETTLE_MS = 900;
  const OVERLAY_ID = "petaldesk-password-overlay";

  let captureEnabled = false;
  let captureAllowedHttpOrigins = new Set();
  let captureListenerInstalled = false;
  let pendingSubmission = null;
  let activeOffer = null;
  let activeCapturePrompt = null;
  let activeRecording = null;
  let recordingListenerInstalled = false;
  let overlay = null;
  const candidateTimers = new Map();

  function errorMessage(error) {
    return error instanceof Error ? error.message : String(error || "Unknown error");
  }

  function isTopFrame() {
    try {
      return root.top === root;
    } catch (_error) {
      return false;
    }
  }

  function currentOrigin() {
    return templates.exactOrigin(root.location.href);
  }

  function originIsAllowed(requestedOrigin, { allowInsecureHttp = false } = {}) {
    let origin;
    try {
      origin = templates.exactOrigin(requestedOrigin || root.location.href);
    } catch (_error) {
      return false;
    }
    if (origin !== currentOrigin()) return false;
    if (origin.startsWith("http://")) {
      return allowInsecureHttp || captureAllowedHttpOrigins.has(origin);
    }
    return origin.startsWith("https://");
  }

  function randomId(prefix) {
    if (root.crypto && typeof root.crypto.randomUUID === "function") {
      return `${prefix}-${root.crypto.randomUUID()}`;
    }
    return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  }

  function safeString(value, maxLength = 512) {
    const normalized = String(value == null ? "" : value);
    return normalized.length > maxLength ? normalized.slice(0, maxLength) : normalized;
  }

  function insecureOriginWarning(origin) {
    return String(origin).startsWith("http://")
      ? "\n警告：这是 HTTP 明文连接，仅在你明确信任的内网站点使用。"
      : "";
  }

  function sendBackground(message) {
    try {
      if (typeof extensionApi.runtime.sendMessage !== "function") {
        return Promise.reject(new Error("WebExtension messaging is unavailable"));
      }
      if (root.browser && root.browser.runtime) {
        return Promise.resolve(extensionApi.runtime.sendMessage(message));
      }
      return new Promise((resolve, reject) => {
        extensionApi.runtime.sendMessage(message, (response) => {
          const lastError = extensionApi.runtime.lastError;
          if (lastError) {
            reject(new Error(lastError.message || "WebExtension messaging failed"));
            return;
          }
          resolve(response);
        });
      });
    } catch (error) {
      return Promise.reject(error);
    }
  }

  function notifyDocumentClosed() {
    if (!isTopFrame()) return;
    // The background keeps the authoritative candidate copy. A pagehide
    // notification clears this document's password candidate even when the
    // browser keeps the tab alive for a navigation or bfcache transition.
    void sendBackground({ type: PAGE_CLOSED_MESSAGE }).catch(() => {});
  }

  function removeOverlay() {
    if (overlay && overlay.isConnected) {
      overlay.remove();
    }
    overlay = null;
  }

  function clearActiveOffer() {
    if (activeOffer && activeOffer.timer != null) clearTimeout(activeOffer.timer);
    activeOffer = null;
  }

  function createOverlay(title, message, actions) {
    removeOverlay();
    const host = root.document.createElement("div");
    host.id = OVERLAY_ID;
    host.style.cssText = [
      "all: initial",
      "position: fixed",
      "z-index: 2147483647",
      "top: 18px",
      "right: 18px",
      "width: min(360px, calc(100vw - 36px))",
      "font-family: system-ui, sans-serif",
    ].join(";");
    const shadow = typeof host.attachShadow === "function" ? host.attachShadow({ mode: "closed" }) : host;
    const style = root.document.createElement("style");
    style.textContent = `
      :host { all: initial; }
      .panel { box-sizing: border-box; color: #17202a; background: #fff; border: 1px solid #c9d2dc;
        border-radius: 8px; box-shadow: 0 8px 28px rgb(0 0 0 / 22%); padding: 14px; }
      h2 { font: 600 15px/1.4 system-ui, sans-serif; margin: 0 0 7px; }
      p { font: 13px/1.45 system-ui, sans-serif; margin: 5px 0; overflow-wrap: anywhere; }
      .origin { color: #566573; font-size: 11px; }
      .buttons { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
      button { border: 1px solid #9aa8b5; border-radius: 5px; background: #f7f9fb; color: #17202a;
        cursor: pointer; font: 600 12px system-ui, sans-serif; padding: 7px 10px; }
      button.primary { background: #1769aa; border-color: #1769aa; color: #fff; }
    `;
    const panel = root.document.createElement("div");
    panel.className = "panel";
    const heading = root.document.createElement("h2");
    heading.textContent = title;
    const body = root.document.createElement("p");
    body.textContent = message;
    panel.append(heading, body);
    const buttonRow = root.document.createElement("div");
    buttonRow.className = "buttons";
    for (const action of actions) {
      const button = root.document.createElement("button");
      button.type = "button";
      button.textContent = action.label;
      if (action.primary) button.className = "primary";
      button.addEventListener("click", () => action.onClick());
      buttonRow.appendChild(button);
    }
    panel.appendChild(buttonRow);
    shadow.append(style, panel);
    (root.document.documentElement || root.document.body).appendChild(host);
    overlay = host;
    return host;
  }

  function stopTemplateRecording({ keepOverlay = false } = {}) {
    if (recordingListenerInstalled) {
      root.document.removeEventListener("click", onTemplateRecordingClick, true);
      recordingListenerInstalled = false;
    }
    activeRecording = null;
    if (!keepOverlay) removeOverlay();
  }

  function showTemplateRecordingOverlay(message = "") {
    if (!activeRecording) return;
    const hasUsername = activeRecording.usernameSelectors.length > 0;
    const hasPassword = activeRecording.passwordSelectors.length > 0;
    const instruction = message || (
      !hasUsername
        ? "请点击登录页面中的用户名或邮箱输入框。"
        : !hasPassword
          ? "用户名字段已记录。请点击密码输入框；两步登录可先手动进入密码页面。"
          : "站点模板已记录，正在保存到飞花。"
    );
    createOverlay(
      "飞花模板录制",
      `${activeRecording.origin}\n${instruction}\n录制只保存字段选择器，不读取输入内容。`,
      [
        {
          label: "取消录制",
          onClick: () => {
            if (!activeRecording) return;
            const { origin, sessionId } = activeRecording;
            stopTemplateRecording();
            void sendBackground({
              type: TEMPLATE_CANCEL_MESSAGE,
              origin,
              sessionId,
            }).catch(() => {});
          },
        },
      ],
    );
  }

  function onTemplateRecordingClick(event) {
    if (!activeRecording || activeRecording.pending || !isTopFrame()) return;
    const input = event && event.target;
    if (!input || String(input.tagName || "").toLowerCase() !== "input") return;
    const type = String(input.type || input.getAttribute && input.getAttribute("type") || "text").toLowerCase();
    const field = type === "password" ? "password" : "username";
    let selector;
    try {
      selector = templates.recordedSelectorForInput(root.document, input, field);
    } catch (error) {
      showTemplateRecordingOverlay(errorMessage(error));
      return;
    }
    if (typeof event.preventDefault === "function") event.preventDefault();
    if (typeof event.stopImmediatePropagation === "function") event.stopImmediatePropagation();
    else if (typeof event.stopPropagation === "function") event.stopPropagation();

    const recording = activeRecording;
    recording.pending = true;
    recording[`${field}Selectors`] = [selector];
    showTemplateRecordingOverlay();
    void sendBackground({
      type: TEMPLATE_PROGRESS_MESSAGE,
      field,
      origin: recording.origin,
      selector,
      sessionId: recording.sessionId,
    }).then((response) => {
      if (!activeRecording || activeRecording.sessionId !== recording.sessionId) return;
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "Template recording was rejected");
      }
      if (response && response.completed === true) {
        stopTemplateRecording();
        return;
      }
      activeRecording.pending = false;
      showTemplateRecordingOverlay();
    }).catch((error) => {
      if (!activeRecording || activeRecording.sessionId !== recording.sessionId) return;
      activeRecording.pending = false;
      activeRecording[`${field}Selectors`] = [];
      showTemplateRecordingOverlay(errorMessage(error));
    });
  }

  function startTemplateRecording(payload) {
    if (!isTopFrame()) throw new Error("Template recording is available only in the top-level page");
    const origin = templates.exactOrigin(payload.origin);
    if (!originIsAllowed(origin, { allowInsecureHttp: payload.allowInsecureHttp === true })) {
      throw new Error("The template recording origin does not match the current page");
    }
    const sessionId = safeString(payload.sessionId, 160);
    if (!sessionId) throw new Error("A template recording session is required");
    const usernameSelectors = Array.isArray(payload.usernameSelectors)
      ? payload.usernameSelectors.map((value) => templates.normalizeRecordedSelector(value)).slice(0, 1)
      : [];
    const passwordSelectors = Array.isArray(payload.passwordSelectors)
      ? payload.passwordSelectors.map((value) => templates.normalizeRecordedSelector(value)).slice(0, 1)
      : [];
    stopTemplateRecording();
    activeRecording = {
      origin,
      passwordSelectors,
      pending: false,
      sessionId,
      usernameSelectors,
    };
    root.document.addEventListener("click", onTemplateRecordingClick, true);
    recordingListenerInstalled = true;
    showTemplateRecordingOverlay();
    return { origin, sessionId, state: "recording" };
  }

  function fillInputValue(input, value) {
    const descriptor = Object.getOwnPropertyDescriptor(
      Object.getPrototypeOf(input),
      "value",
    );
    if (descriptor && typeof descriptor.set === "function") {
      descriptor.set.call(input, value);
    } else {
      input.value = value;
    }
    if (typeof input.dispatchEvent === "function") {
      const EventConstructor = root.Event || Event;
      input.dispatchEvent(new EventConstructor("input", { bubbles: true, composed: true }));
      input.dispatchEvent(new EventConstructor("change", { bubbles: true, composed: true }));
    }
  }

  function offerMatches(payload) {
    if (!activeOffer || !payload || typeof payload !== "object") return false;
    if (String(payload.sessionId || "") !== activeOffer.sessionId) return false;
    if (String(payload.offerId || "") !== activeOffer.offerId) return false;
    if (!originIsAllowed(payload.origin, { allowInsecureHttp: activeOffer.allowInsecureHttp })) {
      return false;
    }
    return Date.now() < activeOffer.expiresAt;
  }

  function showFillOffer(payload) {
    if (!payload || typeof payload !== "object") throw new Error("A fill offer is required");
    const origin = templates.exactOrigin(payload.origin);
    if (!originIsAllowed(origin, { allowInsecureHttp: payload.allowInsecureHttp === true })) {
      throw new Error("The fill offer origin does not match the current page");
    }
    if (Object.prototype.hasOwnProperty.call(payload, "password")) {
      throw new Error("Fill offers must not contain credentials");
    }
    const sessionId = safeString(payload.sessionId, 160);
    const offerId = safeString(payload.offerId || randomId("offer"), 160);
    if (!sessionId) throw new Error("A fill session is required");
    let userTemplate = null;
    if (payload.userTemplate) {
      userTemplate = templates.normalizeUserTemplate(payload.userTemplate, origin);
    }
    clearActiveOffer();
    const offer = {
      allowInsecureHttp: payload.allowInsecureHttp === true,
      entryId: safeString(payload.entryId, 160),
      expiresAt: Date.now() + 2 * 60 * 1_000,
      offerId,
      sessionId,
      userTemplate,
      username: safeString(payload.username, 1_024),
      confirmed: false,
      timer: null,
    };
    offer.timer = setTimeout(() => {
      if (activeOffer !== offer) return;
      clearActiveOffer();
      removeOverlay();
    }, 2 * 60 * 1_000);
    activeOffer = offer;
    createOverlay(
      "飞花密码管理器",
      `当前页面：${origin}\n账户：${activeOffer.username || "未提供"}${insecureOriginWarning(origin)}\n是否填充？不会自动提交表单。`,
      [
        {
          label: "填充",
          primary: true,
          onClick: () => {
            if (!activeOffer || !offerMatches({ sessionId, offerId, origin })) return;
            activeOffer.confirmed = true;
            removeOverlay();
            void sendBackground({
              type: FILL_CONFIRM_MESSAGE,
              sessionId,
              offerId,
              origin,
            }).catch(() => {});
          },
        },
        {
          label: "取消",
          onClick: () => {
            if (activeOffer && offerMatches({ sessionId, offerId, origin })) {
              clearActiveOffer();
              void sendBackground({
                type: FILL_CANCEL_MESSAGE,
                sessionId,
                offerId,
                origin,
              }).catch(() => {});
            }
            removeOverlay();
          },
        },
      ],
    );
    return { offerId, state: "awaiting-confirmation", origin, sessionId };
  }

  function fillSecret(payload) {
    if (!activeOffer || !activeOffer.confirmed || !offerMatches(payload)) {
      if (payload && typeof payload === "object") {
        if (Object.prototype.hasOwnProperty.call(payload, "password")) payload.password = "";
        if (Object.prototype.hasOwnProperty.call(payload, "username")) payload.username = "";
      }
      throw new Error("The fill request is no longer awaiting credentials");
    }
    let password = String(payload.password || "");
    let username = payload.username == null ? activeOffer.username : String(payload.username);
    try {
      if (!password || password.length > 4_096) throw new Error("The password is invalid");
      const fields = templates.identifyLoginFields(root.document, {
        origin: currentOrigin(),
        userTemplate: activeOffer.userTemplate,
      });
      const genericUsernameUnsafe = fields.source === "generic"
        && fields.usernameField
        && username
        && fields.usernameScore < 30;
      const multiplePasswordsAmbiguous = fields.passwordFields.length > 1
        && !fields.passwordTemplateMatchUnique;
      if (fields.ambiguous || genericUsernameUnsafe || multiplePasswordsAmbiguous) {
        throw new Error("The login fields are ambiguous; record a site template before filling");
      }
      if (!fields.passwordField && !(fields.usernameField && username)) {
        throw new Error("No login fields were found; record a site template before filling");
      }
      let filledUsername = false;
      let filledPassword = false;
      if (fields.usernameField && username) {
        fillInputValue(fields.usernameField, username);
        filledUsername = true;
      }
      if (fields.passwordField) {
        fillInputValue(fields.passwordField, password);
        filledPassword = true;
      }
      return {
        filledPassword,
        filledUsername,
        needsNextStep: Boolean(!filledPassword && filledUsername),
        origin: currentOrigin(),
        sessionId: activeOffer.sessionId,
        submitted: false,
      };
    } finally {
      password = "";
      username = "";
      payload.password = "";
      if (Object.prototype.hasOwnProperty.call(payload, "username")) payload.username = "";
      clearActiveOffer();
      removeOverlay();
    }
  }

  function candidateValues(scope) {
    let fields;
    try {
      fields = templates.identifyLoginFields(scope || root.document, { origin: currentOrigin() });
    } catch (_error) {
      return null;
    }
    const passwordFields = fields.passwordFields || [];
    const passwordField = passwordFields.length > 1
      ? passwordFields[passwordFields.length - 1]
      : fields.passwordField;
    const username = fields.usernameField ? String(fields.usernameField.value || "") : "";
    const password = passwordField ? String(passwordField.value || "") : "";
    if ((!password && !username) || password.length > 4_096 || username.length > 1_024) return null;
    return {
      confidence: fields.ambiguous ? "low" : fields.confidence,
      origin: currentOrigin(),
      password,
      source: fields.source,
      stage: fields.stage,
      username,
    };
  }

  function candidateSucceeded(beforeUrl, form, passwordField) {
    const urlChanged = String(root.location.href) !== beforeUrl;
    const stillAttached = form && typeof form.contains === "function" && passwordField
      ? form.contains(passwordField)
      : true;
    const fields = (() => {
      try {
        return templates.identifyLoginFields(root.document, { origin: currentOrigin() });
      } catch (_error) {
        return null;
      }
    })();
    const passwordVisible = Boolean(fields && fields.passwordField);
    return urlChanged || !stillAttached || !passwordVisible;
  }

  function clearCandidate(candidateId) {
    const timer = candidateTimers.get(candidateId);
    if (timer != null) clearTimeout(timer);
    candidateTimers.delete(candidateId);
  }

  function scheduleCandidateExpiry(candidateId, ttlMs = CANDIDATE_TTL_MS) {
    clearCandidate(candidateId);
    const delay = Math.max(1, Number.isFinite(Number(ttlMs)) ? Number(ttlMs) : CANDIDATE_TTL_MS);
    candidateTimers.set(candidateId, setTimeout(() => {
      clearCandidate(candidateId);
      if (activeCapturePrompt && activeCapturePrompt.candidateId === candidateId) {
        activeCapturePrompt = null;
      }
      if (overlay && overlay.isConnected) removeOverlay();
    }, delay));
  }

  function showCapturePrompt(candidate, confidence, notice = "") {
    const isUpdate = candidate.suggestedAction === "update";
    const id = candidate.candidateId;
    const choices = Array.isArray(candidate.accountChoices) ? candidate.accountChoices : [];
    const title = confidence === "low"
      ? "请确认是否登录成功"
      : isUpdate ? "更新飞花中的密码？" : "保存登录信息到飞花？";
    activeCapturePrompt = { ...activeCapturePrompt, ...candidate, confidence };
    if (activeCapturePrompt.savePending) {
      createOverlay(
        "正在保存到飞花",
        `${candidate.origin}\n请稍候，保存结果确认后才会清除这条登录信息。${insecureOriginWarning(candidate.origin)}`,
        [],
      );
      return;
    }
    const actions = [];
    if (choices.length > 0 && candidate.suggestedAction === "select") {
      for (const choice of choices) {
        const label = choice.username
          ? `更新 ${choice.username}`
          : `更新 ${choice.siteName || "此账户"}`;
        actions.push({
          label,
          primary: actions.length === 0,
          onClick: () => submitCaptureDecision(id, "replace", choice.entryId),
        });
      }
    } else if (choices.length > 0 && candidate.suggestedAction === "new") {
      actions.push({
        label: "保存为新账户",
        primary: true,
        onClick: () => submitCaptureDecision(id, "new"),
      });
      for (const choice of choices) {
        const label = choice.username
          ? `更新 ${choice.username}`
          : `更新 ${choice.siteName || "此账户"}`;
        actions.push({
          label,
          onClick: () => submitCaptureDecision(id, "replace", choice.entryId),
        });
      }
    } else if (candidate.suggestedAction === "new" || candidate.suggestedAction === "update") {
      actions.push({
        label: isUpdate ? "更新到飞花" : "保存到飞花",
        primary: true,
        onClick: () => submitCaptureDecision(id, candidate.suggestedAction),
      });
    }
    if (notice) {
      actions.unshift({ label: "关闭", onClick: () => { clearCandidate(id); activeCapturePrompt = null; removeOverlay(); } });
    } else {
      actions.push({
        label: "忽略",
        onClick: () => {
          clearCandidate(id);
          activeCapturePrompt = null;
          removeOverlay();
          void sendBackground({ type: SAVE_DECISION_MESSAGE, candidateId: id, action: "ignore" }).catch(() => {});
        },
      });
    }
    createOverlay(
      title,
      `${candidate.origin}\n账户：${candidate.username || "请选择要更新的账户"}${notice ? `\n${notice}` : ""}${insecureOriginWarning(candidate.origin)}`,
      actions,
    );
  }

  function showLockedPrompt(origin) {
    createOverlay(
      "飞花密码库已锁定",
      `${origin}\n请先在飞花密码管理器中解锁密码库，然后再保存或更新这条登录信息。`,
      [{ label: "关闭", onClick: removeOverlay }],
    );
  }

  function showCaptureSuccess(candidate, action) {
    const title = action === "update" || action === "replace"
      ? "已更新到飞花"
      : "已保存到飞花";
    const message = `${candidate.origin}\n${title}。密码不会自动提交。`;
    const notice = createOverlay(title, message, [
      { label: "关闭", onClick: removeOverlay },
    ]);
    const timer = setTimeout(() => {
      if (overlay === notice) removeOverlay();
    }, 3_000);
    if (timer && typeof timer.unref === "function") timer.unref();
  }

  function submitCaptureDecision(candidateId, action, entryId = null) {
    if (!activeCapturePrompt || activeCapturePrompt.candidateId !== candidateId || activeCapturePrompt.savePending) return;
    activeCapturePrompt.savePending = true;
    showCapturePrompt(activeCapturePrompt, activeCapturePrompt.confidence);
    void sendBackground({
      type: SAVE_DECISION_MESSAGE,
      candidateId,
      action,
      ...(entryId ? { entryId } : {}),
    }).then((response) => {
      if (response && response.ok === false) {
        const error = new Error(response.error && response.error.message || "保存到飞花失败");
        error.code = response.error && response.error.code;
        throw error;
      }
    }).catch((error) => {
      if (!activeCapturePrompt || activeCapturePrompt.candidateId !== candidateId) return;
      activeCapturePrompt.savePending = false;
      const disconnected = error && error.code === "PASSWORD_NATIVE_DISCONNECTED";
      if (disconnected) {
        clearCandidate(candidateId);
        activeCapturePrompt = null;
        removeOverlay();
        return;
      }
      showCapturePrompt(activeCapturePrompt, activeCapturePrompt.confidence, errorMessage(error));
    });
  }

  function completeCandidate(candidateId) {
    if (!captureEnabled || !pendingSubmission || pendingSubmission.candidateId !== candidateId) return;
    const submission = pendingSubmission;
    pendingSubmission = null;
    const confidence = candidateSucceeded(
      submission.beforeUrl,
      submission.form,
      submission.passwordField,
    ) ? submission.confidence : "low";
    void sendBackground({
      type: CAPTURE_SUCCESS_MESSAGE,
      candidateId,
      confidence,
      origin: submission.origin,
    }).catch(() => clearCandidate(candidateId));
  }

  function scheduleCandidate(form) {
    if (!captureEnabled || !form || pendingSubmission) return;
    const values = candidateValues(form);
    if (!values) return;
    if (!values.password && values.username) {
      const staged = {
        origin: values.origin,
        type: CAPTURE_USERNAME_STAGE_MESSAGE,
        username: values.username,
      };
      void sendBackground(staged).finally(() => {
        staged.username = "";
        values.username = "";
      });
      return;
    }
    const candidateId = randomId("candidate");
    const candidate = { ...values, candidateId };
    pendingSubmission = {
      beforeUrl: String(root.location.href),
      candidateId,
      confidence: values.confidence,
      form,
      origin: values.origin,
      passwordField: templates.identifyLoginFields(form, { origin: currentOrigin() }).passwordField,
    };
    scheduleCandidateExpiry(candidateId);
    const submitted = sendBackground({ type: CAPTURE_SUBMITTED_MESSAGE, candidate });
    candidate.password = "";
    values.password = "";
    void submitted.then((response) => {
      if (response && response.ok === false) {
        const error = new Error(response.error && response.error.message || "Login capture was rejected");
        error.code = response.error && response.error.code;
        throw error;
      }
      activeCapturePrompt = {
        candidateId,
        confidence: values.confidence,
        origin: values.origin,
        suggestedAction: null,
        username: values.username,
      };
    }).catch((error) => {
      if (pendingSubmission && pendingSubmission.candidateId === candidateId) pendingSubmission = null;
      clearCandidate(candidateId);
      if (error && error.code === "PASSWORD_USERNAME_UNKNOWN") {
        createOverlay(
          "无法确定要更新的账户",
          `${values.origin}\n请在飞花密码管理器中手动选择对应账户并更新密码。`,
          [{ label: "关闭", onClick: removeOverlay }],
        );
      }
    });
    setTimeout(() => completeCandidate(candidateId), SUCCESS_SETTLE_MS);
  }

  function onSubmit(event) {
    if (!captureEnabled || !isTopFrame()) return;
    const form = event && event.target && typeof event.target.querySelectorAll === "function"
      ? event.target
      : root.document;
    scheduleCandidate(form);
  }

  function onClick(event) {
    if (!captureEnabled || !isTopFrame()) return;
    const target = event && event.target;
    if (!target || typeof target.closest !== "function") return;
    const submit = target.closest('button[type="submit"], input[type="submit"], button[role="button"]');
    if (!submit) return;
    const form = submit.form || (typeof submit.closest === "function" ? submit.closest("form") : null);
    scheduleCandidate(form || root.document);
  }

  function startCapture(payload = {}) {
    let origin;
    try {
      origin = currentOrigin();
    } catch (_error) {
      return false;
    }
    if (origin.startsWith("http://") && !captureAllowedHttpOrigins.has(origin)) return false;
    captureEnabled = true;
    if (!captureListenerInstalled) {
      root.document.addEventListener("submit", onSubmit, true);
      root.document.addEventListener("click", onClick, true);
      captureListenerInstalled = true;
    }
    return true;
  }

  function stopCapture() {
    captureEnabled = false;
    pendingSubmission = null;
    activeCapturePrompt = null;
    for (const candidateId of candidateTimers.keys()) clearCandidate(candidateId);
    if (captureListenerInstalled) {
      root.document.removeEventListener("submit", onSubmit, true);
      root.document.removeEventListener("click", onClick, true);
      captureListenerInstalled = false;
    }
    if (overlay && overlay.id === OVERLAY_ID) removeOverlay();
    return true;
  }

  function handleCommand(command, payload = {}) {
    switch (command) {
      case "fillOffer":
        return showFillOffer(payload);
      case "fillSecret":
        return fillSecret(payload);
      case "fillCancel":
        if (activeOffer && offerMatches(payload)) clearActiveOffer();
        removeOverlay();
        return { cancelled: true };
      case "captureEnable":
        captureAllowedHttpOrigins = new Set(
          Array.isArray(payload.insecureOrigins)
            ? payload.insecureOrigins.map((value) => templates.exactOrigin(value)).filter((value) => value.startsWith("http://"))
            : [],
        );
        return { enabled: startCapture(payload) };
      case "captureDisable":
        return { enabled: !stopCapture() };
      case "captureMatch":
        if (!payload.candidateId) return { matched: false };
        if (payload.action === "same") {
          clearCandidate(payload.candidateId);
          activeCapturePrompt = null;
          removeOverlay();
          return { matched: true, dismissed: true };
        }
        if (payload.action === "locked") {
          const candidateId = safeString(payload.candidateId, 160);
          clearCandidate(candidateId);
          activeCapturePrompt = null;
          showLockedPrompt(templates.exactOrigin(payload.origin));
          return { matched: true, dismissed: true, action: payload.action };
        }
        if (payload.action === "username-required") {
          const candidateId = safeString(payload.candidateId, 160);
          clearCandidate(candidateId);
          activeCapturePrompt = null;
          createOverlay(
            "无法确定要更新的账户",
            `${templates.exactOrigin(payload.origin)}\n当前页面没有可识别的用户名，且飞花中没有同 origin 账户。请在飞花密码管理器中手动新增或更新。`,
            [{ label: "关闭", onClick: removeOverlay }],
          );
          return { matched: true, dismissed: true, action: payload.action };
        }
        if (!activeCapturePrompt || activeCapturePrompt.candidateId !== payload.candidateId) {
          activeCapturePrompt = {
            candidateId: safeString(payload.candidateId, 160),
            confidence: payload.confidence === "high" ? "high" : "low",
            origin: templates.exactOrigin(payload.origin),
            suggestedAction: null,
            username: safeString(payload.username, 1_024),
            accountChoices: [],
            savePending: false,
          };
        }
        activeCapturePrompt.suggestedAction = payload.action === "select"
          ? "select"
          : payload.action === "update" ? "update" : "new";
        activeCapturePrompt.accountChoices = Array.isArray(payload.accounts)
          ? payload.accounts.map((choice) => ({
            entryId: safeString(choice && choice.entryId, 160),
            siteName: safeString(choice && choice.siteName, 512),
            username: safeString(choice && choice.username, 1_024),
          })).filter((choice) => choice.entryId)
          : [];
        showCapturePrompt(activeCapturePrompt, activeCapturePrompt.confidence);
        return { matched: true, action: activeCapturePrompt.suggestedAction };
      case "captureSaveResult": {
        const candidateId = safeString(payload.candidateId, 160);
        if (!activeCapturePrompt || activeCapturePrompt.candidateId !== candidateId) {
          return { handled: false };
        }
        if (payload.success === true) {
          clearCandidate(candidateId);
          const completed = activeCapturePrompt || {
            origin: safeString(payload.origin, 512),
          };
          activeCapturePrompt = null;
          showCaptureSuccess(completed, payload.action);
          return { handled: true, success: true };
        }
        scheduleCandidateExpiry(candidateId, payload.expiresInMs);
        activeCapturePrompt.savePending = false;
        const message = payload.error && payload.error.message
          ? safeString(payload.error.message, 512)
          : "保存到飞花失败，请重试。";
        showCapturePrompt(activeCapturePrompt, activeCapturePrompt.confidence, message);
        return { handled: true, success: false };
      }
      case "templateRecordStart":
        return startTemplateRecording(payload);
      case "templateRecordCancel":
        if (
          activeRecording
          && String(payload.sessionId || "") === activeRecording.sessionId
          && templates.exactOrigin(payload.origin) === activeRecording.origin
        ) {
          stopTemplateRecording();
        }
        return { cancelled: true };
      default:
        throw new Error(`Unsupported password command: ${String(command)}`);
    }
  }

  extensionApi.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || message.type !== MESSAGE_TYPE) return false;
    try {
      const result = handleCommand(message.command, message.payload || {});
      sendResponse({ ok: true, result });
    } catch (error) {
      sendResponse({ ok: false, error: { code: "PASSWORD_COMMAND_FAILED", message: errorMessage(error) } });
    }
    return false;
  });

  root.document.addEventListener("visibilitychange", () => {
    if (root.document.visibilityState === "hidden") {
      notifyDocumentClosed();
      pendingSubmission = null;
      for (const candidateId of candidateTimers.keys()) clearCandidate(candidateId);
      activeCapturePrompt = null;
      if (!activeRecording) removeOverlay();
      clearActiveOffer();
    }
  });
  root.addEventListener("pagehide", () => {
    notifyDocumentClosed();
    pendingSubmission = null;
    clearActiveOffer();
    activeCapturePrompt = null;
    for (const candidateId of candidateTimers.keys()) clearCandidate(candidateId);
    stopTemplateRecording();
    removeOverlay();
  });

  if (isTopFrame()) {
    void sendBackground({
      type: READY_MESSAGE,
      origin: currentOrigin(),
    }).then((response) => {
      if (response && response.captureEnabled) {
        captureAllowedHttpOrigins = new Set(response.insecureOrigins || []);
        startCapture(response);
      }
    }).catch(() => {});
  }
})(typeof globalThis !== "undefined" ? globalThis : this);

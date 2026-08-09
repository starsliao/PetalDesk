(function startPopup(root) {
  "use strict";

  const extensionApi = root.browser || root.chrome;
  if (!extensionApi || !extensionApi.runtime || !root.document) {
    return;
  }

  const document = root.document;

  function sendMessage(message) {
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

  function errorMessage(error) {
    return error instanceof Error ? error.message : String(error || "未知错误");
  }

  function clearChildren(element) {
    while (element.firstChild) element.removeChild(element.firstChild);
  }

  function formatTime(value) {
    const time = Number(value);
    if (!Number.isFinite(time) || time <= 0) return "";
    try {
      return new Date(time).toLocaleTimeString("zh-CN", { hour12: false });
    } catch (_error) {
      return "";
    }
  }

  function addDiagnosticRow(list, label, value, healthy) {
    const item = document.createElement("li");
    item.className = "diagnostic-row";
    const name = document.createElement("span");
    name.className = "label";
    name.textContent = label;
    const state = document.createElement("span");
    state.className = healthy === true
      ? "value ok"
      : healthy === false
        ? "value warn"
        : "value";
    state.textContent = value;
    item.append(name, state);
    list.appendChild(item);
    return item;
  }

  function setDiagnosticsStatus(text) {
    const feedback = document.getElementById("diagnostics-status");
    if (feedback) feedback.textContent = text;
  }

  function setToolLauncherStatus(text) {
    const feedback = document.getElementById("tool-launcher-status");
    if (feedback) feedback.textContent = text;
  }

  function setStatusIndicator(id, state, title) {
    const indicator = document.getElementById(id);
    if (!indicator) return;
    indicator.setAttribute("data-state", state);
    indicator.setAttribute("title", title);
    indicator.setAttribute("aria-label", title);
  }

  function loginDetectionModel(diagnostics) {
    const value = diagnostics.loginDetection && typeof diagnostics.loginDetection === "object"
      ? diagnostics.loginDetection
      : {};
    const desiredKnown = typeof value.desiredEnabled === "boolean"
      || typeof diagnostics.captureDesiredEnabled === "boolean";
    const desiredEnabled = typeof value.desiredEnabled === "boolean"
      ? value.desiredEnabled
      : typeof diagnostics.captureDesiredEnabled === "boolean"
        ? diagnostics.captureDesiredEnabled
        : diagnostics.captureEnabled === true;
    const enabled = typeof value.enabled === "boolean"
      ? value.enabled
      : diagnostics.captureEnabled === true;
    const available = typeof value.available === "boolean"
      ? value.available
      : typeof diagnostics.captureAvailable === "boolean"
        ? diagnostics.captureAvailable
        : true;
    const connectionState = ["connecting", "connected", "reconnecting", "disconnected"]
      .includes(value.connectionState)
      ? value.connectionState
      : diagnostics.secretConnected === true || diagnostics.captureEnabled === true
        ? "connected"
        : diagnostics.nativeConnected === false
          ? "disconnected"
          : "connecting";
    const reconnectAttempt = Number.isInteger(Number(value.reconnectAttempt))
      ? Math.max(0, Number(value.reconnectAttempt))
      : 0;
    const reconnectMaxAttempts = Number.isInteger(Number(value.reconnectMaxAttempts))
      && Number(value.reconnectMaxAttempts) > 0
      ? Number(value.reconnectMaxAttempts)
      : 3;
    let status = "已关闭";
    let health = null;
    if (desiredEnabled) {
      if (connectionState === "reconnecting") {
        status = reconnectAttempt > 0
          ? `正在重连（${Math.min(reconnectAttempt, reconnectMaxAttempts)}/${reconnectMaxAttempts}）`
          : "正在重连";
        health = false;
      } else if (connectionState === "connecting") {
        status = "正在连接";
      } else if (connectionState === "disconnected") {
        status = "连接已断开";
        health = false;
      } else if (enabled) {
        status = "已开启";
        health = true;
      } else if (!available) {
        status = "暂不可用";
        health = false;
      } else {
        status = "正在恢复";
      }
    }
    return {
      available,
      canReconnect: value.canReconnect === true && connectionState === "disconnected",
      connectionState,
      desiredKnown,
      desiredEnabled,
      enabled,
      health,
      preferenceBusy: value.preferenceBusy === true,
      reconnectAttempt,
      reconnectMaxAttempts,
      status,
    };
  }

  function helpTip() {
    const wrapper = document.createElement("span");
    wrapper.className = "help-tip";
    const button = document.createElement("button");
    button.type = "button";
    button.className = "help-tip-button";
    button.setAttribute("aria-label", "登录检测说明");
    button.setAttribute("aria-describedby", "login-detection-tooltip");
    button.setAttribute("title", "登录检测说明");
    button.textContent = "?";
    const tooltip = document.createElement("span");
    tooltip.id = "login-detection-tooltip";
    tooltip.className = "help-tooltip";
    tooltip.setAttribute("role", "tooltip");
    tooltip.textContent = "登录检测会识别网页上的登录操作，在发现新账号或密码变化时提示保存或更新。关闭后仍可填充和复制已保存的账号，但不会再提示保存登录信息。";
    wrapper.append(button, tooltip);
    return wrapper;
  }

  function renderLoginDetectionRow(list, diagnostics) {
    const model = loginDetectionModel(diagnostics);
    const item = document.createElement("li");
    item.className = "login-detection-card";
    const summary = document.createElement("div");
    summary.className = "login-detection-summary";
    const title = document.createElement("div");
    title.className = "login-detection-title";
    const label = document.createElement("span");
    label.id = "login-detection-label";
    label.textContent = "登录检测";
    title.append(label, helpTip());
    const state = document.createElement("span");
    state.className = model.health === true
      ? "login-detection-state ok"
      : model.health === false
        ? "login-detection-state warn"
        : "login-detection-state";
    state.setAttribute("role", "status");
    state.textContent = model.status;
    summary.append(title, state);

    const controls = document.createElement("div");
    controls.className = "login-detection-controls";
    const reconnect = document.createElement("button");
    reconnect.type = "button";
    reconnect.className = "reconnect-button";
    reconnect.textContent = "重连";
    reconnect.setAttribute("aria-label", "重新连接登录检测");
    reconnect.hidden = !model.desiredEnabled || !model.canReconnect;
    reconnect.addEventListener("click", () => {
      setDiagnosticsStatus("");
      void runBusy(reconnect, async () => {
        try {
          const response = await sendMessage({ type: "petaldesk.popup.reconnectLoginDetection" });
          if (response && response.ok === false) {
            throw new Error(response.error && response.error.message || "重连请求失败");
          }
          await refresh();
        } catch (error) {
          setDiagnosticsStatus(errorMessage(error));
        }
      });
    });

    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "switch-control";
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-labelledby", "login-detection-label");
    toggle.setAttribute("aria-checked", model.desiredEnabled ? "true" : "false");
    toggle.setAttribute("aria-label", model.desiredEnabled ? "关闭登录检测" : "开启登录检测");
    toggle.setAttribute("title", model.desiredEnabled ? "关闭登录检测" : "开启登录检测");
    toggle.disabled = model.preferenceBusy || model.connectionState !== "connected";
    const thumb = document.createElement("span");
    thumb.className = "switch-thumb";
    toggle.appendChild(thumb);
    toggle.addEventListener("click", () => {
      setDiagnosticsStatus("");
      void runBusy(toggle, async () => {
        try {
          const response = await sendMessage({
            type: "petaldesk.popup.setLoginDetection",
            enabled: !model.desiredEnabled,
          });
          if (response && response.ok === false) {
            throw new Error(response.error && response.error.message || "登录检测设置失败");
          }
          await refresh();
        } catch (error) {
          setDiagnosticsStatus(errorMessage(error));
        }
      });
    });
    controls.append(reconnect, toggle);
    item.append(summary, controls);
    list.appendChild(item);
    return model;
  }

  function renderDiagnostics(diagnostics) {
    const list = document.getElementById("diagnostics-list");
    clearChildren(list);
    addDiagnosticRow(list, "安装权限", "已随安装授予", true);
    addDiagnosticRow(
      list,
      "Native Host 连接",
      diagnostics.nativeConnected ? "已连接" : "未连接",
      diagnostics.nativeConnected === true,
    );
    let channelText = diagnostics.secretConnected ? "已连接" : "未连接";
    if (diagnostics.lastCommandAt) {
      const outcome = diagnostics.lastCommandOk === false ? "失败" : "成功";
      channelText += `；最近命令${outcome} ${formatTime(diagnostics.lastCommandAt)}`;
      if (diagnostics.lastCommandOk === false && diagnostics.lastCommandErrorCode) {
        channelText += `（${diagnostics.lastCommandErrorCode}）`;
      }
    }
    addDiagnosticRow(list, "桌面密码通道", channelText, diagnostics.secretConnected === true);
    const loginDetection = renderLoginDetectionRow(list, diagnostics);
    setStatusIndicator("status-dot-permission", "ok", "安装权限：正常");
    setStatusIndicator(
      "status-dot-native",
      diagnostics.nativeConnected === true ? "ok" : "error",
      `Native Host：${diagnostics.nativeConnected === true ? "正常" : "异常"}`,
    );
    setStatusIndicator(
      "status-dot-secret",
      diagnostics.secretConnected === true ? "ok" : "error",
      `桌面密码通道：${diagnostics.secretConnected === true ? "正常" : "异常"}`,
    );
    const loginDetectionHealthy = loginDetection.desiredKnown
      && loginDetection.desiredEnabled
      && loginDetection.connectionState === "connected"
      && loginDetection.available
      && loginDetection.enabled;
    const loginIndicatorState = loginDetection.desiredKnown
      && !loginDetection.desiredEnabled
      && !loginDetection.enabled
      ? "off"
      : loginDetectionHealthy
        ? "ok"
        : "error";
    const loginIndicatorText = loginIndicatorState === "off"
      ? "主动关闭"
      : loginIndicatorState === "ok"
        ? "正常"
        : `异常（${loginDetection.status}）`;
    setStatusIndicator(
      "status-dot-login",
      loginIndicatorState,
      `登录检测：${loginIndicatorText}`,
    );
    const version = document.getElementById("extension-version");
    version.textContent = diagnostics.extensionVersion ? `v${diagnostics.extensionVersion}` : "";
    return loginDetection;
  }

  function setStatus(text) {
    document.getElementById("site-status").textContent = text;
  }

  async function runBusy(button, action) {
    const wasDisabled = button.disabled === true;
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      return await action();
    } finally {
      button.disabled = wasDisabled;
      button.removeAttribute("aria-busy");
    }
  }

  async function fillAccount(entryId) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.fill", entryId });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "填充请求失败");
      }
      setStatus("选择要填充的账户：");
      return true;
    } catch (error) {
      setStatus(errorMessage(error));
      return false;
    }
  }

  async function copySecret(entryId, field) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.copySecret", entryId, field });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "复制请求失败");
      }
      const successMessages = {
        username: "已复制用户名",
        password: "已复制密码",
      };
      if (field === "mfa") {
        const expiresInSeconds = Number(response && response.expiresInSeconds);
        const expiryText = Number.isFinite(expiresInSeconds) && expiresInSeconds >= 0
          ? `，${Math.ceil(expiresInSeconds)} 秒后过期`
          : "";
        setStatus(`MFA 验证码已复制${expiryText}`);
      } else {
        setStatus(successMessages[field] || "复制成功");
      }
      return true;
    } catch (error) {
      setStatus(errorMessage(error));
      return false;
    }
  }

  async function deleteAccount(entryId) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.deleteEntry", entryId });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "删除请求失败");
      }
      return true;
    } catch (error) {
      setStatus(errorMessage(error));
      return false;
    }
  }

  let activeAccountEntryId = null;
  let accountContextMenu = null;
  let deleteConfirmation = null;
  let accountActionFocusTarget = null;

  function focusAccountActionTarget() {
    if (accountActionFocusTarget && typeof accountActionFocusTarget.focus === "function") {
      accountActionFocusTarget.focus();
    }
  }

  function removeFloatingElement(element) {
    if (element && element.parentElement) {
      element.parentElement.removeChild(element);
    }
  }

  function closeAccountAction(returnFocus = true) {
    if (!activeAccountEntryId && !accountContextMenu && !deleteConfirmation) return;
    removeFloatingElement(accountContextMenu);
    removeFloatingElement(deleteConfirmation);
    accountContextMenu = null;
    deleteConfirmation = null;
    activeAccountEntryId = null;
    if (returnFocus) focusAccountActionTarget();
    accountActionFocusTarget = null;
  }

  function isWithin(element, candidate) {
    let current = candidate;
    while (current) {
      if (current === element) return true;
      current = current.parentElement;
    }
    return false;
  }

  function contextFocusTarget(item, eventTarget, fallback) {
    let current = eventTarget;
    while (current && current !== item) {
      if (current.tagName === "BUTTON" && typeof current.focus === "function") return current;
      current = current.parentElement;
    }
    const active = document.activeElement;
    if (active && isWithin(item, active) && typeof active.focus === "function") return active;
    return fallback;
  }

  function icon(name) {
    const glyph = document.createElement("span");
    glyph.className = `lucide-icon lucide-${name}`;
    glyph.setAttribute("aria-hidden", "true");
    return glyph;
  }

  function iconButton({ label, iconName, className = "", disabled = false }) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `icon-button ${className}`.trim();
    button.disabled = disabled;
    button.setAttribute("aria-label", label);
    button.setAttribute("title", label);
    button.appendChild(icon(iconName));
    return button;
  }

  function renderDeleteConfirm(account) {
    const confirm = document.createElement("div");
    confirm.className = "delete-confirm";
    confirm.setAttribute("role", "dialog");
    confirm.setAttribute("aria-label", "确认删除账户");
    const label = document.createElement("span");
    label.className = "confirm-label";
    label.textContent = "确认删除此账户？";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "confirm-cancel";
    cancel.textContent = "取消";
    cancel.addEventListener("click", () => {
      closeAccountAction(true);
    });
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "confirm-delete";
    remove.textContent = "删除";
    remove.addEventListener("click", () => {
      void runBusy(remove, async () => {
        cancel.disabled = true;
        try {
          const deleted = await deleteAccount(account.entryId);
          if (!deleted) return;
          closeAccountAction(false);
          await refresh();
        } finally {
          cancel.disabled = false;
        }
      });
    });
    confirm.append(label, cancel, remove);
    return confirm;
  }

  function showDeleteConfirmation(account, item) {
    removeFloatingElement(accountContextMenu);
    accountContextMenu = null;
    deleteConfirmation = renderDeleteConfirm(account);
    item.appendChild(deleteConfirmation);
    const cancel = deleteConfirmation.children[1];
    if (cancel && typeof cancel.focus === "function") cancel.focus();
  }

  function renderAccountContextMenu(account, item) {
    const menu = document.createElement("div");
    menu.className = "account-context-menu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("aria-label", "账户操作");
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "context-delete";
    remove.setAttribute("role", "menuitem");
    remove.setAttribute("aria-label", "删除账户");
    remove.setAttribute("aria-haspopup", "dialog");
    remove.textContent = "删除账户";
    remove.addEventListener("click", (event) => {
      if (typeof event.stopPropagation === "function") event.stopPropagation();
      showDeleteConfirmation(account, item);
    });
    menu.appendChild(remove);
    return menu;
  }

  function openAccountContextMenu(account, item, focusTarget) {
    closeAccountAction(false);
    activeAccountEntryId = account.entryId;
    accountActionFocusTarget = focusTarget;
    accountContextMenu = renderAccountContextMenu(account, item);
    item.appendChild(accountContextMenu);
    const firstItem = accountContextMenu.children[0];
    if (firstItem && typeof firstItem.focus === "function") firstItem.focus();
  }

  function renderAccountItem(account, origin) {
    const item = document.createElement("li");
    item.className = "account-card";
    item.setAttribute("data-entry-id", account.entryId);
    item.setAttribute("role", "group");
    item.setAttribute("aria-label", `${account.siteName || origin} 的账户；右键打开账户操作`);
    const fill = document.createElement("button");
    fill.type = "button";
    fill.className = "account-main";
    fill.setAttribute("aria-label", `填充 ${account.siteName || origin} 的账户`);
    const text = document.createElement("span");
    text.className = "account-text";
    const site = document.createElement("span");
    site.className = "site";
    site.textContent = account.siteName || origin;
    const username = document.createElement("span");
    username.className = "username";
    username.textContent = account.username || "（未保存用户名）";
    text.append(site, username);
    fill.appendChild(text);
    fill.addEventListener("click", () => {
      closeAccountAction(false);
      void runBusy(fill, () => fillAccount(account.entryId));
    });

    const actions = document.createElement("div");
    actions.className = "account-actions";
    const usernameCopy = iconButton({ label: "复制用户名", iconName: "user-round" });
    usernameCopy.addEventListener("click", () => {
      closeAccountAction(false);
      void runBusy(usernameCopy, () => copySecret(account.entryId, "username"));
    });
    const passwordCopy = iconButton({ label: "复制密码", iconName: "key-round" });
    passwordCopy.addEventListener("click", () => {
      closeAccountAction(false);
      void runBusy(passwordCopy, () => copySecret(account.entryId, "password"));
    });
    const hasMfa = account.hasMfa === true;
    const mfaCopy = iconButton({
      label: hasMfa ? "复制 MFA 验证码" : "未关联 MFA",
      iconName: "shield-check",
      disabled: !hasMfa,
    });
    if (hasMfa) {
      mfaCopy.addEventListener("click", () => {
        closeAccountAction(false);
        void runBusy(mfaCopy, () => copySecret(account.entryId, "mfa"));
      });
    }
    actions.append(usernameCopy, passwordCopy, mfaCopy);

    item.addEventListener("contextmenu", (event) => {
      if (typeof event.preventDefault === "function") event.preventDefault();
      if (typeof event.stopPropagation === "function") event.stopPropagation();
      openAccountContextMenu(account, item, contextFocusTarget(item, event.target, fill));
    });
    item.addEventListener("keydown", (event) => {
      const opensContextMenu = event.key === "ContextMenu"
        || (event.key === "F10" && event.shiftKey === true);
      if (!opensContextMenu) return;
      if (typeof event.preventDefault === "function") event.preventDefault();
      if (typeof event.stopPropagation === "function") event.stopPropagation();
      openAccountContextMenu(account, item, contextFocusTarget(item, event.target, fill));
    });

    item.append(fill, actions);
    return item;
  }

  function renderSite(tab) {
    tab = tab && typeof tab === "object" ? tab : {};
    const originElement = document.getElementById("site-origin");
    const accountList = document.getElementById("account-list");
    clearChildren(accountList);
    activeAccountEntryId = null;
    accountContextMenu = null;
    deleteConfirmation = null;
    accountActionFocusTarget = null;
    const origin = typeof tab.origin === "string" ? tab.origin : "";
    originElement.textContent = origin || "当前页面不是可填充的网站";
    const accounts = Array.isArray(tab.accounts) ? tab.accounts : [];
    if (!origin) {
      setStatus("切换到登录页面后即可使用密码填充。");
      return;
    }
    if (tab.locked === true) {
      setStatus("密码库已锁定，请在飞花中解锁。");
      return;
    }
    if (accounts.length === 0) {
      setStatus("此站点暂无已存账户。");
      return;
    }
    setStatus("选择要填充的账户：");
    for (const account of accounts) {
      accountList.appendChild(renderAccountItem(account, origin));
    }
  }

  let diagnosticsRefreshTimer = null;

  function scheduleDiagnosticsRefresh(loginDetection) {
    if (diagnosticsRefreshTimer != null) clearTimeout(diagnosticsRefreshTimer);
    diagnosticsRefreshTimer = null;
    const connectionPending = loginDetection
      && ["connecting", "reconnecting", "disconnected"].includes(
        loginDetection.connectionState,
      );
    const effectiveStatePending = loginDetection
      && loginDetection.connectionState === "connected"
      && loginDetection.available
      && !loginDetection.enabled;
    if (
      !loginDetection
      || !loginDetection.desiredEnabled
      || (!connectionPending && !effectiveStatePending)
    ) return;
    const delay = loginDetection.connectionState === "disconnected" ? 2_000 : 600;
    diagnosticsRefreshTimer = setTimeout(() => {
      diagnosticsRefreshTimer = null;
      void refresh();
    }, delay);
    if (diagnosticsRefreshTimer && typeof diagnosticsRefreshTimer.unref === "function") {
      diagnosticsRefreshTimer.unref();
    }
  }

  async function refresh() {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.getState" });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "无法读取扩展状态");
      }
      const loginDetection = renderDiagnostics(
        response && response.diagnostics ? response.diagnostics : {},
      );
      renderSite(response && response.tab ? response.tab : {});
      scheduleDiagnosticsRefresh(loginDetection);
    } catch (error) {
      renderDiagnostics({});
      renderSite({});
      scheduleDiagnosticsRefresh(null);
      setStatus(errorMessage(error));
    }
  }

  for (const launcher of [
    { id: "open-main", label: "主界面", target: "main" },
    { id: "open-password-manager", label: "密码", target: "passwords" },
    { id: "open-mfa", label: "MFA", target: "mfa" },
    { id: "open-notes", label: "便签", target: "notes" },
  ]) {
    const button = document.getElementById(launcher.id);
    if (!button) continue;
    button.addEventListener("click", () => {
      setToolLauncherStatus("");
      void runBusy(button, async () => {
        try {
          const response = await sendMessage({
            type: "petaldesk.popup.openTool",
            target: launcher.target,
          });
          if (response && response.ok === false) {
            throw new Error(response.error && response.error.message || `无法打开${launcher.label}`);
          }
        } catch (error) {
          setToolLauncherStatus(errorMessage(error));
        }
      });
    });
  }

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || (!activeAccountEntryId && !accountContextMenu && !deleteConfirmation)) return;
    if (typeof event.preventDefault === "function") event.preventDefault();
    closeAccountAction(true);
  });

  document.addEventListener("pointerdown", (event) => {
    if (!activeAccountEntryId && !accountContextMenu && !deleteConfirmation) return;
    const target = event && event.target;
    if (isWithin(accountContextMenu, target) || isWithin(deleteConfirmation, target)) return;
    closeAccountAction(true);
  });

  void refresh();
})(typeof globalThis !== "undefined" ? globalThis : this);

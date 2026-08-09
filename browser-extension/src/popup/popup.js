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
    addDiagnosticRow(
      list,
      "登录检测",
      diagnostics.captureEnabled ? "已开启" : "已关闭",
      diagnostics.captureEnabled === true,
    );
    const version = document.getElementById("extension-version");
    version.textContent = diagnostics.extensionVersion ? `v${diagnostics.extensionVersion}` : "";
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
        mfa: "MFA 验证码已复制，过期后自动清除",
      };
      setStatus(successMessages[field] || "复制成功");
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

  async function refresh() {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.getState" });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "无法读取扩展状态");
      }
      renderDiagnostics(response && response.diagnostics ? response.diagnostics : {});
      renderSite(response && response.tab ? response.tab : {});
    } catch (error) {
      renderDiagnostics({});
      renderSite({});
      setStatus(errorMessage(error));
    }
  }

  document.getElementById("open-manager").addEventListener("click", () => {
    void sendMessage({ type: "petaldesk.popup.openManager" }).catch(() => {});
  });

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

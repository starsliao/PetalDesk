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

  async function fillAccount(entryId) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.fill", entryId });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "填充请求失败");
      }
      setStatus("填充请求已发送，请查看页面。");
    } catch (error) {
      setStatus(errorMessage(error));
    }
  }

  async function copySecret(entryId, field) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.copySecret", entryId, field });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "复制请求失败");
      }
      setStatus(field === "password" ? "已复制密码。" : "已复制用户名。");
    } catch (error) {
      setStatus(errorMessage(error));
    }
  }

  async function deleteAccount(entryId) {
    try {
      const response = await sendMessage({ type: "petaldesk.popup.deleteEntry", entryId });
      if (response && response.ok === false) {
        throw new Error(response.error && response.error.message || "删除请求失败");
      }
    } catch (error) {
      setStatus(errorMessage(error));
      return;
    }
    await refresh();
  }

  let lastTab = {};
  let menuEntryId = null;
  let confirmDeleteEntryId = null;

  function rerenderAccounts() {
    renderSite(lastTab);
  }

  function renderAccountMenu(account) {
    const menu = document.createElement("div");
    menu.className = "account-menu";
    for (const action of [
      { field: "username", label: "复制账号" },
      { field: "password", label: "复制密码" },
    ]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = action.label;
      button.addEventListener("click", () => {
        menuEntryId = null;
        rerenderAccounts();
        void copySecret(account.entryId, action.field);
      });
      menu.appendChild(button);
    }
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "删除账号";
    remove.addEventListener("click", () => {
      menuEntryId = null;
      confirmDeleteEntryId = account.entryId;
      rerenderAccounts();
    });
    menu.appendChild(remove);
    return menu;
  }

  function renderDeleteConfirm(account) {
    const confirm = document.createElement("div");
    confirm.className = "account-menu confirm";
    const label = document.createElement("span");
    label.className = "confirm-label";
    label.textContent = "确认删除？";
    const yes = document.createElement("button");
    yes.type = "button";
    yes.className = "danger";
    yes.textContent = "删除";
    yes.addEventListener("click", () => {
      confirmDeleteEntryId = null;
      rerenderAccounts();
      void deleteAccount(account.entryId);
    });
    const no = document.createElement("button");
    no.type = "button";
    no.textContent = "取消";
    no.addEventListener("click", () => {
      confirmDeleteEntryId = null;
      rerenderAccounts();
    });
    confirm.append(label, yes, no);
    return confirm;
  }

  function renderAccountItem(account, origin) {
    const item = document.createElement("li");
    const row = document.createElement("div");
    row.className = "account-row";
    const button = document.createElement("button");
    button.type = "button";
    button.className = "account";
    const site = document.createElement("span");
    site.className = "site";
    site.textContent = account.siteName || origin;
    const username = document.createElement("span");
    username.className = "username";
    username.textContent = account.username || "（未保存用户名）";
    button.append(site, username);
    button.addEventListener("click", () => {
      void fillAccount(account.entryId);
    });
    const trigger = document.createElement("button");
    trigger.type = "button";
    trigger.className = "menu-trigger";
    trigger.textContent = "⋯";
    trigger.setAttribute("aria-label", "账户操作");
    trigger.setAttribute("title", "账户操作");
    trigger.addEventListener("click", () => {
      menuEntryId = menuEntryId === account.entryId ? null : account.entryId;
      confirmDeleteEntryId = null;
      rerenderAccounts();
    });
    row.append(button, trigger);
    item.appendChild(row);
    if (confirmDeleteEntryId === account.entryId) {
      item.appendChild(renderDeleteConfirm(account));
    } else if (menuEntryId === account.entryId) {
      item.appendChild(renderAccountMenu(account));
    }
    return item;
  }

  function renderSite(tab) {
    lastTab = tab && typeof tab === "object" ? tab : {};
    const originElement = document.getElementById("site-origin");
    const accountList = document.getElementById("account-list");
    clearChildren(accountList);
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

  void refresh();
})(typeof globalThis !== "undefined" ? globalThis : this);

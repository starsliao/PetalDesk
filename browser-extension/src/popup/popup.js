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
      setStatus("请在页面中确认填充。");
    } catch (error) {
      setStatus(errorMessage(error));
    }
  }

  function renderSite(tab) {
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
      const item = document.createElement("li");
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
      item.appendChild(button);
      accountList.appendChild(item);
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

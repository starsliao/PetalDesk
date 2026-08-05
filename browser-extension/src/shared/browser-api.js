(function exposeBrowserApi(root) {
  "use strict";

  const extensionApi = root.browser || root.chrome;
  if (!extensionApi || !extensionApi.runtime || !extensionApi.tabs) {
    throw new Error("WebExtension runtime APIs are unavailable");
  }

  const usesPromiseApi = Boolean(root.browser && root.browser.runtime);

  function chromeLastError() {
    return root.chrome && root.chrome.runtime ? root.chrome.runtime.lastError : null;
  }

  function callbackCall(invoke) {
    return new Promise((resolve, reject) => {
      invoke((value) => {
        const lastError = chromeLastError();
        if (lastError) {
          reject(new Error(lastError.message || "WebExtension API call failed"));
          return;
        }
        resolve(value);
      });
    });
  }

  async function queryTabs(queryInfo) {
    if (usesPromiseApi) {
      return extensionApi.tabs.query(queryInfo);
    }
    return callbackCall((done) => extensionApi.tabs.query(queryInfo, done));
  }

  async function sendTabMessage(tabId, message, options) {
    if (usesPromiseApi) {
      return extensionApi.tabs.sendMessage(tabId, message, options);
    }
    return callbackCall((done) =>
      extensionApi.tabs.sendMessage(tabId, message, options, done),
    );
  }

  async function createTab(createProperties) {
    if (usesPromiseApi) {
      return extensionApi.tabs.create(createProperties);
    }
    return callbackCall((done) => extensionApi.tabs.create(createProperties, done));
  }

  async function getTab(tabId) {
    if (usesPromiseApi) {
      return extensionApi.tabs.get(tabId);
    }
    return callbackCall((done) => extensionApi.tabs.get(tabId, done));
  }

  async function queryAllTabs(queryInfo = {}) {
    return queryTabs(queryInfo);
  }

  async function queryActiveTab() {
    const tabs = await queryTabs({ active: true, currentWindow: true });
    return Array.isArray(tabs) && tabs.length > 0 ? tabs[0] : null;
  }

  function onTabRemoved(listener) {
    if (!extensionApi.tabs || !extensionApi.tabs.onRemoved
      || typeof extensionApi.tabs.onRemoved.addListener !== "function") {
      return false;
    }
    extensionApi.tabs.onRemoved.addListener(listener);
    return true;
  }

  function onActivated(listener) {
    if (!extensionApi.tabs || !extensionApi.tabs.onActivated
      || typeof extensionApi.tabs.onActivated.addListener !== "function") {
      return false;
    }
    extensionApi.tabs.onActivated.addListener(listener);
    return true;
  }

  async function getAllPermissions() {
    if (!extensionApi.permissions || typeof extensionApi.permissions.getAll !== "function") {
      return {};
    }
    if (usesPromiseApi) {
      return extensionApi.permissions.getAll();
    }
    return callbackCall((done) => extensionApi.permissions.getAll(done));
  }

  async function requestPermissions(permissions) {
    if (!extensionApi.permissions || typeof extensionApi.permissions.request !== "function") {
      return false;
    }
    if (usesPromiseApi) {
      return extensionApi.permissions.request(permissions);
    }
    return callbackCall((done) => extensionApi.permissions.request(permissions, done));
  }

  function connectNative(hostName) {
    return extensionApi.runtime.connectNative(hostName);
  }

  function consumeRuntimeLastError() {
    const lastError = extensionApi.runtime.lastError;
    return lastError ? lastError.message || String(lastError) : null;
  }

  function detectBrowserFamily() {
    const userAgent = String(root.navigator && root.navigator.userAgent);
    if (/Firefox\//i.test(userAgent) || usesPromiseApi) {
      return "firefox";
    }
    if (/Edg\//i.test(userAgent)) {
      return "edge";
    }
    return "chrome";
  }

  root.PetalDeskBrowserApi = Object.freeze({
    browserFamily: detectBrowserFamily(),
    connectNative,
    consumeRuntimeLastError,
    createTab,
    extensionId: extensionApi.runtime.id,
    extensionVersion: extensionApi.runtime.getManifest().version,
    getAllPermissions,
    getTab,
    onActivated,
    onTabRemoved,
    queryActiveTab,
    queryAllTabs,
    requestPermissions,
    queryTabs,
    permissions: extensionApi.permissions || null,
    runtime: extensionApi.runtime,
    sendTabMessage,
    action: extensionApi.action || extensionApi.browserAction || null,
  });
})(typeof globalThis !== "undefined" ? globalThis : this);

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
    extensionId: extensionApi.runtime.id,
    extensionVersion: extensionApi.runtime.getManifest().version,
    queryTabs,
    runtime: extensionApi.runtime,
    sendTabMessage,
  });
})(typeof globalThis !== "undefined" ? globalThis : this);

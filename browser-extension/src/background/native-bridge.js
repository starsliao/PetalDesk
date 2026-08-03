(function startNativeBridge(root) {
  "use strict";

  const api = root.PetalDeskBrowserApi;
  if (!api) {
    throw new Error("PetalDesk browser API adapter was not loaded");
  }

  const HOST_NAME = "com.petaldesk.capture";
  const PROTOCOL_VERSION = 1;
  const COMMANDS = new Set([
    "prepare",
    "start",
    "step",
    "status",
    "restore",
    "cancel",
  ]);
  const MAX_RECONNECT_DELAY_MS = 30_000;

  let nativePort = null;
  let reconnectTimer = null;
  let reconnectDelayMs = 1_000;
  const activeSessions = new Map();
  let defaultSessionKey = null;
  const passwordBridge = api.browserFamily === "firefox"
    && root.PetalDeskPasswordBridge
    ? root.PetalDeskPasswordBridge.createPasswordBridge({
      api,
      postToNative,
      protocolVersion: PROTOCOL_VERSION,
    })
    : null;

  function errorMessage(error) {
    return error instanceof Error ? error.message : String(error || "Unknown error");
  }

  function postToNative(message) {
    if (!nativePort) {
      return false;
    }

    try {
      nativePort.postMessage(message);
      return true;
    } catch (error) {
      console.warn("PetalDesk native response failed", error);
      return false;
    }
  }

  async function activeTabId() {
    const tabs = await api.queryTabs({ active: true, currentWindow: true });
    const tab = Array.isArray(tabs) ? tabs[0] : null;
    if (!tab || !Number.isInteger(tab.id)) {
      throw new Error("No active browser tab is available");
    }
    return tab.id;
  }

  function optionalRoutingId(value, name) {
    if (value == null) {
      return null;
    }
    const parsed = Number(value);
    if (!Number.isInteger(parsed) || parsed < 0) {
      throw new Error(`${name} must be a non-negative integer`);
    }
    return parsed;
  }

  async function routeRequest(request) {
    if (!request || typeof request !== "object") {
      throw new Error("Native request must be an object");
    }

    const command = String(request.command || "");
    if (command === "ping") {
      return {
        protocolVersion: PROTOCOL_VERSION,
        browser: api.browserFamily,
        extensionId: api.extensionId,
      };
    }

    if (passwordBridge && passwordBridge.supportsCommand(command)) {
      return passwordBridge.route(request);
    }

    if (!COMMANDS.has(command)) {
      throw new Error(`Unsupported capture command: ${command || "<empty>"}`);
    }

    const payload =
      request.payload && typeof request.payload === "object" ? request.payload : {};
    const requestedTabId = optionalRoutingId(request.tabId ?? payload.tabId, "tabId");
    const requestedFrameId = optionalRoutingId(
      request.frameId ?? payload.frameId,
      "frameId",
    );
    const boundSession = defaultSessionKey
      ? activeSessions.get(defaultSessionKey) || null
      : null;
    const useBoundSession = command !== "prepare" && requestedTabId == null && boundSession;
    const tabId = requestedTabId ?? (useBoundSession ? boundSession.tabId : await activeTabId());
    const frameId = requestedFrameId
      ?? (useBoundSession ? boundSession.frameId : 0);

    const response = await api.sendTabMessage(
      tabId,
      {
        type: "petaldesk.capture.command",
        command,
        payload,
      },
      { frameId },
    );

    if (!response || response.ok !== true) {
      throw new Error(
        response && response.error
          ? response.error.message || String(response.error)
          : "Capture content script did not return a response",
      );
    }

    const sessionKey = `${tabId}:${frameId}`;
    if (command === "prepare") {
      activeSessions.set(sessionKey, { tabId, frameId });
      defaultSessionKey = sessionKey;
    } else if (command === "restore" || command === "cancel") {
      activeSessions.delete(sessionKey);
      if (defaultSessionKey === sessionKey) {
        defaultSessionKey = null;
      }
    }

    return {
      tabId,
      frameId,
      ...response.result,
    };
  }

  function cancelActiveSessions() {
    const sessions = Array.from(activeSessions.values());
    activeSessions.clear();
    defaultSessionKey = null;
    for (const activeSession of sessions) {
      void api.sendTabMessage(
        activeSession.tabId,
        {
          type: "petaldesk.capture.command",
          command: "cancel",
          payload: { reason: "native-host-disconnected" },
        },
        { frameId: activeSession.frameId },
      ).catch(() => {});
    }
  }

  async function onNativeMessage(request) {
    reconnectDelayMs = 1_000;
    if (request && request.type === "extension.event") {
      if (request.event === "secretDisconnected") {
        cancelActiveSessions();
        if (passwordBridge) passwordBridge.disconnect();
      }
      return;
    }
    const id = request && request.id != null ? request.id : null;
    try {
      const requestVersion = request && (request.protocolVersion ?? request.version);
      if (requestVersion != null && requestVersion !== PROTOCOL_VERSION) {
        throw new Error(`Unsupported native protocol version: ${requestVersion}`);
      }
      const result = await routeRequest(request);
      postToNative({
        protocolVersion: PROTOCOL_VERSION,
        type: "extension.response",
        id,
        ok: true,
        result,
      });
    } catch (error) {
      postToNative({
        protocolVersion: PROTOCOL_VERSION,
        type: "extension.response",
        id,
        ok: false,
        error: {
          code: error && error.code ? error.code : "CAPTURE_COMMAND_FAILED",
          message: errorMessage(error),
        },
      });
    }
  }

  function scheduleReconnect() {
    if (reconnectTimer != null) {
      return;
    }

    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, reconnectDelayMs);
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, MAX_RECONNECT_DELAY_MS);
  }

  function connect() {
    if (nativePort) {
      return;
    }

    try {
      const port = api.connectNative(HOST_NAME);
      nativePort = port;
      port.onMessage.addListener(onNativeMessage);
      port.onDisconnect.addListener(() => {
        api.consumeRuntimeLastError();
        nativePort = null;
        cancelActiveSessions();
        if (passwordBridge) {
          passwordBridge.disconnect();
        }
        scheduleReconnect();
      });
      postToNative({
        protocolVersion: PROTOCOL_VERSION,
        type: "extension.ready",
        browser: api.browserFamily,
        extensionVersion: api.extensionVersion,
        extensionId: api.extensionId,
        capabilities: [
          ...Array.from(COMMANDS),
          ...(passwordBridge ? Array.from(passwordBridge.capabilities) : []),
        ],
      });
    } catch (error) {
      nativePort = null;
      console.warn("PetalDesk native host connection failed", error);
      scheduleReconnect();
    }
  }

  api.runtime.onStartup.addListener(connect);
  api.runtime.onInstalled.addListener(connect);
  connect();
})(typeof globalThis !== "undefined" ? globalThis : this);

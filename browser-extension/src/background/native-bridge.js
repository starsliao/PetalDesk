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
  const MAX_FAST_RECONNECT_ATTEMPTS = 3;
  const FAST_RECONNECT_DELAYS_MS = Object.freeze([0, 350, 900]);
  const RECONNECT_HANDSHAKE_TIMEOUT_MS = 2_500;
  const BACKGROUND_RECONNECT_DELAY_MS = 30_000;

  let nativePort = null;
  let reconnectTimer = null;
  let handshakeTimer = null;
  let backgroundReconnectTimer = null;
  let reconnectAttempt = 0;
  let recoveryMode = null;
  let connectionState = "connecting";
  let secretConnected = api.browserFamily !== "firefox";
  let passwordChannelDisconnected = false;
  const activeSessions = new Map();
  let defaultSessionKey = null;
  const passwordBridge = api.browserFamily === "firefox"
    && root.PetalDeskPasswordBridge
    ? root.PetalDeskPasswordBridge.createPasswordBridge({
      api,
      postToNative,
      protocolVersion: PROTOCOL_VERSION,
      requestNativeReconnect,
    })
    : null;

  function errorMessage(error) {
    return error instanceof Error ? error.message : String(error || "Unknown error");
  }

  function postToNative(message) {
    const port = nativePort;
    if (!port) {
      return false;
    }

    try {
      port.postMessage(message);
      return true;
    } catch (error) {
      console.warn("PetalDesk native response failed", error);
      handleNativePortLost(port);
      return false;
    }
  }

  function connectionSnapshot() {
    return {
      canReconnect: connectionState === "disconnected",
      connected: nativePort != null,
      reconnectAttempt,
      reconnectMaxAttempts: MAX_FAST_RECONNECT_ATTEMPTS,
      secretConnected,
      state: connectionState,
    };
  }

  function publishConnectionState() {
    if (!passwordBridge) return;
    const snapshot = connectionSnapshot();
    if (typeof passwordBridge.setNativeConnectionState === "function") {
      passwordBridge.setNativeConnectionState(snapshot);
      return;
    }
    if (typeof passwordBridge.setNativeConnected === "function") {
      passwordBridge.setNativeConnected(snapshot.connected);
    }
  }

  function clearReconnectTimer() {
    if (reconnectTimer != null) clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  function clearHandshakeTimer() {
    if (handshakeTimer != null) clearTimeout(handshakeTimer);
    handshakeTimer = null;
  }

  function clearBackgroundReconnectTimer() {
    if (backgroundReconnectTimer != null) clearTimeout(backgroundReconnectTimer);
    backgroundReconnectTimer = null;
  }

  function unrefTimer(timer) {
    if (timer && typeof timer.unref === "function") timer.unref();
  }

  function closeNativePort() {
    const port = nativePort;
    nativePort = null;
    if (port && typeof port.disconnect === "function") {
      try {
        port.disconnect();
      } catch (_error) {
        // The port may already be closed. Its stale disconnect callback is
        // ignored because nativePort no longer points at this instance.
      }
    }
  }

  function notifyPasswordDisconnected() {
    cancelActiveSessions();
    if (passwordChannelDisconnected) return;
    passwordChannelDisconnected = true;
    if (passwordBridge) passwordBridge.disconnect();
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
    if (request && request.type === "extension.event") {
      if (request.event === "secretDisconnected") {
        secretConnected = false;
        notifyPasswordDisconnected();
        publishConnectionState();
        // A newly-created native host announces the unavailable pipe before it
        // starts its own handshake loop. During an active recovery attempt the
        // watchdog gives that loop a short chance to reconnect. Outside a
        // recovery attempt, rebuild the complete Native Messaging port now.
        if (recoveryMode == null) beginFastReconnect();
      } else if (request.event === "secretConnected") {
        markConnectionHealthy();
      }
      return;
    }
    const id = request && request.id != null ? request.id : null;
    try {
      const requestVersion = request && (request.protocolVersion ?? request.version);
      if (requestVersion != null && requestVersion !== PROTOCOL_VERSION) {
        throw new Error(`Unsupported native protocol version: ${requestVersion}`);
      }
      // A protocol-compatible request necessarily crossed both the Native
      // Messaging port and the desktop secret pipe, so it is a successful
      // reconnect signal even if the requested command itself later fails.
      markConnectionHealthy();
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

  function markConnectionHealthy() {
    const wasSecretConnected = secretConnected;
    clearReconnectTimer();
    clearHandshakeTimer();
    clearBackgroundReconnectTimer();
    reconnectAttempt = 0;
    recoveryMode = null;
    connectionState = "connected";
    secretConnected = true;
    passwordChannelDisconnected = false;
    publishConnectionState();
    if (
      !wasSecretConnected
      && passwordBridge
      && typeof passwordBridge.onSecretConnected === "function"
    ) {
      void passwordBridge.onSecretConnected();
    }
  }

  function armHandshakeWatchdog(mode) {
    clearHandshakeTimer();
    handshakeTimer = setTimeout(() => {
      handshakeTimer = null;
      if (secretConnected) return;
      if (mode === "fast" && recoveryMode === "fast") {
        failFastReconnectAttempt();
      } else if (mode === "slow" && recoveryMode === "slow") {
        finishBackgroundReconnectAttempt();
      } else if (mode === "initial" && recoveryMode == null) {
        beginFastReconnect();
      }
    }, RECONNECT_HANDSHAKE_TIMEOUT_MS);
    unrefTimer(handshakeTimer);
  }

  function openNativePort(mode) {
    if (nativePort) return true;
    try {
      const port = api.connectNative(HOST_NAME);
      nativePort = port;
      port.onMessage.addListener((request) => {
        if (nativePort !== port) return;
        void onNativeMessage(request);
      });
      port.onDisconnect.addListener(() => {
        api.consumeRuntimeLastError();
        handleNativePortLost(port);
      });
      publishConnectionState();
      armHandshakeWatchdog(mode);
      const readyPosted = postToNative({
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
      if (readyPosted && nativePort === port && api.browserFamily !== "firefox") {
        markConnectionHealthy();
      }
      return readyPosted && nativePort === port;
    } catch (error) {
      nativePort = null;
      console.warn("PetalDesk native host connection failed", error);
      publishConnectionState();
      return false;
    }
  }

  function handleNativePortLost(port) {
    if (nativePort !== port) return;
    nativePort = null;
    clearHandshakeTimer();
    secretConnected = false;
    notifyPasswordDisconnected();
    publishConnectionState();
    if (recoveryMode === "fast") {
      failFastReconnectAttempt();
    } else if (recoveryMode === "slow") {
      finishBackgroundReconnectAttempt();
    } else {
      beginFastReconnect();
    }
  }

  function scheduleFastReconnectAttempt() {
    clearReconnectTimer();
    if (reconnectAttempt >= MAX_FAST_RECONNECT_ATTEMPTS) {
      finishFastReconnectCycle();
      return;
    }
    reconnectAttempt += 1;
    connectionState = "reconnecting";
    publishConnectionState();
    const delay = FAST_RECONNECT_DELAYS_MS[reconnectAttempt - 1] || 0;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (recoveryMode !== "fast" || secretConnected) return;
      closeNativePort();
      publishConnectionState();
      const openingAttempt = reconnectAttempt;
      if (
        !openNativePort("fast")
        && recoveryMode === "fast"
        && reconnectAttempt === openingAttempt
      ) {
        failFastReconnectAttempt();
      }
    }, delay);
    unrefTimer(reconnectTimer);
  }

  function failFastReconnectAttempt() {
    if (recoveryMode !== "fast") return;
    clearHandshakeTimer();
    closeNativePort();
    secretConnected = false;
    publishConnectionState();
    if (reconnectAttempt >= MAX_FAST_RECONNECT_ATTEMPTS) {
      finishFastReconnectCycle();
      return;
    }
    scheduleFastReconnectAttempt();
  }

  function finishFastReconnectCycle() {
    clearReconnectTimer();
    clearHandshakeTimer();
    closeNativePort();
    recoveryMode = null;
    reconnectAttempt = MAX_FAST_RECONNECT_ATTEMPTS;
    connectionState = "disconnected";
    secretConnected = false;
    publishConnectionState();
    scheduleBackgroundReconnect();
  }

  function beginFastReconnect() {
    if (recoveryMode === "fast") return connectionSnapshot();
    clearReconnectTimer();
    clearHandshakeTimer();
    clearBackgroundReconnectTimer();
    closeNativePort();
    recoveryMode = "fast";
    reconnectAttempt = 0;
    connectionState = "reconnecting";
    secretConnected = false;
    notifyPasswordDisconnected();
    publishConnectionState();
    scheduleFastReconnectAttempt();
    return connectionSnapshot();
  }

  function scheduleBackgroundReconnect() {
    if (backgroundReconnectTimer != null || secretConnected) return;
    backgroundReconnectTimer = setTimeout(() => {
      backgroundReconnectTimer = null;
      if (secretConnected || recoveryMode != null) return;
      recoveryMode = "slow";
      closeNativePort();
      publishConnectionState();
      if (!openNativePort("slow")) finishBackgroundReconnectAttempt();
    }, BACKGROUND_RECONNECT_DELAY_MS);
    unrefTimer(backgroundReconnectTimer);
  }

  function finishBackgroundReconnectAttempt() {
    if (recoveryMode !== "slow") return;
    clearHandshakeTimer();
    closeNativePort();
    recoveryMode = null;
    reconnectAttempt = MAX_FAST_RECONNECT_ATTEMPTS;
    connectionState = "disconnected";
    secretConnected = false;
    publishConnectionState();
    scheduleBackgroundReconnect();
  }

  function requestNativeReconnect() {
    if (connectionState === "connected" && secretConnected) {
      return connectionSnapshot();
    }
    return beginFastReconnect();
  }

  function connect() {
    if (nativePort || recoveryMode === "fast") return;
    if (connectionState === "disconnected" || recoveryMode === "slow") {
      requestNativeReconnect();
      return;
    }
    connectionState = "connecting";
    reconnectAttempt = 0;
    secretConnected = api.browserFamily !== "firefox";
    if (!openNativePort("initial")) beginFastReconnect();
  }

  api.runtime.onStartup.addListener(connect);
  api.runtime.onInstalled.addListener(connect);
  connect();
})(typeof globalThis !== "undefined" ? globalThis : this);

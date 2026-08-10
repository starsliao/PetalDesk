"use strict";

importScripts(
  chrome.runtime.getURL("shared/browser-api.js"),
  chrome.runtime.getURL("shared/password-templates.js"),
  chrome.runtime.getURL("background/password-bridge.js"),
  chrome.runtime.getURL("background/native-bridge.js"),
);

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const extensionRoot = path.join(__dirname, "..");

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(path.join(extensionRoot, relativePath), "utf8"));
}

function extensionIdFromKey(key) {
  const digest = crypto.createHash("sha256").update(Buffer.from(key, "base64")).digest();
  const alphabet = "abcdefghijklmnop";
  let id = "";
  for (const byte of digest.subarray(0, 16)) {
    id += alphabet[byte >>> 4];
    id += alphabet[byte & 0x0f];
  }
  return id;
}

test("Chromium manifest exposes the same password UI and content scripts as Firefox", () => {
  const chromium = readJson("manifest/chromium.json");
  const firefox = readJson("manifest/firefox.json");

  assert.equal(chromium.manifest_version, 3);
  assert.deepEqual(chromium.permissions, firefox.permissions);
  assert.deepEqual(chromium.host_permissions, firefox.host_permissions);
  assert.deepEqual(chromium.action, firefox.action);
  assert.deepEqual(chromium.icons, firefox.icons);
  assert.deepEqual(chromium.content_scripts, firefox.content_scripts);
  assert.equal(chromium.background.service_worker, "background/chromium-entry.js");
});

test("Chromium service worker loads the shared password bridge before Native Messaging", () => {
  const entry = fs.readFileSync(
    path.join(extensionRoot, "src/background/chromium-entry.js"),
    "utf8",
  );
  const orderedFiles = [
    "shared/browser-api.js",
    "shared/password-templates.js",
    "background/password-bridge.js",
    "background/native-bridge.js",
  ];
  let previous = -1;
  for (const file of orderedFiles) {
    const index = entry.indexOf(file);
    assert.ok(index > previous, `${file} must be loaded in dependency order`);
    previous = index;
  }
});

test("Chromium manifest key fixes the unpacked extension ID", () => {
  const chromium = readJson("manifest/chromium.json");
  assert.equal(
    extensionIdFromKey(chromium.key),
    "eclmlkbklcpcaijloflmohfpeldcbjkp",
  );
});

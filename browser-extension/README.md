# 飞花 - PetalDesk 浏览器长截图扩展

This directory contains the shared WebExtension implementation used by the
飞花 - PetalDesk long-capture browser enhancement. The extension has no runtime npm
dependencies. Release packaging downloads the pinned `web-ext@10.5.0` tool via
`npx` for AMO validation, archives, and optional signing.

## Build and test

From this directory:

```powershell
npm test
npm run build
npm run package:firefox
```

The build creates two unpacked extensions:

- `dist/chromium`: Chrome and Edge Manifest V3 extension.
- `dist/firefox`: Firefox Manifest V3 extension with the stable Gecko ID
  `petaldesk-capture@petaldesk.app`.

`npm run package:firefox` validates the Firefox build and creates a versioned
`dist/artifacts/PetalDesk_Firefox_AMO-upload_*.zip`. This ZIP is for AMO upload;
it is not a signed, user-installable extension. To request an unlisted signed
XPI from AMO, set `AMO_JWT_ISSUER` and `AMO_JWT_SECRET`, then run:

```powershell
.\scripts\package-firefox.ps1 -Sign
```

The signed output is written to `dist/signed/*-signed.xpi`. Chrome/Edge store
publication and their production extension IDs remain separate release steps.
The Firefox manifest declares AMO data collection permission `websiteActivity`
because it sends scroll geometry to the registered Native Messaging host. That
host is on the same machine; the extension does not send page content to a
remote service.

## Capture protocol

The background process connects to the Native Messaging host
`com.petaldesk.capture`. It opens protocol v1 with an `extension.ready` message.
Native requests use this envelope:

```json
{
  "protocolVersion": 1,
  "type": "command",
  "id": "request-id",
  "command": "prepare",
  "tabId": 123,
  "frameId": 0,
  "payload": {
    "anchor": { "x": 640, "y": 400 }
  }
}
```

Responses use
`{ "protocolVersion": 1, "type": "extension.response", "id": "...", "ok": true }`.
Supported commands are `prepare`, `start`, `step`, `status`, `restore`, and
`cancel`. `ping` is handled directly by the background process. When `tabId`
is omitted, the active tab in the current window is used. `frameId` defaults to
the top frame.

The content script:

1. Locates the nearest vertically scrollable ancestor under the anchor.
2. Saves the original scroll offset and inline `scroll-behavior` value.
3. Pauses CSS animation and transition effects during capture.
4. Leaves fixed and sticky elements visible for the first frame, then hides
   and later restores their inline visibility.
5. Waits for animation frames and a quiet DOM interval after each scroll.
6. Reports actual scroll movement and bottom-of-container state.

`restore` and `cancel` both reverse every page mutation and return the original
scroll position. Browser-restricted pages fail at message delivery so the
desktop application can fall back to its general capture engine.

Native host templates and Windows registration instructions are under
`native-host/`.

# 飞花 - PetalDesk 浏览器增强扩展

This directory contains the shared WebExtension implementation used by the
飞花 - PetalDesk browser enhancement. Firefox v1 adds the password-manager
companion to the existing long-capture feature. The extension has no runtime
npm dependencies. Release packaging downloads the pinned `web-ext@10.5.0` tool via
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
- `dist/firefox`: Firefox Manifest V3 extension with long capture and password
  features, using the stable Gecko ID `petaldesk-capture@petaldesk.app`.

`npm run package:firefox` validates the Firefox build and creates a versioned
`dist/artifacts/PetalDesk_Firefox_AMO-upload_*.zip`. This ZIP is for a public AMO
listed upload; it is not a signed, user-installable extension. The same command
also creates `PetalDesk_Firefox_AMO-source_*.zip` with readable source, tests,
build scripts, and reviewer fixtures. API signing is optional and requires an
explicit channel. After setting `AMO_JWT_ISSUER` and
`AMO_JWT_SECRET`, run one of:

```powershell
.\scripts\package-firefox.ps1 -Sign -Channel Listed
.\scripts\package-firefox.ps1 -Sign -Channel Unlisted
```

Unlisted signing downloads an XPI under `dist/signed`. Listed API submission may
not return an XPI until Mozilla makes it available. Without `-Sign`, packaging
never requires AMO credentials or performs an external submission. Chrome/Edge
password publication remains a later release step.

The Firefox manifest declares required `websiteActivity` for scroll geometry
and required `authenticationInfo` for usernames/passwords; both are granted at
installation time. The toolbar action opens a popup with connection
diagnostics and the accounts PetalDesk offers for the active tab. The Native
Messaging host is on the same machine; the extension does not send data to a
remote service. AMO listing, privacy, permission, and reviewer drafts are in
`amo/`.

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

## Password protocol

Firefox advertises `password-fill` and `password-capture` plus these commands:

- `password.open`
- `password.offerFill`
- `password.offerFillDirect`
- `password.provideCredentials`
- `password.cancelFill`
- `password.requestConsent`
- `password.setCaptureEnabled`
- `password.captureMatch`
- `password.saveResult`
- `password.resolveCapture`
- `password.startTemplateRecording`
- `password.cancelTemplateRecording`
- `password.getStatus`
- `password.updateBadge`

`password.requestConsent` is a no-op compatibility probe: authentication
access is a required install-time permission, so it always reports a granted
state. `password.updateBadge` caches up to 16 account summaries
(`entryId`/`username`/`siteName`, never secrets) for one tab, mirrors the
count into the toolbar badge, and backs the action popup. The popup talks to
the background over `petaldesk.popup.*` runtime messages (`getState`, `fill`,
`openManager`, `copySecret`, `deleteEntry`) that only accept the extension's
own popup sender; `copySecret` and `deleteEntry` are forwarded to the desktop
as bare events so credentials never pass through the extension.

Password events use
`{ "type": "extension.event", "event": "...", "payload": { ... } }`.
The event names are `tabReady`, `fillConfirm`, `fillResult`,
`captureCandidate`, `saveDecision`, `originActive`, `fillRequest`,
`openPasswordManager`, `copySecret`, `deleteEntry`, `templateRecordingReady`,
`templateRecordingProgress`, `templateRecordingResult`, and
`templateRecordingCancelled`.
`password.captureMatch` can return `new`, `update`, `same`, `select`,
`username-required`, or `locked`; a `select` or `new` decision with accounts
is completed by a bound `replace` save action, and `locked` dismisses the
candidate with an unlock notice. `password.saveResult` keeps a failed
candidate available for a short retry window and clears it only after a
confirmed success.

The two-phase fill protocol is mandatory: `password.offerFill` contains no
password; after the page produces `fillConfirm`, the host sends one
`password.provideCredentials` command. Fill offers carry `direct: true`, so
the content script confirms immediately without a page overlay and shows a
short auto-dismissing notice once the fields are filled; the overlay is still
used for template recording. `password.offerFillDirect` skips
`password.open` by binding an existing tab and exact origin into a
ready session, then follows the same offer and confirmation steps. The
request is bound to one session, tab, document, and exact origin. Fill offers
are broadcast to every frame of the tab: a frame answers only when it is the
offer origin itself or same-site with it (e.g. the `dl.reg.163.com` login
iframe inside `mail.163.com`) and contains login fields, so the confirming
same-site frame becomes the sole `fillSecret` target. Login capture follows
the same rule: a same-site iframe reports candidates with the top-level
origin plus its own `frameOrigin`, and cross-site frames are discarded.
Content scripts fill fields
and dispatch `input`/`change` events but never submit the form. A submitted
password form is reported as a high-confidence success immediately; the save
prompt does not wait for a post-submit success signal. Login candidates
stay in extension memory for at most 30 seconds. A username captured on the
first page of a two-step login can remain in memory for at most two minutes on
the same tab and exact origin. Neither value is written to extension storage.
`password.offerFill.userTemplate` is either `null` or a complete constrained
template object; legacy string template IDs are rejected instead of silently
falling back to generic field detection.

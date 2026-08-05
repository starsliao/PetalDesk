# AMO reviewer notes (draft)

## Dependency

The extension is a companion for the PetalDesk Windows desktop application. Native Messaging functionality requires the public PetalDesk installer: https://github.com/starsliao/PetalDesk/releases/download/v0.6.3/PetalDesk_0.6.3_x64-setup.exe.

The extension ID is fixed as `petaldesk-capture@petaldesk.app`, and the Native Messaging host name is `com.petaldesk.capture`.

## Reproducible build

Requirements:

- Node.js 22 or later
- npm
- Windows PowerShell 5.1 or PowerShell 7

From `browser-extension`:

```powershell
npm test
npm run build
npm run package:firefox
```

The project has no runtime npm dependencies, bundler, minifier, generated JavaScript, or obfuscation. `scripts/build.mjs` copies the readable source and selected image assets into `dist/firefox`. Packaging pins `web-ext@10.5.0` and produces `dist/artifacts/PetalDesk_Firefox_AMO-upload_<version>.zip` plus `PetalDesk_Firefox_AMO-source_<version>.zip` containing the readable source, tests, scripts, and reviewer fixtures.

## Temporary-load test

1. Run `npm run build`.
2. Open `about:debugging#/runtime/this-firefox`.
3. Choose **Load Temporary Add-on** and select `dist/firefox/manifest.json`.
4. Install/start PetalDesk so its Firefox Native Messaging host is registered.
5. Confirm PetalDesk reports the Firefox extension connection and the existing long-screenshot feature remains available.

## Password-fill test

1. In PetalDesk, create a test entry for an HTTPS login page or use the loopback fixture below with the HTTP warning explicitly accepted.
2. Choose **Open and fill**. The extension creates a new Firefox tab bound to the request.
3. Observe that the page overlay contains the origin, username, **Fill**, and **Cancel**, but no password has been delivered yet.
4. Choose **Fill**. Only then is the one-time credential sent. The username/password fields change and the form is not submitted.
5. Choose **Cancel** in a new attempt and confirm no field changes.

## Login-detection consent test

1. Enable login detection in PetalDesk. The Native Messaging request only arms the consent flow.
2. Click the PetalDesk Firefox toolbar icon. Firefox displays the optional authentication-information consent prompt from this direct user action.
3. Decline: password capture remains disabled and long screenshots still work.
4. Repeat and allow: submit a test form. A likely success prompts to save/update; an unchanged account creates no prompt after PetalDesk's match decision.
5. Disable login detection in PetalDesk and confirm subsequent submissions create no candidate.

## Local no-real-account fixture

The fixture never transmits form data and logs no credentials.

```powershell
node .\amo\reviewer-fixture\server.mjs
```

Open `http://127.0.0.1:43127/login`. Because this is intentionally HTTP, PetalDesk must explicitly allow this exact origin for the test entry. Suggested dummy values are `reviewer@example.invalid` and `review-only-password`.

Submitting the fixture removes the login form and changes the URL in the same page, providing a deterministic likely-success signal without contacting an external service.

Additional deterministic pages are available on the same origin:

- `/two-step` keeps a username briefly in memory between the username and password steps.
- `/password-change` contains a username, current password, new password, and confirmation field.
- `/failed-login` keeps the password form visible so the extension shows a low-confidence confirmation instead of claiming success.
- `/ambiguous` has deliberately ambiguous fields so the extension fails safely rather than guessing.

## Security cases worth checking

- Change the tab to another origin between the offer and confirmation: the request is rejected.
- Navigate after confirming but before credentials arrive: the new document cannot consume the old offer.
- Try a cross-origin iframe: v1 only accepts the top-level frame.
- Inspect extension storage: no credential or candidate is stored.
- Search the package: there is no automatic form submission code.

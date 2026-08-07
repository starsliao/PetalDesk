# AMO reviewer notes (draft)

## Dependency

The extension is a companion for the PetalDesk Windows desktop application. Native Messaging functionality requires the public PetalDesk installer: https://github.com/starsliao/PetalDesk/releases/download/v0.7.4/PetalDesk_0.7.4_x64-setup.exe.

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
3. The user action in PetalDesk starts a password-free offer. The bound content frame is validated, then the one-time credential is sent without a second page confirmation.
4. Confirm the username/password fields change, a short fill notice appears, and the form is not submitted.

## Toolbar popup and badge test

1. With accounts saved for a site, open that site: the toolbar badge shows the saved-account count for the exact origin and refreshes on tab switch, navigation, and vault changes. Lock the vault manually in PetalDesk: the badge clears.
2. Click the toolbar button: the popup shows layered diagnostics (install-time data permission, Native Host stdio, desktop password channel, recent request outcome, extension version), the current site's account list, and an **Open PetalDesk password manager** button.
3. Choose an account in the popup: the current page is filled directly, a short result notice appears, and the form is not submitted.

## Login-detection test

1. The `authenticationInfo` data permission is granted once at installation together with `websiteActivity`; there is no runtime or toolbar consent prompt. Confirm password features work immediately after installation.
2. Submit a test form. Login detection is always active while the connected vault is available, so submission immediately prompts to save as a new account or update an existing one (with a choice of which account); an unchanged account creates no prompt after PetalDesk's match decision.
3. Lock the vault manually in PetalDesk and submit again: the prompt reports the locked state instead of offering a save.

## Local no-real-account fixture

The fixture never transmits form data and logs no credentials.

```powershell
node .\amo\reviewer-fixture\server.mjs
```

Open `http://127.0.0.1:43127/login`. Because this is intentionally HTTP, PetalDesk must explicitly allow this exact origin for the test entry. Suggested dummy values are `reviewer@example.invalid` and `review-only-password`.

Submitting the fixture removes the login form and changes the URL in the same page without contacting an external service. The save/update prompt is triggered by the user's submission rather than a later success heuristic.

Additional deterministic pages are available on the same origin:

- `/two-step` keeps a username briefly in memory between the username and password steps.
- `/password-change` contains a username, current password, new password, and confirmation field.
- `/failed-login` keeps the password form visible; submission still produces the same save/update decision prompt, and the reviewer can ignore it.
- `/ambiguous` has deliberately ambiguous fields so the extension fails safely rather than guessing.

## Security cases worth checking

- Change the tab to another origin between the offer and credential delivery: the request is rejected.
- Navigate after the content frame is bound but before credentials arrive: the new document cannot consume the old offer.
- Try an arbitrary cross-origin iframe: it is rejected. The only current audited delegation is the `dl.reg.163.com` login frame embedded by `mail.163.com`.
- Inspect extension storage: no credential or candidate is stored.
- Search the package: there is no automatic form submission code.

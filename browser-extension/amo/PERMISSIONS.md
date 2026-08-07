# Firefox permissions and data-consent justification

This document is a draft for the AMO permission fields and reviewer notes. It must stay consistent with the submitted manifest and released desktop application.

## WebExtension permissions

### `nativeMessaging`

Required to communicate with the PetalDesk Native Messaging Host installed on the same Windows account. Long-capture commands, one-time fill messages, and login-candidate decisions use this local channel. The extension does not connect to an Internet service.

### `tabs`

Required to create the exact login tab requested by PetalDesk and bind a fill session to its tab ID, document, top-level origin, and validated login frame. It is also used to cancel a request when that target changes. Tab activation and navigation events additionally refresh the per-site account-count badge.

### `<all_urls>` host access

Long screenshots and password forms can occur on user-selected sites, so the content scripts need access to ordinary HTTP and HTTPS pages. Password operations still enforce exact scheme, host, and port matching. Browser-restricted pages fail closed. HTTP password operations require a separate per-origin opt-in.

### `action`

The toolbar button opens a popup showing layered connection diagnostics (install-time data permission, Native Host stdio, desktop password channel, recent request outcome, extension version), the accounts saved for the current exact origin, and a button that opens the PetalDesk password manager. Choosing an account in the popup fills the current page directly and shows a short result notice. The badge on the button shows how many accounts PetalDesk has stored for the current site.

## Firefox data collection permissions

Firefox treats data transferred from the add-on to a native application as transmission outside the add-on even when everything remains on the same computer.

### Required: `websiteActivity`

The long-screenshot feature sends user-initiated scroll position, viewport, and page-settling information to the local PetalDesk host. This is required for the extension's existing long-capture purpose.

### Required: `authenticationInfo`

Password filling and login detection handle usernames and passwords. This category is declared as required in the manifest, so Firefox presents and grants it once at installation together with `websiteActivity`. The extension never requests it again at runtime and there is no toolbar consent step; declining means not installing (or later removing) the extension.

## Data minimization

- The toolbar badge shows only the number of saved accounts for the exact current origin; it is cleared when the vault is manually locked.
- Fill offers contain the account label/username but no password.
- A password is requested only for a user-triggered fill after the background validates `fillConfirm` from the bound content frame. This message contains frame identity but no password.
- Filled credentials are removed from message objects immediately after use.
- Login candidates remain only in extension memory for at most 30 seconds. A username-only first step may remain for at most two minutes so the same tab can complete a two-step login on the exact same origin.
- No password, candidate, or fill message is written to browser storage, disk, console, or the legacy screenshot file spool.
- The extension never submits a form, bypasses MFA, solves CAPTCHA, or fills an arbitrary cross-origin frame. Cross-origin delegation is deny-by-default; the only current audited mapping is the `dl.reg.163.com` login frame embedded by `mail.163.com`.

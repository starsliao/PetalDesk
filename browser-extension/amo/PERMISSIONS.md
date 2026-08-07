# Firefox permissions and data-consent justification

This document is a draft for the AMO permission fields and reviewer notes. It must stay consistent with the submitted manifest and released desktop application.

## WebExtension permissions

### `nativeMessaging`

Required to communicate with the PetalDesk Native Messaging Host installed on the same Windows account. Long-capture commands, one-time password/TOTP fill messages, popup copy decisions, and login-candidate decisions use this local channel. The extension does not connect to an Internet service.

### `tabs`

Required to create the exact login tab requested by PetalDesk and bind a fill session to its tab ID, document, top-level origin, and validated login frame. It is also used to cancel a request when that target changes. Tab activation and navigation events additionally refresh the per-site account-count badge.

### `<all_urls>` host access

Long screenshots and password forms can occur on user-selected sites, so the content scripts need access to ordinary HTTP and HTTPS pages. Password operations still enforce exact scheme, host, and port matching. Browser-restricted pages fail closed. HTTP password operations require a separate per-origin opt-in.

### `action`

The toolbar button opens a popup showing layered connection diagnostics (install-time data permission, Native Host stdio, desktop password channel, recent request outcome, extension version), the accounts saved for the current exact origin, and a button that opens the PetalDesk password manager. Each account card can fill the page or request local copying of its username, password, or linked MFA code; deletion requires a second confirmation. The badge shows how many accounts PetalDesk has stored for the current site.

## Firefox data collection permissions

Firefox treats data transferred from the add-on to a native application as transmission outside the add-on even when everything remains on the same computer.

### Required: `websiteActivity`

The long-screenshot feature sends user-initiated scroll position, viewport, and page-settling information to the local PetalDesk host. This is required for the extension's existing long-capture purpose.

### Required: `authenticationInfo`

Password filling, login detection, and linked TOTP filling handle authentication information. This category is declared as required in the manifest, so Firefox presents and grants it once at installation together with `websiteActivity`. The extension never requests it again at runtime and there is no toolbar consent step; declining means not installing (or later removing) the extension.

## Data minimization

- The toolbar badge shows only the number of saved accounts for the exact current origin; it is cleared when the vault is manually locked.
- Fill offers contain the account label/username but no password.
- A password is requested only for a user-triggered fill after the background validates `fillConfirm` from the bound content frame. This message contains frame identity but no password.
- Filled credentials are removed from message objects immediately after use.
- Badge account summaries contain only `hasMfa`; they do not expose an MFA entry ID, label, secret, or current code. Popup MFA copy is written by PetalDesk directly to the system clipboard and never returns the code to extension JavaScript.
- Automatic TOTP filling requires a five-minute desktop journey and a single-use 30-second challenge bound to the connection, flow, tab, top-level HTTPS origin, frame, and document. The short-lived code is removed immediately after filling.
- Login candidates remain only in extension memory for at most 30 seconds. A username-only first step may remain for at most two minutes so the same tab can complete a two-step login on the exact same origin.
- No password, TOTP, MFA identifier, candidate, or fill message is written to browser storage, disk, console, diagnostics, or the legacy screenshot file spool.
- The extension never submits a form or bypasses MFA. It fills only a locally generated TOTP for an explicitly linked account inside a trusted login journey. Recovery codes, SMS or email codes, CAPTCHA, passkeys, security keys, CVV, postal codes, coupons, hidden fields, and arbitrary cross-origin OTP frames are excluded. Password-frame delegation remains deny-by-default; the only current audited mapping is the `dl.reg.163.com` login frame embedded by `mail.163.com`.

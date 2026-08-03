# Firefox permissions and data-consent justification

This document is a draft for the AMO permission fields and reviewer notes. It must stay consistent with the submitted manifest and released desktop application.

## WebExtension permissions

### `nativeMessaging`

Required to communicate with the PetalDesk Native Messaging Host installed on the same Windows account. Long-capture commands, one-time fill messages, and login-candidate decisions use this local channel. The extension does not connect to an Internet service.

### `tabs`

Required to create the exact login tab requested by PetalDesk and bind a fill session to its tab ID, top-level frame, document, and exact origin. It is also used to cancel a request when that target changes.

### `<all_urls>` host access

Long screenshots and password forms can occur on user-selected sites, so the content scripts need access to ordinary HTTP and HTTPS pages. Password operations still enforce exact scheme, host, and port matching. Browser-restricted pages fail closed. HTTP password operations require a separate per-origin opt-in.

### `action`

The toolbar button is the explicit Firefox user gesture used to request optional authentication-information consent. A Native Messaging command can arm the request but cannot display the Firefox permission prompt itself.

## Firefox data collection permissions

Firefox treats data transferred from the add-on to a native application as transmission outside the add-on even when everything remains on the same computer.

### Required: `websiteActivity`

The long-screenshot feature sends user-initiated scroll position, viewport, and page-settling information to the local PetalDesk host. This is required for the extension's existing long-capture purpose.

### Optional: `authenticationInfo`

Password filling and login detection handle usernames and passwords. This category is optional in the manifest and is not granted at installation. PetalDesk first records the user's feature choice. Firefox then asks for permission only from the extension toolbar click handler. Without the grant, fill and capture commands fail with `AUTHENTICATION_CONSENT_REQUIRED` and long screenshots remain available.

## Data minimization

- Fill offers contain the account label/username but no password.
- The password is requested only after the page overlay sends `fillConfirm` for the bound session.
- Filled credentials are removed from message objects immediately after use.
- Login candidates remain only in extension memory for at most 30 seconds. A username-only first step may remain for at most two minutes so the same tab can complete a two-step login on the exact same origin.
- No password, candidate, or fill message is written to browser storage, disk, console, or the legacy screenshot file spool.
- The extension never submits a form, bypasses MFA, solves CAPTCHA, or fills cross-origin frames.

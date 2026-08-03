# PetalDesk Browser Companion privacy notice (draft)

Last updated: 2026-08-04

This draft must be reviewed by the product owner and published at a stable HTTPS URL before the extension is submitted to AMO.

## Scope

PetalDesk Browser Companion is a Firefox extension that works with the PetalDesk desktop application installed on the same Windows computer. The extension has no PetalDesk cloud account and does not send information to PetalDesk, analytics providers, advertising networks, or other remote services.

## Information handled

The extension may handle the following information only to provide a feature requested by the user:

- Website activity used for long screenshots, including scroll position, viewport measurements, and page-settling state.
- The URL origin and tab/frame identifiers needed to bind a password request to the intended page.
- Authentication information, including usernames and passwords, when the user separately enables password features and grants Firefox's optional authentication-information permission.

The extension does not intentionally collect page text, browsing history, payment information, health information, personal communications, location, advertising identifiers, or analytics.

## How information is used

- Long-screenshot measurements are sent through Firefox Native Messaging to the local PetalDesk process to coordinate a screenshot requested by the user.
- A fill offer is shown on the intended page without a password. Only after the user confirms the page overlay does PetalDesk provide one-time credentials for field filling.
- When login detection is enabled, a submitted login candidate is held briefly in memory and sent to the local PetalDesk process so the encrypted vault can decide whether it is unchanged, new, or an update.

The extension fills fields but never submits a login form. It does not bypass CAPTCHA, passkeys, SMS verification, security keys, or multi-factor authentication.

## Storage and retention

The extension does not persist credentials or login candidates in Firefox storage or on disk. Candidate credentials expire after at most 30 seconds and are also cleared on pagehide, tab close, disconnect, or a user decision. For a two-step login, a username-only first step may remain in extension memory for at most two minutes, bound to the same tab and exact origin; it is cleared on timeout, tab close, disconnect, or a new origin. Fill credentials are cleared from extension message objects immediately after use.

Saved credentials are stored by the PetalDesk desktop application in an encrypted local vault under the user's chosen PetalDesk data location. Vault retention and deletion are controlled by the user in PetalDesk.

## Sharing and remote transfer

The extension does not sell, rent, share, or remotely transmit personal information. Native Messaging transfers data only to the registered PetalDesk executable running under the same Windows user account.

## Security controls

- Exact scheme, host, and port matching for password operations.
- Binding to a single session, tab, top-level frame, document, and origin.
- A visible confirmation before every fill and no automatic submission.
- HTTPS by default; HTTP requires a separate per-site warning and opt-in.
- Short in-memory expiration for pending requests and login candidates.

No security control can make an already compromised device or website safe. Users should keep Firefox, Windows, and PetalDesk updated.

## User choices

Users can decline or revoke the optional authentication-information permission in Firefox. They can disable login detection, delete saved accounts, or uninstall the extension. Long screenshots remain available when authentication-information access is not granted.

## Contact

Privacy questions: `<SUPPORT_EMAIL>` (to be confirmed by the publisher during submission)

Support page: https://github.com/starsliao/PetalDesk/issues

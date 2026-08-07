# PetalDesk Browser Companion privacy notice (draft)

Last updated: 2026-08-07

This draft must be reviewed by the product owner and published at a stable HTTPS URL before the extension is submitted to AMO.

## Scope

PetalDesk Browser Companion is a Firefox extension that works with the PetalDesk desktop application installed on the same Windows computer. The extension has no PetalDesk cloud account and does not send information to PetalDesk, analytics providers, advertising networks, or other remote services.

## Information handled

The extension may handle the following information only to provide a feature requested by the user:

- Website activity used for long screenshots, including scroll position, viewport measurements, and page-settling state.
- The URL origin and tab/frame identifiers needed to bind a password request to the intended page.
- Authentication information, including usernames and passwords, under the `authenticationInfo` data-collection permission that Firefox presents and grants once at installation. There is no additional runtime consent prompt.

The extension does not intentionally collect page text, browsing history, payment information, health information, personal communications, location, advertising identifiers, or analytics.

## How information is used

- Long-screenshot measurements are sent through Firefox Native Messaging to the local PetalDesk process to coordinate a screenshot requested by the user.
- After the user chooses an account in PetalDesk or the toolbar popup, a password-free offer is sent to the intended page. The bound content frame confirms its identity and field availability before PetalDesk provides one-time credentials for field filling; no second page confirmation is required.
- A login candidate submitted by the user is held briefly in memory and sent to the local PetalDesk process so the encrypted vault can decide whether it is unchanged, new, or an update. The prompt offers saving as a new account or updating an existing one, and reports when the vault is manually locked.
- The toolbar badge shows the number of accounts saved for the exact current origin, and the toolbar popup lists those account labels on click. Both are read from the local vault through the local channel, refresh on tab or vault changes, and are cleared when the vault is manually locked. After the extension connects, capture, fill, and badge keep working while the PetalDesk password window is closed; a manual lock stops them immediately.

The extension fills fields but never submits a login form. It does not bypass CAPTCHA, passkeys, SMS verification, security keys, or multi-factor authentication.

## Storage and retention

The extension does not persist credentials or login candidates in Firefox storage or on disk. Candidate credentials expire after at most 30 seconds and are also cleared on pagehide, tab close, disconnect, or a user decision. For a two-step login, a username-only first step may remain in extension memory for at most two minutes, bound to the same tab and exact origin; it is cleared on timeout, tab close, disconnect, or a new origin. Fill credentials are cleared from extension message objects immediately after use.

Saved credentials are stored by the PetalDesk desktop application in an encrypted local vault under the user's chosen PetalDesk data location. Vault retention and deletion are controlled by the user in PetalDesk.

## Sharing and remote transfer

The extension does not sell, rent, share, or remotely transmit personal information. Native Messaging transfers data only to the registered PetalDesk executable running under the same Windows user account.

## Security controls

- Exact scheme, host, and port matching for password operations.
- Binding to a single session, tab, document, top-level origin, and validated login frame. Cross-origin delegation is deny-by-default; the only current audited mapping is from `mail.163.com` to its `dl.reg.163.com` login frame.
- An explicit account choice before every fill and no automatic submission.
- HTTPS by default; HTTP requires a separate per-site warning and opt-in.
- Short in-memory expiration for pending requests and login candidates.

No security control can make an already compromised device or website safe. Users should keep Firefox, Windows, and PetalDesk updated.

## User choices

The `authenticationInfo` permission is granted at installation; users who do not want it can decline the installation or remove the extension from Firefox at any time. They can lock the vault or delete saved accounts in PetalDesk.

## Contact

Privacy questions: `<SUPPORT_EMAIL>` (to be confirmed by the publisher during submission)

Support page: https://github.com/starsliao/PetalDesk/issues

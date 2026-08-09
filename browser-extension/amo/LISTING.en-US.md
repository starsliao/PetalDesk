# AMO listed submission copy (en-US draft)

## Name

PetalDesk Browser Companion

## Summary

Connect Firefox to the local PetalDesk desktop app for long screenshots, secure password filling and saving, and linked local TOTP.

## Description

PetalDesk Browser Companion is the Firefox extension for the PetalDesk Windows desktop application.

Features:

- Capture long web pages through the existing PetalDesk screenshot workflow.
- Open a login page from the PetalDesk password manager and fill credentials after the user chooses the account in PetalDesk.
- Detect login forms submitted by the user and immediately offer to add or update the account in PetalDesk.
- Show the number of saved accounts for the current site on the toolbar badge. Clicking the toolbar button opens a popup with layered connection diagnostics and the current site's account list; choosing an account silently fills the current page without a success overlay or form submission.
- Link a password account to one local MFA entry. After credentials are filled, a trusted login journey can fill one clear 6-, 7-, or 8-digit TOTP field. Ambiguous fields require a user click, and an exact cross-origin HTTPS MFA page requires confirmation the first time.
- Copy the username, password, or linked MFA code from fixed account-card actions. PetalDesk writes an MFA copy directly to the system clipboard and clears it when the code expires. Account deletion starts from the card's context menu and still requires confirmation.
- Record constrained field templates for changed or internal sites. Recording stores selectors only and does not read field contents.
- Use generic login-form detection and built-in field templates for Google, Microsoft, Alibaba Cloud, Tencent Cloud, and Huawei Cloud.

Security properties:

- The extension communicates only with the PetalDesk Native Messaging Host installed on the same computer. It does not send data to PetalDesk or third-party servers.
- Authentication-information access is a required data-collection permission granted once at installation. The extension never asks for it again at runtime and there is no toolbar consent step.
- Password filling starts only after the user chooses an account in PetalDesk or its toolbar popup; linked TOTP remains confined to the resulting trusted login journey. The extension fills fields but never submits a form.
- TOTP linking starts only after PetalDesk fills the password, or after a manual login exactly matches one unique saved account. Recovery codes, SMS or email codes, CAPTCHA, passkeys, security keys, CVV, postal codes, and coupons are excluded.
- MFA secrets remain in the encrypted desktop vault. A current code used for automatic filling exists only briefly in the local in-memory channel; a toolbar MFA copy never passes through the extension.
- Login candidates stay briefly in extension memory and are cleared on timeout, disconnect, or page close. They are never written to browser storage.
- HTTP password use is blocked by default and requires an explicit per-origin opt-in in PetalDesk.

The password vault remains encrypted on the user's Windows device. The extension cannot read it independently.

## Suggested category and tags

- Primary category: Privacy & Security
- Alternative category: Other
- Tags: password manager, productivity, screenshot, local-first, PetalDesk

## 0.8.1 release notes

- Successful credential filling is now silent. Account deletion moves to a right-click context menu with confirmation, so cards no longer reserve height for a delete icon.
- Closing the password-manager window normally no longer disables Firefox login detection; save and update prompts continue unless the native channel disconnects, the vault is explicitly locked, or the vault is unavailable.
- Closing the MFA tool normally no longer marks its vault as explicitly locked. A deliberate user lock continues to block background code generation and copying.
- QR, image, and URI MFA imports allow editing the account name, issuer, account identifier, and icon while keeping algorithm, digits, and period read-only.
- The desktop password manager adds press-and-hold password reveal and matches the extension's username, password, and MFA copy icons, with MFA disabled for unlinked accounts.

## Required owner details before submission

- Publisher name: `<PUBLISHER_NAME>`
- Support email: `<SUPPORT_EMAIL>`
- Support page: https://github.com/starsliao/PetalDesk/issues
- Homepage: https://starsliao.github.io/PetalDesk/
- Public privacy-policy URL: https://starsliao.github.io/PetalDesk/privacy.html
- Public Windows installer URL: https://github.com/starsliao/PetalDesk/releases/download/v0.8.1/PetalDesk_0.8.1_x64-setup.exe
- License: MIT

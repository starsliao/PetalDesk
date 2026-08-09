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
- Detect login forms submitted by the user and immediately offer to add or update the account in PetalDesk. The pending prompt is restored after navigation in the same tab until the user explicitly handles it.
- Show the number of saved accounts for the current site on the toolbar badge. Clicking the toolbar button opens a popup with layered connection diagnostics and the current site's account list; choosing an account silently fills the current page without a success overlay or form submission.
- Open the PetalDesk main window, password manager, MFA authenticator, or a new standalone note directly from the toolbar popup.
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
- Before a save prompt is established, a password-bearing candidate is short-lived in extension memory. After it is handed to local PetalDesk, the extension clears the password and retains only non-secret prompt metadata; the desktop keeps the pending candidate only in process memory until the user acts, the tab closes, detection is disabled, the vault locks, or the local connection ends. Nothing is written to browser storage.
- HTTP password use is blocked by default and requires an explicit per-origin opt-in in PetalDesk.

The password vault remains encrypted on the user's Windows device. The extension cannot read it independently.

## Suggested category and tags

- Primary category: Privacy & Security
- Alternative category: Other
- Tags: password manager, productivity, screenshot, local-first, PetalDesk

## 0.8.2 release notes

- Login detection starts enabled and can be turned off independently in the toolbar popup. An unexpected disconnect starts three immediate fast reconnect attempts before a manual reconnect button appears; low-rate background recovery continues after that. Closing the password-manager window normally does not stop detection; turning detection off affects save/update prompts only, not fill, copy, MFA, or long screenshots.
- Pending save or update prompts survive same-tab navigation, refreshes, redirects, iframe replacement, and BFCache restoration until the user explicitly handles them or a real termination condition occurs.
- The toolbar popup adds four PetalDesk launchers for the main window, password manager, MFA authenticator, and a new standalone note.
- Once a save prompt is established, the extension immediately clears the candidate password and retains only password-free prompt metadata. No Firefox permission was added, and filling still never submits forms.

## Required owner details before submission

- Publisher name: `<PUBLISHER_NAME>`
- Support email: `<SUPPORT_EMAIL>`
- Support page: https://github.com/starsliao/PetalDesk/issues
- Homepage: https://starsliao.github.io/PetalDesk/
- Public privacy-policy URL: https://starsliao.github.io/PetalDesk/privacy.html
- Public Windows installer URL: https://github.com/starsliao/PetalDesk/releases/download/v0.8.2/PetalDesk_0.8.2_x64-setup.exe
- License: MIT

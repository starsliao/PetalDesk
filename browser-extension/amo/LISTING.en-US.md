# AMO listed submission copy (en-US draft)

## Name

PetalDesk Browser Companion

## Summary

Connect Firefox to the local PetalDesk desktop app for long screenshots and user-initiated password filling and saving.

## Description

PetalDesk Browser Companion is the Firefox extension for the PetalDesk Windows desktop application.

Features:

- Capture long web pages through the existing PetalDesk screenshot workflow.
- Open a login page from the PetalDesk password manager and fill credentials after the user chooses the account in PetalDesk.
- Detect login forms submitted by the user and immediately offer to add or update the account in PetalDesk.
- Show the number of saved accounts for the current site on the toolbar badge. Clicking the toolbar button opens a popup with layered connection diagnostics and the current site's account list; choosing an account fills the current page directly and shows a short result notice.
- Record constrained field templates for changed or internal sites. Recording stores selectors only and does not read field contents.
- Use generic login-form detection and built-in field templates for Google, Microsoft, Alibaba Cloud, Tencent Cloud, and Huawei Cloud.

Security properties:

- The extension communicates only with the PetalDesk Native Messaging Host installed on the same computer. It does not send data to PetalDesk or third-party servers.
- Authentication-information access is a required data-collection permission granted once at installation. The extension never asks for it again at runtime and there is no toolbar consent step.
- A fill starts only after the user chooses an account in PetalDesk or its toolbar popup. The extension fills fields but never submits a form.
- Login candidates stay briefly in extension memory and are cleared on timeout, disconnect, or page close. They are never written to browser storage.
- HTTP password use is blocked by default and requires an explicit per-origin opt-in in PetalDesk.

The password vault remains encrypted on the user's Windows device. The extension cannot read it independently.

## Suggested category and tags

- Primary category: Privacy & Security
- Alternative category: Other
- Tags: password manager, productivity, screenshot, local-first, PetalDesk

## First release notes

- Retains PetalDesk long-page capture.
- Adds user-initiated password filling and login detection.
- Makes authentication information a required install-time data permission with no runtime re-prompt.
- Adds a per-site account-count toolbar badge and a toolbar popup with connection diagnostics.
- Adds login templates for Google, Microsoft, Alibaba Cloud, Tencent Cloud, and Huawei Cloud.
- Adds user-recorded templates bound to one exact site origin.

## Required owner details before submission

- Publisher name: `<PUBLISHER_NAME>`
- Support email: `<SUPPORT_EMAIL>`
- Support page: https://github.com/starsliao/PetalDesk/issues
- Homepage: https://starsliao.github.io/PetalDesk/
- Public privacy-policy URL: https://starsliao.github.io/PetalDesk/privacy.html
- Public Windows installer URL: https://github.com/starsliao/PetalDesk/releases/download/v0.7.4/PetalDesk_0.7.4_x64-setup.exe
- License: MIT

# AMO listed submission copy (en-US draft)

## Name

PetalDesk Browser Companion

## Summary

Connect Firefox to the local PetalDesk desktop app for long screenshots and user-confirmed password filling and saving.

## Description

PetalDesk Browser Companion is the Firefox extension for the PetalDesk Windows desktop application.

Features:

- Capture long web pages through the existing PetalDesk screenshot workflow.
- Open a login page from the PetalDesk password manager and fill credentials only after an additional confirmation on that page.
- Detect login forms submitted by the user and offer to add or update the account in PetalDesk after likely login success.
- Record constrained field templates for changed or internal sites. Recording stores selectors only and does not read field contents.
- Use generic login-form detection and built-in field templates for Google, Microsoft, Alibaba Cloud, Tencent Cloud, and Huawei Cloud.

Security properties:

- The extension communicates only with the PetalDesk Native Messaging Host installed on the same computer. It does not send data to PetalDesk or third-party servers.
- Authentication-information access is optional. After enabling the feature in PetalDesk, the user must click the PetalDesk toolbar button and accept Firefox's permission prompt.
- Every fill displays an in-page confirmation. The extension fills fields but never submits a form.
- Login candidates stay briefly in extension memory and are cleared on timeout, disconnect, or page close. They are never written to browser storage.
- HTTP password use is blocked by default and requires an explicit per-origin opt-in in PetalDesk.

The password vault remains encrypted on the user's Windows device. The extension cannot read it independently.

## Suggested category and tags

- Primary category: Privacy & Security
- Alternative category: Other
- Tags: password manager, productivity, screenshot, local-first, PetalDesk

## First release notes

- Retains PetalDesk long-page capture.
- Adds user-confirmed password filling and login detection.
- Adds optional Firefox authentication-information consent.
- Adds login templates for Google, Microsoft, Alibaba Cloud, Tencent Cloud, and Huawei Cloud.
- Adds user-recorded templates bound to one exact site origin.

## Required owner details before submission

- Publisher name: `<PUBLISHER_NAME>`
- Support email: `<SUPPORT_EMAIL>`
- Support page: https://github.com/starsliao/PetalDesk/issues
- Homepage: https://starsliao.github.io/PetalDesk/
- Public privacy-policy URL: https://starsliao.github.io/PetalDesk/privacy.html
- Public Windows installer URL: https://github.com/starsliao/PetalDesk/releases/download/v0.6.0/PetalDesk_0.6.0_x64-setup.exe
- License: MIT

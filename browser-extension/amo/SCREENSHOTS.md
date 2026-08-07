# AMO screenshot capture guide

Capture PNG images at 1280 x 800 or larger from the release build. Keep Firefox at 100% zoom and use only the local reviewer fixture with dummy credentials. Do not composite or recreate Firefox permission prompts.

## Required captures

1. **Password manager and Firefox connection**: PetalDesk password manager with the browser status shown as connected. Use a dummy entry such as `reviewer@example.invalid`; no password may be visible.
2. **Direct fill result**: choose a dummy account in the toolbar popup, then capture the fixture page with its fields filled and the short PetalDesk result notice visible. The form must remain unsubmitted.
3. **Save or update prompt**: submit the fixture with dummy values and capture the resulting PetalDesk save prompt. A second capture may show the update wording after changing the dummy password.
4. **Template recording**: the fixture page with the recording overlay asking for the username or password field. Do not enter real credentials.
5. **Long screenshot compatibility**: the existing PetalDesk long-screenshot workflow running with the same extension build.
6. **Firefox data permission**: capture the install-time Firefox data-collection permission presentation listing required `websiteActivity` and `authenticationInfo` (from the installation flow or the extension's `about:addons` details). This image must be captured manually from Firefox and must not be mocked by HTML or image editing.
7. **Toolbar popup and badge**: capture the toolbar popup showing the layered diagnostics and fixed account cards, with the account-count badge visible on the toolbar button. Show the username, password, and MFA icon positions plus the separate delete icon; use only dummy entries and do not expose a password or code.
8. **Linked MFA editor**: capture one dummy password entry linked through the MFA search selector. The view may show only the MFA name, issuer, and account name; no secret or current TOTP may be visible.
9. **TOTP field handling**: on a reviewer-controlled HTTPS page, capture either a filled single TOTP field with the form still unsubmitted, or the one-click prompt for an ambiguous candidate. Do not include a live code in the screenshot; wait for it to expire or obscure the field through the real page controls before capture.
10. **Delete confirmation**: capture the confirmation popover anchored below the account's top-right delete icon. The card height and neighboring accounts must remain unchanged.

## Review before upload

- Crop only browser or application chrome that does not help explain the feature.
- Check the image for passwords, personal email addresses, profile names, bookmarks, browsing history, notifications, and filesystem paths.
- Keep the extension name, exact test origin, and relevant action labels readable.
- Do not show a successful automatic form submission; the extension fills fields only.
- Check the popup status area and clipboard history for copied TOTP values before capturing or uploading.

# AMO screenshot capture guide

Capture PNG images at 1280 x 800 or larger from the release build. Keep Firefox at 100% zoom and use only the local reviewer fixture with dummy credentials. Do not composite or recreate Firefox permission prompts.

## Required captures

1. **Password manager and Firefox connection**: PetalDesk password manager with the browser status shown as connected. Use a dummy entry such as `reviewer@example.invalid`; no password may be visible.
2. **Fill confirmation**: the fixture page with the PetalDesk overlay showing the exact origin, dummy username, Fill, and Cancel actions. Capture before confirming so no password has been delivered.
3. **Save or update prompt**: submit the fixture with dummy values and capture the resulting PetalDesk save prompt. A second capture may show the update wording after changing the dummy password.
4. **Template recording**: the fixture page with the recording overlay asking for the username or password field. Do not enter real credentials.
5. **Long screenshot compatibility**: the existing PetalDesk long-screenshot workflow running with the same extension build.
6. **Firefox data permission**: capture the install-time Firefox data-collection permission presentation listing required `websiteActivity` and `authenticationInfo` (from the installation flow or the extension's `about:addons` details). This image must be captured manually from Firefox and must not be mocked by HTML or image editing.
7. **Toolbar popup and badge**: capture the toolbar popup showing the layered diagnostics and the current site's account list, with the account-count badge visible on the toolbar button. Use only dummy entries; no password may be visible.

## Review before upload

- Crop only browser or application chrome that does not help explain the feature.
- Check the image for passwords, personal email addresses, profile names, bookmarks, browsing history, notifications, and filesystem paths.
- Keep the extension name, exact test origin, and relevant action labels readable.
- Do not show a successful automatic form submission; the extension fills fields only.

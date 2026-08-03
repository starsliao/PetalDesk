# AMO screenshot capture guide

Capture PNG images at 1280 x 800 or larger from the release build. Keep Firefox at 100% zoom and use only the local reviewer fixture with dummy credentials. Do not composite or recreate Firefox permission prompts.

## Required captures

1. **Password manager and Firefox connection**: PetalDesk password manager with the browser status shown as connected. Use a dummy entry such as `reviewer@example.invalid`; no password may be visible.
2. **Fill confirmation**: the fixture page with the PetalDesk overlay showing the exact origin, dummy username, Fill, and Cancel actions. Capture before confirming so no password has been delivered.
3. **Save or update prompt**: submit the fixture with dummy values and capture the resulting PetalDesk save prompt. A second capture may show the update wording after changing the dummy password.
4. **Template recording**: the fixture page with the recording overlay asking for the username or password field. Do not enter real credentials.
5. **Long screenshot compatibility**: the existing PetalDesk long-screenshot workflow running with the same extension build.
6. **Firefox data permission**: capture the real Firefox `authenticationInfo` permission prompt produced by clicking the toolbar action after PetalDesk arms consent. This image must be captured manually from Firefox and must not be mocked by HTML or image editing.

## Review before upload

- Crop only browser or application chrome that does not help explain the feature.
- Check the image for passwords, personal email addresses, profile names, bookmarks, browsing history, notifications, and filesystem paths.
- Keep the extension name, exact test origin, and relevant action labels readable.
- Do not show a successful automatic form submission; the extension fills fields only.

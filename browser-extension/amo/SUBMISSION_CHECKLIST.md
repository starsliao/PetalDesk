# AMO listed submission checklist

- [ ] Product owner confirms the publisher display name and support email; all other public URLs and the MIT license are already filled in.
- [ ] Product owner reviews `PRIVACY.md` and publishes the approved text at a stable HTTPS URL.
- [ ] Replace the remaining `<PUBLISHER_NAME>` and `<SUPPORT_EMAIL>` fields during submission.
- [ ] Confirm the extension and desktop app use one release version.
- [ ] Run `npm test`, `npm run build`, and `npm run package:firefox`.
- [ ] Confirm `web-ext lint` has no errors. The known Android-only warning is acceptable because v1 is Firefox Desktop only.
- [ ] Upload `dist/artifacts/PetalDesk_Firefox_AMO-upload_<version>.zip` as **On this site / Listed** in AMO Developer Hub.
- [ ] If AMO requests source, upload `dist/artifacts/PetalDesk_Firefox_AMO-source_<version>.zip` and paste the build steps from `REVIEWER_NOTES.md`.
- [ ] Complete the AMO data categories with required `websiteActivity` and optional `authenticationInfo` exactly as declared by the manifest.
- [ ] Add Chinese and English listing copy, icon, screenshots, privacy URL, and reviewer notes.
- [ ] Capture the real release screenshots described in `SCREENSHOTS.md`; do not mock Firefox permission UI.
- [ ] The product owner performs the final submission while signed into the verified AMO account with 2FA.
- [ ] After approval, install from the public AMO page and verify Native Messaging, password consent/fill/capture, long screenshots, and update behavior.

Optional API submission after the first listing exists:

```powershell
$env:AMO_JWT_ISSUER = '<stored outside the repository>'
$env:AMO_JWT_SECRET = '<stored outside the repository>'
.\scripts\package-firefox.ps1 -Sign -Channel Listed
```

Unlisted signing is only a fallback/development path:

```powershell
.\scripts\package-firefox.ps1 -Sign -Channel Unlisted
```

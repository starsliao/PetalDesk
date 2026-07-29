# Windows Native Messaging registration templates

The extension expects a Native Messaging host named `com.petaldesk.capture`.
The desktop/sidecar build for 飞花 - PetalDesk owns the host executable and
must implement the browser Native Messaging framing protocol (32-bit
little-endian message length followed by UTF-8 JSON).

The JSON files in `../manifests` show the required browser-specific manifest
shape. Chromium uses `allowed_origins`; Firefox uses `allowed_extensions` and
the stable Gecko ID `petaldesk-capture@petaldesk.app`.

`pnpm package:windows` builds `petaldesk-browser-host.exe` first and includes it
at the root of the installation directory for 飞花 - PetalDesk. The installer always writes and
registers the Firefox manifest. Release builds can register the store versions
of Chrome and Edge by setting either or both environment variables:

```powershell
$env:PETALDESK_CHROME_EXTENSION_ID = '<32-character-store-id>'
$env:PETALDESK_EDGE_EXTENSION_ID = '<32-character-store-id>'
pnpm package:windows
```

Missing Chromium IDs are intentionally non-fatal: the corresponding browser
registration is omitted. A supplied ID with an invalid format fails the build.
The uninstaller removes all three browser registration keys and the Native
Messaging manifest files owned by 飞花 - PetalDesk.

For a development or installer integration test, run:

```powershell
.\Register-PetalDeskNativeHost.ps1 `
  -HostExecutable 'C:\Program Files\PetalDesk\petaldesk-browser-host.exe' `
  -ChromeExtensionId '<32-character unpacked-or-store-id>' `
  -EdgeExtensionId '<32-character unpacked-or-store-id>'
```

Both Chromium ID parameters are optional. Omitting them registers Firefox only.

Use `-WhatIf` to inspect all writes. Registration is per-user under these keys:

- `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture`
- `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.petaldesk.capture`
- `HKCU\Software\Mozilla\NativeMessagingHosts\com.petaldesk.capture`

Remove the per-user registrations with:

```powershell
.\Unregister-PetalDeskNativeHost.ps1
```

Chrome and Edge extension IDs must be replaced with the actual unpacked or
store-assigned IDs. Firefox production use still requires an AMO-signed build;
these templates do not perform signing or store publication.

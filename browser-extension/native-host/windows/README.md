# Windows Native Messaging registration templates

The extension expects a Native Messaging host named `com.petaldesk.capture`.
The desktop/sidecar build for 飞花 - PetalDesk owns the host executable and
must implement the browser Native Messaging framing protocol (32-bit
little-endian message length followed by UTF-8 JSON).

The JSON files in `../manifests` show the required browser-specific manifest
shape. Chromium uses `allowed_origins`; Firefox uses `allowed_extensions` and
the stable Gecko ID `petaldesk-capture@petaldesk.app`.

`pnpm package:windows` builds `petaldesk-browser-host.exe` first and includes it
at the root of the installation directory for 飞花 - PetalDesk. The installer
registers Firefox plus Chrome and Edge. Both Chromium registrations use the
manifest-key-derived ID `eclmlkbklcpcaijloflmohfpeldcbjkp`.

Optional environment overrides are accepted only when they equal that fixed ID:

```powershell
$env:PETALDESK_CHROME_EXTENSION_ID = 'eclmlkbklcpcaijloflmohfpeldcbjkp'
$env:PETALDESK_EDGE_EXTENSION_ID = 'eclmlkbklcpcaijloflmohfpeldcbjkp'
pnpm package:windows
```

The build derives the ID from `manifest/chromium.json` and fails if its public
key, an override, or the expected ID disagree. The uninstaller removes all
three browser registration keys and the Native Messaging manifest files owned
by 飞花 - PetalDesk.

For a development or installer integration test, run:

```powershell
.\Register-PetalDeskNativeHost.ps1 `
  -HostExecutable 'C:\Program Files\PetalDesk\petaldesk-browser-host.exe' `
  -ChromeExtensionId 'eclmlkbklcpcaijloflmohfpeldcbjkp' `
  -EdgeExtensionId 'eclmlkbklcpcaijloflmohfpeldcbjkp'
```

Both Chromium ID parameters default to the fixed ID, so omitting them registers
the same unpacked extension for Chrome and Edge.

Use `-WhatIf` to inspect all writes. Registration is per-user under these keys:

- `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture`
- `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.petaldesk.capture`
- `HKCU\Software\Mozilla\NativeMessagingHosts\com.petaldesk.capture`

Remove the per-user registrations with:

```powershell
.\Unregister-PetalDeskNativeHost.ps1
```

Firefox production use still requires an AMO-signed build. The GitHub Chrome
ZIP is an unpacked developer-mode build; these templates do not perform signing
or store publication.

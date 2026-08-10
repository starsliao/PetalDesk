[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArchivePath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')]
    [string]$Version,

    [ValidatePattern('^[a-p]{32}$')]
    [string]$ExpectedExtensionId = 'eclmlkbklcpcaijloflmohfpeldcbjkp'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$resolvedArchive = (Resolve-Path -LiteralPath $ArchivePath).Path
$identityGuard = Join-Path $PSScriptRoot 'Test-ChromiumExtensionIdentity.ps1'
if (-not (Test-Path -LiteralPath $identityGuard -PathType Leaf)) {
    throw "Chromium identity guard is missing: $identityGuard"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($resolvedArchive)
try {
    $entries = @($archive.Entries)
    $entryGroups = @($entries | Group-Object -Property FullName)
    $duplicates = @($entryGroups | Where-Object { $_.Count -ne 1 })
    if ($duplicates.Count -ne 0) {
        throw "Chromium archive contains duplicate entries: $($duplicates.Name -join ', ')"
    }

    $requiredEntries = @(
        'manifest.json',
        'background/chromium-entry.js',
        'background/native-bridge.js',
        'background/password-bridge.js',
        'shared/browser-api.js',
        'shared/password-templates.js',
        'shared/scroll-core.js',
        'content/capture-session.js',
        'content/password-manager.js',
        'popup/popup.html',
        'popup/popup.js',
        'popup/popup.css',
        'assets/icons/petaldesk-16.png',
        'assets/icons/petaldesk-32.png',
        'assets/icons/petaldesk-48.png',
        'assets/icons/petaldesk-96.png',
        'assets/icons/petaldesk-128.png'
    )
    foreach ($entryName in $requiredEntries) {
        $entry = $archive.GetEntry($entryName)
        if ($null -eq $entry -or $entry.Length -le 0) {
            throw "Chromium archive is missing required file: $entryName"
        }
    }

    $privateKeys = @($entries | Where-Object { $_.FullName -match '(?i)\.pem$' })
    if ($privateKeys.Count -ne 0) {
        throw "Chromium archive must not contain private key files: $($privateKeys.FullName -join ', ')"
    }

    $manifestEntry = $archive.GetEntry('manifest.json')
    $stream = $manifestEntry.Open()
    try {
        $reader = [System.IO.StreamReader]::new(
            $stream,
            [System.Text.Encoding]::UTF8,
            $true
        )
        try {
            $manifestText = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }

    try {
        $manifest = $manifestText | ConvertFrom-Json
    }
    catch {
        throw "Chromium archive manifest is not valid JSON: $($_.Exception.Message)"
    }
    if ([string]$manifest.version -cne $Version) {
        throw "Chromium archive version '$($manifest.version)' does not match '$Version'."
    }
    if ([int]$manifest.manifest_version -ne 3) {
        throw 'Chromium archive must use Manifest V3.'
    }

    $backgroundProperty = $manifest.PSObject.Properties['background']
    $serviceWorker = if ($null -eq $backgroundProperty) {
        $null
    } else {
        $backgroundProperty.Value.PSObject.Properties['service_worker']
    }
    if ($null -eq $serviceWorker -or [string]$serviceWorker.Value -cne 'background/chromium-entry.js') {
        throw 'Chromium archive must use background/chromium-entry.js as its service worker.'
    }

    $actionProperty = $manifest.PSObject.Properties['action']
    $defaultPopup = if ($null -eq $actionProperty) {
        $null
    } else {
        $actionProperty.Value.PSObject.Properties['default_popup']
    }
    if ($null -eq $defaultPopup -or [string]$defaultPopup.Value -cne 'popup/popup.html') {
        throw 'Chromium archive must expose popup/popup.html from the toolbar action.'
    }

    $contentScriptsProperty = $manifest.PSObject.Properties['content_scripts']
    $hasPasswordManager = $false
    if ($null -ne $contentScriptsProperty) {
        foreach ($contentScript in @($contentScriptsProperty.Value)) {
            $scriptsProperty = $contentScript.PSObject.Properties['js']
            if ($null -ne $scriptsProperty -and
                @($scriptsProperty.Value) -ccontains 'content/password-manager.js') {
                $hasPasswordManager = $true
                break
            }
        }
    }
    if (-not $hasPasswordManager) {
        throw 'Chromium archive manifest does not load content/password-manager.js.'
    }

    & $identityGuard `
        -ManifestJson $manifestText `
        -ExpectedExtensionId $ExpectedExtensionId | Out-Null
}
finally {
    $archive.Dispose()
}

Write-Host "Chromium extension archive validated: $resolvedArchive"

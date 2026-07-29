[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$ManifestDirectory = (Join-Path $env:LOCALAPPDATA 'PetalDesk\NativeMessaging')
)

$ErrorActionPreference = 'Stop'
$registrationKeys = @(
    'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture'
    'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.petaldesk.capture'
    'HKCU:\Software\Mozilla\NativeMessagingHosts\com.petaldesk.capture'
)

foreach ($key in $registrationKeys) {
    if ((Test-Path -LiteralPath $key) -and $PSCmdlet.ShouldProcess($key, 'Remove Native Messaging host registration')) {
        Remove-Item -LiteralPath $key -Force
    }
}

$manifestNames = @(
    'com.petaldesk.capture.chrome.json'
    'com.petaldesk.capture.edge.json'
    'com.petaldesk.capture.chromium.json'
    'com.petaldesk.capture.firefox.json'
)

foreach ($manifestName in $manifestNames) {
    $manifestPath = Join-Path $ManifestDirectory $manifestName
    if ((Test-Path -LiteralPath $manifestPath) -and $PSCmdlet.ShouldProcess($manifestPath, 'Remove Native Messaging manifest')) {
        Remove-Item -LiteralPath $manifestPath -Force
    }
}

if ($WhatIfPreference) {
    Write-Output "Previewed removal of com.petaldesk.capture."
} else {
    Write-Output "Unregistered com.petaldesk.capture."
}

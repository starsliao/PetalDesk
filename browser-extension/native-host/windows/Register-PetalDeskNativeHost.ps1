[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory = $true)]
    [string]$HostExecutable,

    [string]$ChromeExtensionId = '',

    [string]$EdgeExtensionId = '',

    [string]$ManifestDirectory = (Join-Path $env:LOCALAPPDATA 'PetalDesk\NativeMessaging')
)

$ErrorActionPreference = 'Stop'
$hostPath = (Resolve-Path -LiteralPath $HostExecutable).Path
$manifestRoot = [System.IO.Path]::GetFullPath($ManifestDirectory)
$chromeManifestPath = Join-Path $manifestRoot 'com.petaldesk.capture.chrome.json'
$edgeManifestPath = Join-Path $manifestRoot 'com.petaldesk.capture.edge.json'
$firefoxManifestPath = Join-Path $manifestRoot 'com.petaldesk.capture.firefox.json'

foreach ($entry in @(
    @{ Name = 'Chrome'; Value = $ChromeExtensionId }
    @{ Name = 'Edge'; Value = $EdgeExtensionId }
)) {
    if (-not [string]::IsNullOrWhiteSpace($entry.Value) -and $entry.Value -notmatch '^[a-p]{32}$') {
        throw "$($entry.Name) extension ID must contain exactly 32 lowercase characters from a through p."
    }
}

$firefoxManifest = [ordered]@{
    name = 'com.petaldesk.capture'
    description = '飞花 - PetalDesk browser capture native messaging host'
    path = $hostPath
    type = 'stdio'
    allowed_extensions = @('petaldesk-capture@petaldesk.app')
}

if ($PSCmdlet.ShouldProcess($manifestRoot, 'Write Native Messaging manifests')) {
    New-Item -ItemType Directory -Path $manifestRoot -Force | Out-Null
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(
        $firefoxManifestPath,
        ($firefoxManifest | ConvertTo-Json -Depth 4),
        $utf8WithoutBom
    )

    if (-not [string]::IsNullOrWhiteSpace($ChromeExtensionId)) {
        $chromeManifest = [ordered]@{
            name = 'com.petaldesk.capture'
            description = '飞花 - PetalDesk browser capture native messaging host'
            path = $hostPath
            type = 'stdio'
            allowed_origins = @("chrome-extension://$ChromeExtensionId/")
        }
        [System.IO.File]::WriteAllText(
            $chromeManifestPath,
            ($chromeManifest | ConvertTo-Json -Depth 4),
            $utf8WithoutBom
        )
    }

    if (-not [string]::IsNullOrWhiteSpace($EdgeExtensionId)) {
        $edgeManifest = [ordered]@{
            name = 'com.petaldesk.capture'
            description = '飞花 - PetalDesk browser capture native messaging host'
            path = $hostPath
            type = 'stdio'
            allowed_origins = @("chrome-extension://$EdgeExtensionId/")
        }
        [System.IO.File]::WriteAllText(
            $edgeManifestPath,
            ($edgeManifest | ConvertTo-Json -Depth 4),
            $utf8WithoutBom
        )
    }
}

$registrations = @(
    @{ Key = 'HKCU:\Software\Mozilla\NativeMessagingHosts\com.petaldesk.capture'; Manifest = $firefoxManifestPath }
)
if (-not [string]::IsNullOrWhiteSpace($ChromeExtensionId)) {
    $registrations += @{
        Key = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture'
        Manifest = $chromeManifestPath
    }
}
if (-not [string]::IsNullOrWhiteSpace($EdgeExtensionId)) {
    $registrations += @{
        Key = 'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.petaldesk.capture'
        Manifest = $edgeManifestPath
    }
}

foreach ($registration in $registrations) {
    if ($PSCmdlet.ShouldProcess($registration.Key, 'Register Native Messaging host')) {
        New-Item -Path $registration.Key -Force | Out-Null
        Set-Item -Path $registration.Key -Value $registration.Manifest
    }
}

if ($WhatIfPreference) {
    Write-Output "Previewed registration for com.petaldesk.capture."
} else {
    $browsers = @('Firefox')
    if (-not [string]::IsNullOrWhiteSpace($ChromeExtensionId)) { $browsers += 'Chrome' }
    if (-not [string]::IsNullOrWhiteSpace($EdgeExtensionId)) { $browsers += 'Edge' }
    Write-Output "Registered com.petaldesk.capture for $($browsers -join ', ')."
}

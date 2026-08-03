[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,

    [string]$SignaturePath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[^/\s]+/[^/\s]+$')]
    [string]$Repository,

    [string]$Tag
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$manifestFile = Get-Item -LiteralPath $ManifestPath -ErrorAction Stop
$installer = Get-Item -LiteralPath $InstallerPath -ErrorAction Stop
if ($installer.Length -le 0) {
    throw "The Windows update payload is empty: $InstallerPath"
}
if ([string]::IsNullOrWhiteSpace($SignaturePath)) {
    $SignaturePath = "$($installer.FullName).sig"
}
$signatureFile = Get-Item -LiteralPath $SignaturePath -ErrorAction Stop

if ([string]::IsNullOrWhiteSpace($Tag)) {
    $Tag = "v$Version"
}
$expectedInstallerName = "PetalDesk_{0}_x64-setup.exe" -f $Version
if ($installer.Name -cne $expectedInstallerName) {
    throw "Installer name does not match version. Expected '$expectedInstallerName', got '$($installer.Name)'."
}
if ($signatureFile.Name -cne "$($installer.Name).sig") {
    throw "The signature must use the final installer name plus .sig."
}

try {
    $manifest = [System.IO.File]::ReadAllText($manifestFile.FullName) | ConvertFrom-Json
}
catch {
    throw "Updater manifest is not valid JSON: $($manifestFile.FullName). $($_.Exception.Message)"
}

$expectedTopLevelProperties = @("version", "notes", "pub_date", "platforms")
$actualTopLevelProperties = @($manifest.PSObject.Properties.Name)
$topLevelDifference = @(Compare-Object -ReferenceObject $expectedTopLevelProperties -DifferenceObject $actualTopLevelProperties)
if ($topLevelDifference.Count -ne 0) {
    throw "Updater manifest has unexpected top-level fields: $($actualTopLevelProperties -join ', ')"
}
if ($manifest.version -cne $Version) {
    throw "Updater manifest version mismatch. Expected '$Version', got '$($manifest.version)'."
}
if ($manifest.notes -isnot [string] -or [string]::IsNullOrWhiteSpace($manifest.notes)) {
    throw "Updater manifest notes must be a non-empty string."
}
try {
    [void][DateTimeOffset]::Parse($manifest.pub_date, [Globalization.CultureInfo]::InvariantCulture)
}
catch {
    throw "Updater manifest pub_date is not valid RFC 3339: $($manifest.pub_date)"
}

$platformNames = @($manifest.platforms.PSObject.Properties.Name)
if ($platformNames.Count -ne 1 -or $platformNames[0] -cne "windows-x86_64") {
    throw "The initial updater manifest can only publish windows-x86_64; found: $($platformNames -join ', ')"
}
$windows = $manifest.platforms."windows-x86_64"
$signature = [System.IO.File]::ReadAllText($signatureFile.FullName).Trim()
if ($windows.signature -cne $signature) {
    throw "Updater manifest signature does not match the final Windows installer .sig file."
}
try {
    [void][Convert]::FromBase64String($windows.signature)
}
catch {
    throw "The Windows signature in the updater manifest is not valid Base64."
}

$expectedUrl = "https://github.com/{0}/releases/download/{1}/{2}" -f @(
    $Repository,
    [Uri]::EscapeDataString($Tag),
    [Uri]::EscapeDataString($installer.Name)
)
if ($windows.url -cne $expectedUrl) {
    throw "Windows update URL mismatch. Expected '$expectedUrl', got '$($windows.url)'."
}

Write-Host "Windows updater manifest validated: $($manifestFile.FullName)"

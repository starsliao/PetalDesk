[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,

    [string]$SignaturePath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[^/\s]+/[^/\s]+$')]
    [string]$Repository,

    [string]$Tag,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [string]$Notes,

    [string]$NotesPath,

    [DateTimeOffset]$PublicationDate = [DateTimeOffset]::UtcNow
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not [string]::IsNullOrWhiteSpace($Notes) -and -not [string]::IsNullOrWhiteSpace($NotesPath)) {
    throw "Specify either Notes or NotesPath, not both."
}

$installer = Get-Item -LiteralPath $InstallerPath -ErrorAction Stop
if ($installer.PSIsContainer) {
    throw "The Windows update payload must be a file: $InstallerPath"
}
if ($installer.Length -le 0) {
    throw "The Windows update payload is empty: $InstallerPath"
}

$expectedInstallerName = "PetalDesk_{0}_x64-setup.exe" -f $Version
if ($installer.Name -cne $expectedInstallerName) {
    throw "Installer name does not match version. Expected '$expectedInstallerName', got '$($installer.Name)'."
}

if ([string]::IsNullOrWhiteSpace($SignaturePath)) {
    $SignaturePath = "$($installer.FullName).sig"
}
$signatureFile = Get-Item -LiteralPath $SignaturePath -ErrorAction Stop
if ($signatureFile.PSIsContainer) {
    throw "The Windows update signature must be a file: $SignaturePath"
}
if ($signatureFile.Name -cne "$($installer.Name).sig") {
    throw "The signature must use the final installer name plus .sig: $($installer.Name).sig"
}

$signature = [System.IO.File]::ReadAllText($signatureFile.FullName).Trim()
if ([string]::IsNullOrWhiteSpace($signature)) {
    throw "The Windows update signature is empty: $($signatureFile.FullName)"
}
try {
    [void][Convert]::FromBase64String($signature)
}
catch {
    throw "The Windows update signature is not valid Base64: $($signatureFile.FullName)"
}

if ([string]::IsNullOrWhiteSpace($Tag)) {
    $Tag = "v$Version"
}
if ($Tag -cne "v$Version") {
    throw "Update tag must exactly match version. Expected 'v$Version', got '$Tag'."
}

if (-not [string]::IsNullOrWhiteSpace($NotesPath)) {
    $notesFile = Get-Item -LiteralPath $NotesPath -ErrorAction Stop
    if ($notesFile.PSIsContainer) {
        throw "NotesPath must point to a file: $NotesPath"
    }
    $Notes = [System.IO.File]::ReadAllText($notesFile.FullName).Trim()
}
if ([string]::IsNullOrWhiteSpace($Notes)) {
    $Notes = "PetalDesk $Version update"
}

$downloadUrl = "https://github.com/{0}/releases/download/{1}/{2}" -f @(
    $Repository,
    [Uri]::EscapeDataString($Tag),
    [Uri]::EscapeDataString($installer.Name)
)
$manifest = [ordered]@{
    version = $Version
    notes = $Notes
    pub_date = $PublicationDate.ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = $signature
            url = $downloadUrl
        }
    }
}

$resolvedOutputPath = [System.IO.Path]::GetFullPath($OutputPath)
$outputDirectory = Split-Path -Parent $resolvedOutputPath
if ([string]::IsNullOrWhiteSpace($outputDirectory)) {
    throw "Cannot resolve updater manifest output directory: $OutputPath"
}
[void][System.IO.Directory]::CreateDirectory($outputDirectory)

$temporaryPath = Join-Path $outputDirectory (".{0}.{1}.tmp" -f ([System.IO.Path]::GetFileName($resolvedOutputPath)), [Guid]::NewGuid().ToString("N"))
try {
    $json = $manifest | ConvertTo-Json -Depth 5
    [System.IO.File]::WriteAllText(
        $temporaryPath,
        "$json`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $temporaryPath -Destination $resolvedOutputPath -Force
}
finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
}

Write-Host "Windows updater manifest generated: $resolvedOutputPath"

[CmdletBinding()]
param(
    [switch]$SkipTestAndBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$extensionRoot = Split-Path -Parent $PSScriptRoot
$chromiumRoot = Join-Path $extensionRoot 'dist\chromium'
$artifactsRoot = Join-Path $extensionRoot 'dist\artifacts'
$manifestPath = Join-Path $chromiumRoot 'manifest.json'
$identityGuard = Join-Path $PSScriptRoot 'Test-ChromiumExtensionIdentity.ps1'
$packageGuard = Join-Path $PSScriptRoot 'Test-ChromiumExtensionPackage.ps1'
$expectedExtensionId = 'eclmlkbklcpcaijloflmohfpeldcbjkp'

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $Command"
    }
}

Push-Location $extensionRoot
try {
    if (-not $SkipTestAndBuild) {
        Invoke-CheckedCommand -Command 'npm.cmd' -Arguments @('test')
        Invoke-CheckedCommand -Command 'npm.cmd' -Arguments @('run', 'build')
    }

    $manifestText = [System.IO.File]::ReadAllText(
        $manifestPath,
        [System.Text.Encoding]::UTF8
    )
    $manifest = $manifestText | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace([string]$manifest.version)) {
        throw "Chromium manifest does not define a version: $manifestPath"
    }
    $extensionVersion = [string]$manifest.version

    & $identityGuard `
        -ManifestPath $manifestPath `
        -ExpectedExtensionId $expectedExtensionId | Out-Null

    New-Item -ItemType Directory -Path $artifactsRoot -Force | Out-Null
    Get-ChildItem -LiteralPath $artifactsRoot `
        -Filter 'PetalDesk_Chrome_unpacked_*.zip' -File -ErrorAction SilentlyContinue |
        Remove-Item -Force
    $chromeArchive = Join-Path $artifactsRoot (
        'PetalDesk_Chrome_unpacked_{0}.zip' -f $extensionVersion
    )
    if (Test-Path -LiteralPath $chromeArchive) {
        Remove-Item -LiteralPath $chromeArchive -Force
    }
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::Open(
        $chromeArchive,
        [System.IO.Compression.ZipArchiveMode]::Create
    )
    try {
        $sourcePrefixLength = $chromiumRoot.TrimEnd('\').Length + 1
        $sourceFiles = @(Get-ChildItem -LiteralPath $chromiumRoot -Recurse -File |
            Sort-Object -Property FullName)
        foreach ($sourceFile in $sourceFiles) {
            $entryName = $sourceFile.FullName.Substring($sourcePrefixLength).Replace('\', '/')
            $entry = $archive.CreateEntry(
                $entryName,
                [System.IO.Compression.CompressionLevel]::Optimal
            )
            $sourceStream = [System.IO.File]::OpenRead($sourceFile.FullName)
            try {
                $entryStream = $entry.Open()
                try {
                    $sourceStream.CopyTo($entryStream)
                }
                finally {
                    $entryStream.Dispose()
                }
            }
            finally {
                $sourceStream.Dispose()
            }
        }
    }
    finally {
        $archive.Dispose()
    }

    & $packageGuard `
        -ArchivePath $chromeArchive `
        -Version $extensionVersion `
        -ExpectedExtensionId $expectedExtensionId
    Write-Output "Chrome unpacked extension archive: $chromeArchive"
}
finally {
    Pop-Location
}

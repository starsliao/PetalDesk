[CmdletBinding()]
param(
    [switch]$Sign
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$extensionRoot = Split-Path -Parent $PSScriptRoot
$firefoxRoot = Join-Path $extensionRoot 'dist\firefox'
$artifactsRoot = Join-Path $extensionRoot 'dist\artifacts'
$signedRoot = Join-Path $extensionRoot 'dist\signed'
$manifestPath = Join-Path $firefoxRoot 'manifest.json'
$webExtPackage = 'web-ext@10.5.0'

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
    Invoke-CheckedCommand -Command 'npm.cmd' -Arguments @('test')
    Invoke-CheckedCommand -Command 'npm.cmd' -Arguments @('run', 'build')
    Invoke-CheckedCommand -Command 'npx.cmd' -Arguments @(
        '--yes', $webExtPackage,
        'lint',
        '--source-dir', $firefoxRoot
    )

    # Windows PowerShell 5.1 treats UTF-8 files without a BOM as ANSI unless
    # the encoding is explicit. The extension name is localized, so read the
    # manifest through .NET to preserve valid JSON on every Windows locale.
    $manifestText = [System.IO.File]::ReadAllText(
        $manifestPath,
        [System.Text.Encoding]::UTF8
    )
    $manifest = $manifestText | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace($manifest.version)) {
        throw "Firefox manifest does not define a version: $manifestPath"
    }
    $extensionVersion = $manifest.version

    New-Item -ItemType Directory -Path $artifactsRoot -Force | Out-Null
    Invoke-CheckedCommand -Command 'npx.cmd' -Arguments @(
        '--yes', $webExtPackage,
        'build',
        '--source-dir', $firefoxRoot,
        '--artifacts-dir', $artifactsRoot,
        '--overwrite-dest'
    )

    $builtArchives = @(
        Get-ChildItem -LiteralPath $artifactsRoot -Filter '*.zip' -File
    )
    if ($builtArchives.Count -ne 1) {
        throw "Expected one Firefox archive, found $($builtArchives.Count)."
    }
    $amoArchive = Join-Path $artifactsRoot (
        "PetalDesk_Firefox_AMO-upload_{0}.zip" -f $extensionVersion
    )
    Move-Item -LiteralPath $builtArchives[0].FullName -Destination $amoArchive -Force
    Write-Output "Firefox AMO upload archive: $amoArchive"

    if ($Sign) {
        $amoIssuer = [Environment]::GetEnvironmentVariable('AMO_JWT_ISSUER', 'Process')
        $amoSecret = [Environment]::GetEnvironmentVariable('AMO_JWT_SECRET', 'Process')
        if ([string]::IsNullOrWhiteSpace($amoIssuer) -or
            [string]::IsNullOrWhiteSpace($amoSecret)) {
            throw 'AMO_JWT_ISSUER and AMO_JWT_SECRET are required with -Sign.'
        }

        if (Test-Path -LiteralPath $signedRoot) {
            Remove-Item -LiteralPath $signedRoot -Recurse -Force
        }
        New-Item -ItemType Directory -Path $signedRoot -Force | Out-Null
        Invoke-CheckedCommand -Command 'npx.cmd' -Arguments @(
            '--yes', $webExtPackage,
            'sign',
            '--source-dir', $firefoxRoot,
            '--artifacts-dir', $signedRoot,
            '--api-key', $amoIssuer,
            '--api-secret', $amoSecret,
            '--channel', 'unlisted',
            '--no-input'
        )

        $signedPackages = @(
            Get-ChildItem -LiteralPath $signedRoot -Filter '*.xpi' -File
        )
        if ($signedPackages.Count -ne 1) {
            throw "Expected one AMO-signed Firefox XPI, found $($signedPackages.Count)."
        }
        $signedXpi = Join-Path $signedRoot (
            "PetalDesk_Firefox_{0}-signed.xpi" -f $extensionVersion
        )
        Move-Item -LiteralPath $signedPackages[0].FullName -Destination $signedXpi -Force
        Write-Output "AMO-signed Firefox extension: $signedXpi"
    }
}
finally {
    Pop-Location
}

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$extensionRoot = Split-Path -Parent $PSScriptRoot

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
}
finally {
    Pop-Location
}

& (Join-Path $PSScriptRoot 'package-firefox.ps1') -SkipTestAndBuild
& (Join-Path $PSScriptRoot 'package-chromium.ps1') -SkipTestAndBuild

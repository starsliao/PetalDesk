[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$version = "9.8.7"
$repository = "starsliao/PetalDesk"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("petaldesk-updater-test-{0}" -f [Guid]::NewGuid().ToString("N"))
$installerPath = Join-Path $temporaryRoot "PetalDesk_${version}_x64-setup.exe"
$signaturePath = "$installerPath.sig"
$manifestPath = Join-Path $temporaryRoot "latest.json"

try {
    [void][System.IO.Directory]::CreateDirectory($temporaryRoot)
    [System.IO.File]::WriteAllBytes($installerPath, [byte[]](0..127))
    $testSignature = [Convert]::ToBase64String([byte[]](0..95))
    [System.IO.File]::WriteAllText($signaturePath, "$testSignature`n", [System.Text.UTF8Encoding]::new($false))

    & (Join-Path $PSScriptRoot "New-WindowsUpdaterManifest.ps1") `
        -Version $version `
        -InstallerPath $installerPath `
        -SignaturePath $signaturePath `
        -Repository $repository `
        -OutputPath $manifestPath `
        -PublicationDate ([DateTimeOffset]::Parse("2026-08-03T00:00:00Z"))

    & (Join-Path $PSScriptRoot "Test-WindowsUpdaterManifest.ps1") `
        -Version $version `
        -ManifestPath $manifestPath `
        -InstallerPath $installerPath `
        -SignaturePath $signaturePath `
        -Repository $repository

    $invalidManifest = [System.IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    $invalidManifest.platforms | Add-Member -NotePropertyName "darwin-aarch64" -NotePropertyValue ([pscustomobject]@{
        signature = $testSignature
        url = "https://example.invalid/unsigned-macos-update"
    })
    [System.IO.File]::WriteAllText(
        $manifestPath,
        (($invalidManifest | ConvertTo-Json -Depth 5) + "`n"),
        [System.Text.UTF8Encoding]::new($false)
    )

    $rejectedUnexpectedPlatform = $false
    try {
        & (Join-Path $PSScriptRoot "Test-WindowsUpdaterManifest.ps1") `
            -Version $version `
            -ManifestPath $manifestPath `
            -InstallerPath $installerPath `
            -SignaturePath $signaturePath `
            -Repository $repository
    }
    catch {
        $rejectedUnexpectedPlatform = $_.Exception.Message.Contains("只能发布 windows-x86_64")
    }
    if (-not $rejectedUnexpectedPlatform) {
        throw "清单校验器未拒绝意外加入的 macOS 更新平台。"
    }

    Write-Host "Windows 自动更新清单脚本测试通过。"
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

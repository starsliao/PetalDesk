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
    throw "Notes 与 NotesPath 只能指定其中一个。"
}

$installer = Get-Item -LiteralPath $InstallerPath -ErrorAction Stop
if ($installer.PSIsContainer) {
    throw "Windows 更新载荷必须是文件：$InstallerPath"
}
if ($installer.Length -le 0) {
    throw "Windows 更新载荷为空：$InstallerPath"
}

$expectedInstallerName = "PetalDesk_{0}_x64-setup.exe" -f $Version
if ($installer.Name -cne $expectedInstallerName) {
    throw "安装包文件名与版本不匹配：期望 '$expectedInstallerName'，实际 '$($installer.Name)'。"
}

if ([string]::IsNullOrWhiteSpace($SignaturePath)) {
    $SignaturePath = "$($installer.FullName).sig"
}
$signatureFile = Get-Item -LiteralPath $SignaturePath -ErrorAction Stop
if ($signatureFile.PSIsContainer) {
    throw "Windows 更新签名必须是文件：$SignaturePath"
}
if ($signatureFile.Name -cne "$($installer.Name).sig") {
    throw "签名文件必须与最终安装包同名并追加 .sig：$($installer.Name).sig"
}

$signature = [System.IO.File]::ReadAllText($signatureFile.FullName).Trim()
if ([string]::IsNullOrWhiteSpace($signature)) {
    throw "Windows 更新签名为空：$($signatureFile.FullName)"
}
try {
    [void][Convert]::FromBase64String($signature)
}
catch {
    throw "Windows 更新签名不是有效的 Base64 内容：$($signatureFile.FullName)"
}

if ([string]::IsNullOrWhiteSpace($Tag)) {
    $Tag = "v$Version"
}
if ($Tag -cne "v$Version") {
    throw "更新标签必须与版本严格匹配：期望 'v$Version'，实际 '$Tag'。"
}

if (-not [string]::IsNullOrWhiteSpace($NotesPath)) {
    $notesFile = Get-Item -LiteralPath $NotesPath -ErrorAction Stop
    if ($notesFile.PSIsContainer) {
        throw "NotesPath 必须指向文件：$NotesPath"
    }
    $Notes = [System.IO.File]::ReadAllText($notesFile.FullName).Trim()
}
if ([string]::IsNullOrWhiteSpace($Notes)) {
    $Notes = "飞花 - PetalDesk $Version 更新"
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
    throw "无法解析更新清单输出目录：$OutputPath"
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

Write-Host "Windows 自动更新清单已生成：$resolvedOutputPath"

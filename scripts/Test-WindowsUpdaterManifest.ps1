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
    throw "Windows 更新载荷为空：$InstallerPath"
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
    throw "安装包文件名与版本不匹配：期望 '$expectedInstallerName'，实际 '$($installer.Name)'。"
}
if ($signatureFile.Name -cne "$($installer.Name).sig") {
    throw "签名文件必须与最终安装包同名并追加 .sig。"
}

try {
    $manifest = [System.IO.File]::ReadAllText($manifestFile.FullName) | ConvertFrom-Json
}
catch {
    throw "更新清单不是有效 JSON：$($manifestFile.FullName)。$($_.Exception.Message)"
}

$expectedTopLevelProperties = @("version", "notes", "pub_date", "platforms")
$actualTopLevelProperties = @($manifest.PSObject.Properties.Name)
$topLevelDifference = @(Compare-Object -ReferenceObject $expectedTopLevelProperties -DifferenceObject $actualTopLevelProperties)
if ($topLevelDifference.Count -ne 0) {
    throw "更新清单顶层字段不符合预期：$($actualTopLevelProperties -join ', ')"
}
if ($manifest.version -cne $Version) {
    throw "更新清单版本不匹配：期望 '$Version'，实际 '$($manifest.version)'。"
}
if ($manifest.notes -isnot [string] -or [string]::IsNullOrWhiteSpace($manifest.notes)) {
    throw "更新清单 notes 必须是非空字符串。"
}
try {
    [void][DateTimeOffset]::Parse($manifest.pub_date, [Globalization.CultureInfo]::InvariantCulture)
}
catch {
    throw "更新清单 pub_date 不是有效的 RFC 3339 时间：$($manifest.pub_date)"
}

$platformNames = @($manifest.platforms.PSObject.Properties.Name)
if ($platformNames.Count -ne 1 -or $platformNames[0] -cne "windows-x86_64") {
    throw "第一阶段更新清单只能发布 windows-x86_64，实际为：$($platformNames -join ', ')"
}
$windows = $manifest.platforms."windows-x86_64"
$signature = [System.IO.File]::ReadAllText($signatureFile.FullName).Trim()
if ($windows.signature -cne $signature) {
    throw "更新清单签名与最终 Windows 安装包的 .sig 文件不一致。"
}
try {
    [void][Convert]::FromBase64String($windows.signature)
}
catch {
    throw "更新清单中的 Windows 签名不是有效的 Base64 内容。"
}

$expectedUrl = "https://github.com/{0}/releases/download/{1}/{2}" -f @(
    $Repository,
    [Uri]::EscapeDataString($Tag),
    [Uri]::EscapeDataString($installer.Name)
)
if ($windows.url -cne $expectedUrl) {
    throw "Windows 更新下载地址不符合预期：期望 '$expectedUrl'，实际 '$($windows.url)'。"
}

Write-Host "Windows 自动更新清单校验通过：$($manifestFile.FullName)"

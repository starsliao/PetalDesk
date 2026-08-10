[CmdletBinding(DefaultParameterSetName = 'Path')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Path')]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true, ParameterSetName = 'Json')]
    [string]$ManifestJson,

    [ValidatePattern('^[a-p]{32}$')]
    [string]$ExpectedExtensionId = 'eclmlkbklcpcaijloflmohfpeldcbjkp'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($PSCmdlet.ParameterSetName -eq 'Path') {
    $resolvedManifest = (Resolve-Path -LiteralPath $ManifestPath).Path
    $ManifestJson = [System.IO.File]::ReadAllText(
        $resolvedManifest,
        [System.Text.Encoding]::UTF8
    )
}

try {
    $manifest = $ManifestJson | ConvertFrom-Json
}
catch {
    throw "Chromium manifest is not valid JSON: $($_.Exception.Message)"
}

$keyProperty = $manifest.PSObject.Properties['key']
if ($null -eq $keyProperty -or [string]::IsNullOrWhiteSpace([string]$keyProperty.Value)) {
    throw 'Chromium manifest must define the public key used for its stable extension ID.'
}

try {
    $publicKey = [Convert]::FromBase64String(([string]$keyProperty.Value).Trim())
}
catch {
    throw "Chromium manifest key is not valid Base64: $($_.Exception.Message)"
}
if ($publicKey.Length -eq 0) {
    throw 'Chromium manifest key decoded to an empty public key.'
}

$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
    $digest = $sha256.ComputeHash($publicKey)
}
finally {
    $sha256.Dispose()
}

$alphabetBase = [int][char]'a'
$extensionIdCharacters = New-Object char[] 32
for ($index = 0; $index -lt 16; $index += 1) {
    $extensionIdCharacters[$index * 2] = [char]($alphabetBase + ($digest[$index] -shr 4))
    $extensionIdCharacters[$index * 2 + 1] = [char]($alphabetBase + ($digest[$index] -band 0x0f))
}
$extensionId = -join $extensionIdCharacters

if ($extensionId -cne $ExpectedExtensionId) {
    throw "Chromium manifest key derives extension ID '$extensionId', expected '$ExpectedExtensionId'."
}

Write-Output $extensionId

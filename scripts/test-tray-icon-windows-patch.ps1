param(
    [switch]$SkipCargoTest,
    [string]$BinaryPath
)

$ErrorActionPreference = "Stop"

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$expectedTrayVersion = "0.24.2"
$expectedTaoVersion = "0.35.3"
$rootManifest = Join-Path $repositoryRoot "src-tauri\Cargo.toml"
$crateManifest = Join-Path $repositoryRoot "src-tauri\vendor\tray-icon\Cargo.toml"
$taoManifest = Join-Path $repositoryRoot "src-tauri\vendor\tao\Cargo.toml"
$taoEventLoop = Join-Path $repositoryRoot "src-tauri\vendor\tao\src\platform_impl\windows\event_loop.rs"
$windowsBackendDirectory = Join-Path $repositoryRoot "src-tauri\vendor\tray-icon\src\platform_impl\windows"
$releaseWorkflow = Join-Path $repositoryRoot ".github\workflows\release.yml"

if (-not (Test-Path -LiteralPath $rootManifest -PathType Leaf)) {
    throw "PetalDesk Cargo manifest is missing: $rootManifest"
}
if (-not (Test-Path -LiteralPath $crateManifest -PathType Leaf)) {
    throw "Vendored tray-icon manifest is missing: $crateManifest"
}
if (-not (Test-Path -LiteralPath $taoManifest -PathType Leaf)) {
    throw "Vendored Tao manifest is missing: $taoManifest"
}
if (-not (Test-Path -LiteralPath $taoEventLoop -PathType Leaf)) {
    throw "Vendored Tao Windows event loop is missing: $taoEventLoop"
}
if (-not (Test-Path -LiteralPath $windowsBackendDirectory -PathType Container)) {
    throw "Vendored tray-icon Windows backend is missing: $windowsBackendDirectory"
}
if (-not (Test-Path -LiteralPath $releaseWorkflow -PathType Leaf)) {
    throw "Release workflow is missing: $releaseWorkflow"
}

$windowsSources = @(Get-ChildItem -LiteralPath $windowsBackendDirectory -Filter "*.rs" -File -Recurse)
if ($windowsSources.Count -eq 0) {
    throw "Vendored tray-icon Windows backend has no Rust sources: $windowsBackendDirectory"
}
$forbiddenSymbols = @(
    ("Shell_NotifyIcon" + "GetRect"),
    ("Peek" + "MessageW"),
    ("Dispatch" + "MessageW")
)
foreach ($sourceFile in $windowsSources) {
    $source = Get-Content -LiteralPath $sourceFile.FullName -Raw
    foreach ($symbol in $forbiddenSymbols) {
        if ($source.IndexOf($symbol, [System.StringComparison]::Ordinal) -ge 0) {
            throw "Forbidden blocking or nested-pump API '$symbol' is present in '$($sourceFile.FullName)'."
        }
    }
}

$taoSource = Get-Content -LiteralPath $taoEventLoop -Raw
$taskbarHandlerMatch = [regex]::Match(
    $taoSource,
    '(?s)} else if msg == \*S_U_TASKBAR_RESTART \{(?<handler>.*?)\r?\n      \}'
)
if (-not $taskbarHandlerMatch.Success) {
    throw "Tao TaskbarCreated handler was not found in '$taoEventLoop'."
}
$taskbarHandler = $taskbarHandlerMatch.Groups["handler"].Value
$normalizedTaskbarHandler = [regex]::Replace($taskbarHandler, '\s+', ' ').Trim()
$expectedTaskbarHandler = "let skip_taskbar = subclass_input.window_state.lock().skip_taskbar; let _ = set_skip_taskbar(window, skip_taskbar);"
if (-not $normalizedTaskbarHandler.Equals($expectedTaskbarHandler, [System.StringComparison]::Ordinal)) {
    throw "Tao TaskbarCreated handler no longer has the exact lock-free Explorer call sequence."
}

$releaseSource = Get-Content -LiteralPath $releaseWorkflow -Raw
foreach ($requiredText in @("test-tray-icon-windows-patch.ps1", "-BinaryPath")) {
    if ($releaseSource.IndexOf($requiredText, [System.StringComparison]::Ordinal) -lt 0) {
        throw "Release workflow no longer contains required tray guard text '$requiredText'."
    }
}

$metadataJson = & cargo metadata --manifest-path $rootManifest --locked --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw "Cargo metadata failed with exit code $LASTEXITCODE."
}
$metadata = $metadataJson | ConvertFrom-Json
$trayPackages = @($metadata.packages | Where-Object { $_.name -eq "tray-icon" })
if ($trayPackages.Count -ne 1) {
    throw "Expected exactly one resolved tray-icon package, found $($trayPackages.Count)."
}
if ($trayPackages[0].version -ne $expectedTrayVersion) {
    throw "Expected tray-icon $expectedTrayVersion, resolved $($trayPackages[0].version)."
}

$expectedManifest = [System.IO.Path]::GetFullPath($crateManifest)
$resolvedManifest = [System.IO.Path]::GetFullPath($trayPackages[0].manifest_path)
if (-not $resolvedManifest.Equals($expectedManifest, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Cargo resolved tray-icon from '$resolvedManifest' instead of the patched fork '$expectedManifest'."
}
if ($null -ne $trayPackages[0].source) {
    throw "The resolved tray-icon package unexpectedly has registry source '$($trayPackages[0].source)'."
}

$taoPackages = @($metadata.packages | Where-Object { $_.name -eq "tao" })
if ($taoPackages.Count -ne 1) {
    throw "Expected exactly one resolved Tao package, found $($taoPackages.Count)."
}
if ($taoPackages[0].version -ne $expectedTaoVersion) {
    throw "Expected Tao $expectedTaoVersion, resolved $($taoPackages[0].version)."
}
$expectedTaoManifest = [System.IO.Path]::GetFullPath($taoManifest)
$resolvedTaoManifest = [System.IO.Path]::GetFullPath($taoPackages[0].manifest_path)
if (-not $resolvedTaoManifest.Equals($expectedTaoManifest, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Cargo resolved Tao from '$resolvedTaoManifest' instead of the patched fork '$expectedTaoManifest'."
}
if ($null -ne $taoPackages[0].source) {
    throw "The resolved Tao package unexpectedly has registry source '$($taoPackages[0].source)'."
}

if (-not $SkipCargoTest) {
    & cargo test --manifest-path $crateManifest --lib
    if ($LASTEXITCODE -ne 0) {
        throw "Vendored tray-icon tests failed with exit code $LASTEXITCODE."
    }
    & cargo check --manifest-path $rootManifest --locked --all-targets
    if ($LASTEXITCODE -ne 0) {
        throw "PetalDesk/Tao integration check failed with exit code $LASTEXITCODE."
    }
}

if (-not [string]::IsNullOrWhiteSpace($BinaryPath)) {
    $resolvedBinary = if ([System.IO.Path]::IsPathRooted($BinaryPath)) {
        $BinaryPath
    } else {
        Join-Path $repositoryRoot $BinaryPath
    }
    if (-not (Test-Path -LiteralPath $resolvedBinary -PathType Leaf)) {
        throw "PetalDesk binary is missing: $resolvedBinary"
    }

    $binaryText = [System.Text.Encoding]::ASCII.GetString(
        [System.IO.File]::ReadAllBytes($resolvedBinary)
    )
    $blockedImport = "Shell_NotifyIcon" + "GetRect"
    if ($binaryText.IndexOf($blockedImport, [System.StringComparison]::Ordinal) -ge 0) {
        throw "The PetalDesk binary still imports the blocking tray rectangle API."
    }
}

Write-Host "Windows tray and taskbar deadlock patch checks passed."

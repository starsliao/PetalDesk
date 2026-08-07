param(
    [switch]$SkipAppBuild,
    [switch]$RequireUpdaterSignature
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $projectRoot "src-tauri"
$manifestPath = Join-Path $tauriRoot "Cargo.toml"
$releaseDir = Join-Path $tauriRoot "target\release"
$releaseExe = Join-Path $releaseDir "petaldesk.exe"
$nativeHostBinaryName = "petaldesk-browser-host.exe"
$nativeHostReleaseExe = Join-Path $releaseDir $nativeHostBinaryName
$nativeHostRegistrationScript = Join-Path $projectRoot "browser-extension\native-host\windows\Register-PetalDeskNativeHost.ps1"
$nsisDir = Join-Path $releaseDir "nsis\x64"
$installerScript = Join-Path $nsisDir "installer.nsi"
$nsisOutput = Join-Path $nsisDir "nsis-output.exe"
$makensis = Join-Path $env:LOCALAPPDATA "tauri\NSIS\makensis.exe"
$configPath = Join-Path $tauriRoot "tauri.conf.json"
$windowsConfigPath = Join-Path $tauriRoot "tauri.windows.conf.json"
$storagePathHooks = Join-Path $tauriRoot "nsis\storage-path.nsh"
$installerDisplayName = "飞花 - PetalDesk"
$desktopShortcutName = "飞花"
$updaterPrivateKey = [Environment]::GetEnvironmentVariable("TAURI_SIGNING_PRIVATE_KEY")
$updaterPrivateKeyPassword = [Environment]::GetEnvironmentVariable("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")

# Cargo embeds this value in get_app_info, which the About dialog renders as
# the package time. Set it once for the whole installer build so the several
# Tauri/Cargo invocations (app, Native Messaging host, and final EXE) all carry
# the same timestamp. SOURCE_DATE_EPOCH remains an explicit reproducible-build
# override; otherwise use the actual UTC start time of this packaging run.
$buildTimestamp = [Environment]::GetEnvironmentVariable("PETALDESK_BUILD_TIMESTAMP", "Process")
if ([string]::IsNullOrWhiteSpace($buildTimestamp)) {
    $buildTimestamp = [Environment]::GetEnvironmentVariable("SOURCE_DATE_EPOCH", "Process")
}
if ([string]::IsNullOrWhiteSpace($buildTimestamp)) {
    $buildTimestamp = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString()
}
if ($buildTimestamp -notmatch '^[1-9][0-9]*$') {
    throw "PETALDESK_BUILD_TIMESTAMP/SOURCE_DATE_EPOCH 必须是正数 Unix 秒级时间戳：$buildTimestamp"
}
$env:PETALDESK_BUILD_TIMESTAMP = $buildTimestamp
Write-Host "About 打包时间（UTC Unix timestamp）：$buildTimestamp"

if ([string]::IsNullOrWhiteSpace($updaterPrivateKey)) {
    if (-not [string]::IsNullOrEmpty($updaterPrivateKeyPassword)) {
        throw "已设置 TAURI_SIGNING_PRIVATE_KEY_PASSWORD，但缺少 TAURI_SIGNING_PRIVATE_KEY。"
    }
    if ($RequireUpdaterSignature) {
        throw "发布构建缺少 TAURI_SIGNING_PRIVATE_KEY，无法生成 Windows 自动更新签名。"
    }
}

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        & $Command @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "命令执行失败（退出码 $LASTEXITCODE）：$Command $($Arguments -join ' ')"
        }
    }
    finally {
        Pop-Location
    }
}

function Get-OptionalChromiumExtensionId {
    param(
        [Parameter(Mandatory = $true)][string]$EnvironmentVariable
    )

    $value = [Environment]::GetEnvironmentVariable($EnvironmentVariable, "Process")
    if ([string]::IsNullOrWhiteSpace($value)) {
        return $null
    }
    $value = $value.Trim()
    if ($value -notmatch '^[a-p]{32}$') {
        throw "$EnvironmentVariable 必须是 32 位小写 a-p 字符的 Chromium 扩展 ID。"
    }
    return $value
}

function Test-LegacyWorkspaceResolver {
    param(
        [Parameter(Mandatory = $true)][string]$MakensisPath,
        [Parameter(Mandatory = $true)][string]$HooksPath
    )

    # Compile and run a tiny silent NSIS probe around the real installer hook.
    # This installs no application files; it only verifies that Windows
    # PowerShell 5.1 can round-trip a legacy JSON path containing Chinese,
    # spaces, and JSON-escaped backslashes through the NSIS Unicode boundary.
    $probeRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
        "petaldesk-nsis-storage-probe-{0}" -f [Guid]::NewGuid().ToString("N")
    )
    $legacyProductName = @("Fei", "Hua") -join ""
    $legacyDisplayName = [string][char]0x98DE + [char]0x82B1
    $legacyLocalAppData = Join-Path $probeRoot "本地 配置"
    $legacySettingsDir = Join-Path $legacyLocalAppData $legacyProductName
    $legacyWorkspace = Join-Path $probeRoot "旧便签 数据\$legacyDisplayName"
    $legacyExtendedWorkspace = "\\?\$legacyWorkspace"
    $documents = [Environment]::GetFolderPath("MyDocuments")
    $legacyDefaultWorkspace = Join-Path $documents $legacyDisplayName
    $currentDefaultWorkspace = Join-Path $documents "PetalDesk"
    $newStorageParent = Join-Path $probeRoot "新存储 父目录"
    $newStorageRoot = Join-Path $newStorageParent "PetalDesk"
    $extendedStorageRoot = Join-Path $probeRoot "扩展路径 数据\PetalDesk"
    $probeScript = Join-Path $probeRoot "probe.nsi"
    $probeExe = Join-Path $probeRoot "probe.exe"
    $probeResult = Join-Path $probeRoot "resolved-path.txt"
    $newStorageResult = Join-Path $probeRoot "new-storage-path.txt"
    $extendedStorageResult = Join-Path $probeRoot "extended-storage-path.txt"
    $namespaceStorageResult = Join-Path $probeRoot "namespace-storage-paths.txt"
    $invalidStorageResult = Join-Path $probeRoot "invalid-storage-paths.txt"
    $settingsPath = Join-Path $legacySettingsDir "settings.json"
    $pointerPath = Join-Path $legacySettingsDir "storage-path.txt"
    $currentSettingsDir = Join-Path $probeRoot "current-config"
    $currentPointerPath = Join-Path $currentSettingsDir "storage-path.txt"
    $currentResolvedResult = Join-Path $probeRoot "current-resolved-path.txt"
    $originalLocalAppData = [Environment]::GetEnvironmentVariable("LOCALAPPDATA", "Process")

    try {
        [System.IO.Directory]::CreateDirectory($legacySettingsDir) | Out-Null
        [System.IO.Directory]::CreateDirectory($currentSettingsDir) | Out-Null
        [System.IO.Directory]::CreateDirectory($legacyWorkspace) | Out-Null
        [System.IO.Directory]::CreateDirectory($newStorageParent) | Out-Null

        # Old Rust builds persisted canonical Windows paths with the \\?\
        # extended-length prefix. The installer must normalize that prefix
        # before nsDialogs, GetFullPathName, CreateDirectory, and pointer I/O.
        $workspaceJson = ConvertTo-Json -InputObject $legacyExtendedWorkspace -Compress
        $settingsJson = '{"workspacePath":' + $workspaceJson + ',"editorMode":"typora"}'
        [System.IO.File]::WriteAllText(
            $settingsPath,
            $settingsJson,
            (New-Object System.Text.UTF8Encoding($false))
        )
        [System.IO.File]::WriteAllText(
            $pointerPath,
            $legacyExtendedWorkspace,
            [System.Text.Encoding]::Unicode
        )
        [System.IO.File]::WriteAllText(
            $currentPointerPath,
            $legacyExtendedWorkspace,
            [System.Text.Encoding]::Unicode
        )

        $escapedHooksPath = $HooksPath.Replace('$', '$$').Replace('"', '$\"')
        $probeText = @'
Unicode true
RequestExecutionLevel user
SilentInstall silent
SetCompress off
Name "飞花 - PetalDesk storage compatibility probe"
OutFile "probe.exe"
!include MUI2.nsh
!include FileFunc.nsh
!include LogicLib.nsh
!define PETALDESK_STORAGE_POINTER_DIR "$EXEDIR\current-config"
!include "__PETALDESK_STORAGE_HOOKS__"

Section
  StrCpy $PetalDeskStoragePath ""
  Call PetalDeskResolveLegacyWorkspacePath
  FileOpen $0 "$EXEDIR\resolved-path.txt" w
  FileWriteUTF16LE /BOM $0 "$PetalDeskStoragePath"
  FileClose $0

  StrCpy $PetalDeskStoragePath "$EXEDIR\新存储 父目录\PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileOpen $0 "$EXEDIR\new-storage-path.txt" w
  FileWriteUTF16LE /BOM $0 "$PetalDeskStoragePathError|$PetalDeskStoragePath"
  FileClose $0

  FileOpen $8 "$EXEDIR\namespace-storage-paths.txt" w
  StrCpy $PetalDeskStoragePath "\\?\UNC\server\share\PetalDesk"
  Call PetalDeskNormalizeStoragePath
  FileWriteUTF16LE /BOM $8 "$PetalDeskStoragePathError|$PetalDeskStoragePath|"
  StrCpy $PetalDeskStoragePath "\\?\unc\server\share\PetalDesk"
  Call PetalDeskNormalizeStoragePath
  FileWriteUTF16LE $8 "$PetalDeskStoragePathError|$PetalDeskStoragePath"
  FileClose $8

  StrCpy $PetalDeskStoragePath "\\?\$EXEDIR\扩展路径 数据\PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileOpen $0 "$EXEDIR\extended-storage-path.txt" w
  FileWriteUTF16LE /BOM $0 "$PetalDeskStoragePathError|$PetalDeskStoragePath"
  FileClose $0

  FileOpen $9 "$EXEDIR\invalid-storage-paths.txt" w
  StrCpy $PetalDeskStoragePath "C:PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileWriteUTF16LE /BOM $9 "$PetalDeskStoragePathError|"
  StrCpy $PetalDeskStoragePath "\PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileWriteUTF16LE $9 "$PetalDeskStoragePathError|"
  StrCpy $PetalDeskStoragePath "\\.\C:\PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileWriteUTF16LE $9 "$PetalDeskStoragePathError|"
  StrCpy $PetalDeskStoragePath "\\?\Volume{00000000-0000-0000-0000-000000000000}\PetalDesk"
  Call PetalDeskPrepareStoragePath
  FileWriteUTF16LE $9 "$PetalDeskStoragePathError"
  FileClose $9


  StrCpy $PetalDeskStoragePath ""
  Call PetalDeskResolveStoragePath
  FileOpen $7 "$EXEDIR\current-resolved-path.txt" w
  FileWriteUTF16LE /BOM $7 "$PetalDeskStoragePath"
  FileClose $7
  Call PetalDeskPersistStoragePath
SectionEnd
'@.Replace('__PETALDESK_STORAGE_HOOKS__', $escapedHooksPath)
        [System.IO.File]::WriteAllText(
            $probeScript,
            $probeText,
            (New-Object System.Text.UTF8Encoding($false))
        )

        Invoke-CheckedCommand -Command $MakensisPath -Arguments @(
            "/INPUTCHARSET", "UTF8", "/V2", "probe.nsi"
        ) -WorkingDirectory $probeRoot

        [Environment]::SetEnvironmentVariable(
            "LOCALAPPDATA",
            $legacyLocalAppData,
            "Process"
        )
        $probeProcess = Start-Process -FilePath $probeExe -ArgumentList @("/S") `
            -WorkingDirectory $probeRoot -WindowStyle Hidden -Wait -PassThru
        if ($probeProcess.ExitCode -ne 0) {
            throw "NSIS 旧数据存储路径检查程序失败（退出码 $($probeProcess.ExitCode)）。"
        }

        $resolved = [System.IO.File]::ReadAllText(
            $probeResult,
            [System.Text.Encoding]::Unicode
        )
        $expected = [System.IO.Path]::GetFullPath($legacyWorkspace)
        if (-not $resolved.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "NSIS 旧数据存储指针解析校验失败：期望 '$expected'，实际 '$resolved'"
        }

        $currentResolved = [System.IO.File]::ReadAllText(
            $currentResolvedResult,
            [System.Text.Encoding]::Unicode
        )
        if (-not $currentResolved.Equals(
                $expected,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS 当前数据存储指针显示规范化失败：期望 '$expected'，实际 '$currentResolved'"
        }
        $persistedPointerBytes = [System.IO.File]::ReadAllBytes($currentPointerPath)
        if ($persistedPointerBytes.Length -lt 2 -or
            $persistedPointerBytes[0] -ne 0xFF -or
            $persistedPointerBytes[1] -ne 0xFE) {
            throw "NSIS 当前数据存储指针持久化后缺少 UTF-16LE BOM。"
        }
        $persistedPointer = [System.Text.Encoding]::Unicode.GetString(
            $persistedPointerBytes,
            2,
            $persistedPointerBytes.Length - 2
        )
        if (-not $persistedPointer.Equals(
                $expected,
                [System.StringComparison]::OrdinalIgnoreCase
            ) -or $persistedPointer.StartsWith("\\?\")) {
            throw "NSIS 当前数据存储指针未写回普通路径：'$persistedPointer'"
        }

        # The pre-unified settings.json remains the final compatibility fallback.
        Remove-Item -LiteralPath $pointerPath -Force
        Remove-Item -LiteralPath $probeResult -Force
        $probeProcess = Start-Process -FilePath $probeExe -ArgumentList @("/S") `
            -WorkingDirectory $probeRoot -WindowStyle Hidden -Wait -PassThru
        if ($probeProcess.ExitCode -ne 0) {
            throw "NSIS 旧设置路径检查程序失败（退出码 $($probeProcess.ExitCode)）。"
        }
        $resolvedFromSettings = [System.IO.File]::ReadAllText(
            $probeResult,
            [System.Text.Encoding]::Unicode
        )
        if (-not $resolvedFromSettings.Equals(
                $expected,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS 旧设置路径解析校验失败：期望 '$expected'，实际 '$resolvedFromSettings'"
        }

        # The old Documents default is a brand-owned location, so a new
        # PetalDesk install must select Documents\PetalDesk instead. A custom
        # workspace was verified above and must remain unchanged.
        [System.IO.File]::WriteAllText(
            $pointerPath,
            $legacyDefaultWorkspace,
            [System.Text.Encoding]::Unicode
        )
        Remove-Item -LiteralPath $probeResult -Force
        $probeProcess = Start-Process -FilePath $probeExe -ArgumentList @("/S") `
            -WorkingDirectory $probeRoot -WindowStyle Hidden -Wait -PassThru
        if ($probeProcess.ExitCode -ne 0) {
            throw "NSIS 旧默认目录映射检查程序失败（退出码 $($probeProcess.ExitCode)）。"
        }
        $resolvedDefault = [System.IO.File]::ReadAllText(
            $probeResult,
            [System.Text.Encoding]::Unicode
        )
        $expectedDefault = [System.IO.Path]::GetFullPath($currentDefaultWorkspace)
        if (-not $resolvedDefault.Equals(
                $expectedDefault,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS 旧默认目录映射校验失败：期望 '$expectedDefault'，实际 '$resolvedDefault'"
        }
        Remove-Item -LiteralPath $pointerPath -Force

        $preparedStorage = [System.IO.File]::ReadAllText(
            $newStorageResult,
            [System.Text.Encoding]::Unicode
        )
        $expectedStorage = "|$([System.IO.Path]::GetFullPath($newStorageRoot))"
        if (-not $preparedStorage.Equals(
                $expectedStorage,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS 新数据存储目录创建校验失败：期望 '$expectedStorage'，实际 '$preparedStorage'"
        }
        if (-not [System.IO.Directory]::Exists($newStorageRoot)) {
            throw "NSIS 未创建用户选择的新数据存储目录：$newStorageRoot"
        }
        $writeTestFiles = [System.IO.Directory]::GetFiles(
            $newStorageRoot,
            ".petaldesk-write-test-*"
        )
        if ($writeTestFiles.Length -ne 0) {
            throw "NSIS 数据存储写入检查遗留了临时文件：$($writeTestFiles -join ', ')"
        }

        $preparedExtendedStorage = [System.IO.File]::ReadAllText(
            $extendedStorageResult,
            [System.Text.Encoding]::Unicode
        )
        $expectedExtendedStorage = "|$([System.IO.Path]::GetFullPath($extendedStorageRoot))"
        if (-not $preparedExtendedStorage.Equals(
                $expectedExtendedStorage,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS 扩展长度数据路径规范化失败：期望 '$expectedExtendedStorage'，实际 '$preparedExtendedStorage'"
        }

        $namespaceStorage = [System.IO.File]::ReadAllText(
            $namespaceStorageResult,
            [System.Text.Encoding]::Unicode
        )
        $expectedNamespaceStorage = "|\\server\share\PetalDesk||\\server\share\PetalDesk"
        if (-not $namespaceStorage.Equals(
                $expectedNamespaceStorage,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "NSIS UNC 扩展路径规范化失败：期望 '$expectedNamespaceStorage'，实际 '$namespaceStorage'"
        }

        $invalidStorage = [System.IO.File]::ReadAllText(
            $invalidStorageResult,
            [System.Text.Encoding]::Unicode
        )
        if ($invalidStorage -ne "invalid|invalid|invalid|invalid") {
            throw "NSIS 未拒绝非完整或设备命名空间路径：$invalidStorage"
        }

        # A malformed legacy file must not leak a partial path or fail the
        # installer. The caller will retain Documents\PetalDesk as its default.
        [System.IO.File]::WriteAllText(
            $settingsPath,
            '{"workspacePath":',
            (New-Object System.Text.UTF8Encoding($false))
        )
        if (Test-Path -LiteralPath $probeResult) {
            Remove-Item -LiteralPath $probeResult -Force
        }
        $probeProcess = Start-Process -FilePath $probeExe -ArgumentList @("/S") `
            -WorkingDirectory $probeRoot -WindowStyle Hidden -Wait -PassThru
        if ($probeProcess.ExitCode -ne 0) {
            throw "NSIS 旧数据存储回退检查程序失败（退出码 $($probeProcess.ExitCode)）。"
        }
        $fallbackResult = [System.IO.File]::ReadAllText(
            $probeResult,
            [System.Text.Encoding]::Unicode
        )
        if ($fallbackResult.Length -ne 0) {
            throw "NSIS 旧数据存储无效 JSON 未安全回退。"
        }

        Write-Host "NSIS 数据存储检查通过（当前指针解析/写回、自定义路径、UNC、旧默认目录映射、新目录创建、\\?\ 前缀、中文、空格、JSON 转义、失败回退）。"
    }
    finally {
        [Environment]::SetEnvironmentVariable(
            "LOCALAPPDATA",
            $originalLocalAppData,
            "Process"
        )
        $resolvedProbeRoot = [System.IO.Path]::GetFullPath($probeRoot)
        $resolvedTempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $resolvedTempPrefix = $resolvedTempRoot.TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        ) + [System.IO.Path]::DirectorySeparatorChar
        $probeLeaf = [System.IO.Path]::GetFileName($resolvedProbeRoot)
        $isExpectedProbeRoot = $resolvedProbeRoot.StartsWith(
            $resolvedTempPrefix,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -and $probeLeaf.StartsWith(
            "petaldesk-nsis-storage-probe-",
            [System.StringComparison]::Ordinal
        )
        if ($isExpectedProbeRoot -and (Test-Path -LiteralPath $resolvedProbeRoot)) {
            Remove-Item -LiteralPath $resolvedProbeRoot -Recurse -Force
        }
    }
}

if (-not (Test-Path -LiteralPath $storagePathHooks)) {
    throw "没有找到数据存储目录安装器脚本：$storagePathHooks"
}
if (-not (Test-Path -LiteralPath $nativeHostRegistrationScript)) {
    throw "没有找到 Native Messaging 注册脚本：$nativeHostRegistrationScript"
}

$storageHooksText = [System.IO.File]::ReadAllText(
    $storagePathHooks,
    [System.Text.Encoding]::UTF8
)
$postInstallHooks = [regex]::Matches(
    $storageHooksText,
    '(?s)!macro\s+NSIS_HOOK_POSTINSTALL\b(?<body>.*?)!macroend'
)
if ($postInstallHooks.Count -ne 1) {
    throw "数据存储安装器脚本必须且只能声明一个 NSIS_HOOK_POSTINSTALL。"
}
$persistCallCount = ([regex]::Matches(
    $postInstallHooks[0].Groups['body'].Value,
    '(?m)^\s*Call\s+PetalDeskPersistStoragePath\s*$'
)).Count
if ($persistCallCount -ne 1) {
    throw "NSIS_HOOK_POSTINSTALL 必须且只能调用一次 PetalDeskPersistStoragePath。"
}

$chromeExtensionId = Get-OptionalChromiumExtensionId -EnvironmentVariable "PETALDESK_CHROME_EXTENSION_ID"
$edgeExtensionId = Get-OptionalChromiumExtensionId -EnvironmentVariable "PETALDESK_EDGE_EXTENSION_ID"

$configText = [System.IO.File]::ReadAllText($configPath, [System.Text.Encoding]::UTF8)
$config = $configText | ConvertFrom-Json
$windowsConfigText = [System.IO.File]::ReadAllText($windowsConfigPath, [System.Text.Encoding]::UTF8)
$windowsConfig = $windowsConfigText | ConvertFrom-Json
$configuredHooks = $windowsConfig.bundle.windows.nsis.installerHooks
if ([string]::IsNullOrWhiteSpace($configuredHooks)) {
    throw "tauri.windows.conf.json 未配置 bundle.windows.nsis.installerHooks。"
}
$resolvedHooks = [System.IO.Path]::GetFullPath((Join-Path $tauriRoot $configuredHooks))
if (-not $resolvedHooks.Equals(
    [System.IO.Path]::GetFullPath($storagePathHooks),
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw "tauri.windows.conf.json 的 installerHooks 未指向：$storagePathHooks"
}

$expectedBundleResources = @{
    "../browser-extension/native-host/windows/Register-PetalDeskNativeHost.ps1" = "native-messaging/Register-PetalDeskNativeHost.ps1"
}
foreach ($resource in $expectedBundleResources.GetEnumerator()) {
    $configuredResource = $windowsConfig.bundle.resources.PSObject.Properties[$resource.Key]
    if ($null -eq $configuredResource -or $configuredResource.Value -ne $resource.Value) {
        throw "tauri.windows.conf.json 缺少 Native Messaging 资源映射：$($resource.Key) -> $($resource.Value)"
    }
}

if ($null -eq $chromeExtensionId) {
    Write-Host "未设置 PETALDESK_CHROME_EXTENSION_ID；安装包将跳过 Chrome Native Messaging 注册。"
}
if ($null -eq $edgeExtensionId) {
    Write-Host "未设置 PETALDESK_EDGE_EXTENSION_ID；安装包将跳过 Edge Native Messaging 注册。"
}

# A clean checkout does not have frontendDist yet. Build the Tauri app first so
# generate_context! can resolve ../build when Cargo compiles the Native Host.
if (-not $SkipAppBuild) {
    Invoke-CheckedCommand -Command "pnpm.cmd" -Arguments @(
        "tauri", "build", "--no-bundle", "--ci", "--no-sign"
    ) -WorkingDirectory $projectRoot
}

Invoke-CheckedCommand -Command "cargo.exe" -Arguments @(
    "build",
    "--manifest-path", $manifestPath,
    "--release",
    "--bin", "petaldesk-browser-host",
    "--features", "browser-native-host"
) -WorkingDirectory $projectRoot
if (-not (Test-Path -LiteralPath $nativeHostReleaseExe)) {
    throw "Native Messaging Host 构建后不存在：$nativeHostReleaseExe"
}

# Tauri 负责生成与当前版本匹配的 NSIS 脚本。它会暂时给 Release EXE
# 写入 bundle 类型信息，因此下面还会重新编译一次未修改的 custom-protocol EXE。
Invoke-CheckedCommand -Command "pnpm.cmd" -Arguments @(
    "tauri", "bundle", "--bundles", "nsis", "--ci", "--no-sign"
) -WorkingDirectory $projectRoot

if (-not (Test-Path -LiteralPath $installerScript)) {
    throw "没有找到 Tauri 生成的 NSIS 脚本：$installerScript"
}

# NSISdl::download 会显示下载进度，但插件默认进度文字是英文。
# 简体中文安装器使用 /TRANSLATE2 显示中文连接状态、百分比、速度和剩余时间；
# 其他语言继续使用插件默认文字。
$downloadLine = '        NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"'
$localizedDownload = @'
        ${If} $LANGUAGE == ${LANG_SIMPCHINESE}
          NSISdl::download /TRANSLATE2 "正在下载 %s" "正在连接微软下载服务器..." "（剩余 1 秒）" "（剩余 1 分钟）" "（剩余 1 小时）" "（剩余 %u 秒）" "（剩余 %u 分钟）" "（剩余 %u 小时）" "%skB / %skB（%d%%），%u.%01ukB/s" "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        ${Else}
          NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        ${EndIf}
'@

$installerText = [System.IO.File]::ReadAllText($installerScript)
if (-not $installerText.Contains("storage-path.nsh")) {
    throw "Tauri 生成的 NSIS 脚本没有包含数据存储目录 hooks。"
}
if (-not $installerText.Contains("Register-PetalDeskNativeHost.ps1")) {
    throw "Tauri 生成的 NSIS 脚本没有包含 Native Messaging 注册脚本。"
}

# The host cannot be a tauri.conf.json resource: Cargo executes Tauri's build
# script before this binary exists, so resource validation would make the host
# depend on its own output. Add it only after Tauri has generated installer.nsi.
# A running exe cannot be overwritten and Firefox may respawn the host at any
# moment, so both the main binary and the host are installed via rename-aside:
# renaming a locked executable succeeds and the running process keeps the old
# file while new launches get the new binary.
$nativeHostSourceForNsis = [System.IO.Path]::GetFullPath($nativeHostReleaseExe)
$nativeHostSourceForNsis = $nativeHostSourceForNsis.Replace('$', '$$').Replace('"', '$\"')
$nativeHostInstallInstruction = @(
    '  IfFileExists "$INSTDIR\petaldesk-browser-host.exe" 0 petaldesk_host_binary_absent',
    '  Delete "$INSTDIR\petaldesk-browser-host.locked-old"',
    '  Rename "$INSTDIR\petaldesk-browser-host.exe" "$INSTDIR\petaldesk-browser-host.locked-old"',
    '  petaldesk_host_binary_absent:',
    ('  File "/oname=$INSTDIR\{0}" "{1}"' -f $nativeHostBinaryName, $nativeHostSourceForNsis)
) -join [Environment]::NewLine
$nativeHostDeleteInstruction = '  Delete "$INSTDIR\{0}"' -f $nativeHostBinaryName
$nativeHostInstallPattern = '(?m)^[ \t]*File(?:[ \t]+/a)?[ \t]+"/oname=(?:\$INSTDIR\\)?{0}"[ \t]+"{1}"[ \t]*\r?$' -f `
    [regex]::Escape($nativeHostBinaryName), [regex]::Escape($nativeHostSourceForNsis)
$nativeHostDeletePattern = '(?m)^[ \t]*Delete[ \t]+"\$INSTDIR\\{0}"[ \t]*\r?$' -f `
    [regex]::Escape($nativeHostBinaryName)
$nativeHostInstallerEdits = @(
    @{
        Anchor = '  ; Copy external binaries'
        Instruction = $nativeHostInstallInstruction
        Pattern = $nativeHostInstallPattern
        Description = '安装'
    },
    @{
        Anchor = '  ; Delete external binaries'
        Instruction = $nativeHostDeleteInstruction
        Pattern = $nativeHostDeletePattern
        Description = '卸载'
    }
)
foreach ($edit in $nativeHostInstallerEdits) {
    $instructionCount = ([regex]::Matches($installerText, $edit.Pattern)).Count
    if ($instructionCount -gt 1) {
        throw "Native Messaging Host $($edit.Description)指令重复（实际 $instructionCount）。"
    }
    if ($instructionCount -eq 1) {
        continue
    }

    $anchorCount = ([regex]::Matches(
        $installerText,
        [regex]::Escape($edit.Anchor)
    )).Count
    if ($anchorCount -ne 1) {
        throw "无法唯一定位 Native Messaging Host $($edit.Description)位置（期望 1，实际 $anchorCount）。"
    }
    $installerText = $installerText.Replace(
        $edit.Anchor,
        "$($edit.Anchor)$([Environment]::NewLine)$($edit.Instruction)"
    )
}
foreach ($edit in $nativeHostInstallerEdits) {
    $instructionCount = ([regex]::Matches($installerText, $edit.Pattern)).Count
    if ($instructionCount -ne 1) {
        throw "Native Messaging Host $($edit.Description)指令注入失败（期望 1，实际 $instructionCount）。"
    }
}

# Skip Tauri NSIS template's maintenance page for upgrades: when an older
# version is detected, update in place (overwrite) instead of asking whether to
# uninstall first. This matches the behavior of Tauri's /UPDATE auto-update mode.
# Same-version reinstalls and downgrades keep the maintenance page.
$semverCompareAnchor = @(
    '  nsis_tauri_utils::SemverCompare "${VERSION}" $R0',
    '  Pop $R0'
) -join [Environment]::NewLine
$semverCompareAnchorCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($semverCompareAnchor)
)).Count
if ($semverCompareAnchorCount -ne 1) {
    throw "无法唯一定位 NSIS 版本比较位置（期望 1，实际 $semverCompareAnchorCount），Tauri 的 NSIS 模板可能已经变化。"
}
$installerText = $installerText.Replace(
    $semverCompareAnchor,
    @(
        $semverCompareAnchor,
        '  ; PetalDesk: upgrades update in place; the maintenance page only appears',
        '  ; for same-version reinstalls and downgrades.',
        '  ${If} $R0 = 1',
        '    Abort',
        '  ${EndIf}'
    ) -join [Environment]::NewLine
)

# The same rename-aside resilience for the main binary: if the user dismisses
# the running-app prompt, the upgrade still completes instead of failing the
# file write with an Abort/Retry/Ignore error.
$mainBinaryInstruction = '  File "${MAINBINARYSRCPATH}"'
$mainBinaryInstructionCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($mainBinaryInstruction)
)).Count
if ($mainBinaryInstructionCount -ne 1) {
    throw "无法唯一定位主程序写入指令（期望 1，实际 $mainBinaryInstructionCount），Tauri 的 NSIS 模板可能已经变化。"
}
$installerText = $installerText.Replace(
    $mainBinaryInstruction,
    @(
        '  IfFileExists "$INSTDIR\petaldesk.exe" 0 petaldesk_main_binary_absent',
        '  Delete "$INSTDIR\petaldesk.locked-old"',
        '  Rename "$INSTDIR\petaldesk.exe" "$INSTDIR\petaldesk.locked-old"',
        '  petaldesk_main_binary_absent:',
        $mainBinaryInstruction
    ) -join [Environment]::NewLine
)

# Keep Tauri's productName as the stable internal identity. Only replace NSIS
# presentation fields; install paths, registry keys, executable names, and the
# final installer filename remain PetalDesk-compatible. The desktop shortcut is
# handled separately below and is intentionally presented as “飞花”.
$productNameDefine = "!define PRODUCTNAME `"$($config.productName)`""
$displayNameDefine = "!define PETALDESK_DISPLAYNAME `"$installerDisplayName`""
$shortcutNameDefine = "!define PETALDESK_SHORTCUTNAME `"$desktopShortcutName`""
$productNameDefineCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($productNameDefine)
)).Count
if ($productNameDefineCount -ne 1) {
    throw "无法唯一定位 NSIS PRODUCTNAME 定义，Tauri 的 NSIS 模板可能已经变化。"
}
$installerText = $installerText.Replace(
    $productNameDefine,
    @(
        $productNameDefine,
        $displayNameDefine,
        $shortcutNameDefine
    ) -join [Environment]::NewLine
)

$displayNameReplacements = @(
    @{
        Find = 'Name "${PRODUCTNAME}"'
        Replace = 'Name "${PETALDESK_DISPLAYNAME}"'
        Count = 1
    },
    @{
        Find = 'VIAddVersionKey "ProductName" "${PRODUCTNAME}"'
        Replace = 'VIAddVersionKey "ProductName" "${PETALDESK_DISPLAYNAME}"'
        Count = 1
    },
    @{
        Find = 'VIAddVersionKey "FileDescription" "${PRODUCTNAME}"'
        Replace = 'VIAddVersionKey "FileDescription" "${PETALDESK_DISPLAYNAME}"'
        Count = 1
    },
    @{
        Find = '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"'
        Replace = '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PETALDESK_DISPLAYNAME}"'
        Count = 2
    },
    @{
        Find = 'WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "${PRODUCTNAME}"'
        Replace = 'WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "${PETALDESK_DISPLAYNAME}"'
        Count = 1
    },
    @{
        Find = 'WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${MANUFACTURER}"'
        Replace = 'WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${PETALDESK_DISPLAYNAME}"'
        Count = 1
    }
)

foreach ($replacement in $displayNameReplacements) {
    $matchCount = ([regex]::Matches(
        $installerText,
        [regex]::Escape($replacement.Find)
    )).Count
    if ($matchCount -ne $replacement.Count) {
        throw "NSIS 显示名称补丁定位失败：$($replacement.Find)（期望 $($replacement.Count)，实际 $matchCount）。"
    }
    $installerText = $installerText.Replace($replacement.Find, $replacement.Replace)
}

# Keep PetalDesk as the internal product identity while presenting the desktop
# shortcut with the concise Chinese name requested by the application UI.
$desktopShortcutReference = '$DESKTOP\${PRODUCTNAME}.lnk'
$desktopShortcutReplacement = '$DESKTOP\${PETALDESK_SHORTCUTNAME}.lnk'
$desktopShortcutReferenceCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($desktopShortcutReference)
)).Count
if ($desktopShortcutReferenceCount -ne 7) {
    throw "无法完整定位 NSIS 桌面快捷方式路径（期望 7，实际 $desktopShortcutReferenceCount）。"
}
$installerText = $installerText.Replace(
    $desktopShortcutReference,
    $desktopShortcutReplacement
)

# Upgrade an existing PetalDesk.lnk in place even when the installer is running
# in update mode. The target check avoids touching an unrelated shortcut that
# happens to have the same filename.
$desktopShortcutFunction = 'Function CreateOrUpdateDesktopShortcut'
$desktopShortcutFunctionCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($desktopShortcutFunction)
)).Count
if ($desktopShortcutFunctionCount -ne 1) {
    throw "无法唯一定位 NSIS 桌面快捷方式函数。"
}
$desktopShortcutMigration = @'
Function MigrateLegacyDesktopShortcutName
  !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 <> 1
  ${AndIf} $OldMainBinaryName != ""
    !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
    Pop $0
  ${EndIf}
  ${If} $0 = 1
    !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
    Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    CreateShortcut "$DESKTOP\${PETALDESK_SHORTCUTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$DESKTOP\${PETALDESK_SHORTCUTNAME}.lnk"
  ${EndIf}
FunctionEnd

Function CreateOrUpdateDesktopShortcut
'@
$installerText = $installerText.Replace(
    $desktopShortcutFunction,
    $desktopShortcutMigration.TrimEnd()
)

$startMenuShortcutAnchor = '  ; Create start menu shortcut'
$startMenuShortcutAnchorCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($startMenuShortcutAnchor)
)).Count
if ($startMenuShortcutAnchorCount -ne 1) {
    throw "无法唯一定位 NSIS 开始菜单快捷方式创建位置。"
}
$installerText = $installerText.Replace(
    $startMenuShortcutAnchor,
    "  Call MigrateLegacyDesktopShortcutName$([Environment]::NewLine)$([Environment]::NewLine)$startMenuShortcutAnchor"
)

$languageFiles = @(
    (Join-Path $nsisDir "SimpChinese.nsh"),
    (Join-Path $nsisDir "English.nsh")
)
foreach ($languageFile in $languageFiles) {
    if (-not (Test-Path -LiteralPath $languageFile)) {
        throw "没有找到 Tauri 生成的 NSIS 语言文件：$languageFile"
    }
    $languageText = [System.IO.File]::ReadAllText($languageFile)
    $languageReference = '${PRODUCTNAME}'
    $languageReferenceCount = ([regex]::Matches(
        $languageText,
        [regex]::Escape($languageReference)
    )).Count
    if ($languageReferenceCount -lt 1) {
        throw "NSIS 语言文件中没有产品显示名称引用：$languageFile"
    }
    $languageText = $languageText.Replace(
        $languageReference,
        '${PETALDESK_DISPLAYNAME}'
    )
    [System.IO.File]::WriteAllText(
        $languageFile,
        $languageText,
        [System.Text.UTF8Encoding]::new($false)
    )
}

$storageHooksText = [System.IO.File]::ReadAllText($storagePathHooks)
$nativeHostProcessGuard = '!insertmacro CheckIfAppIsRunning "petaldesk-browser-host.exe" "PetalDesk browser integration"'
$preInstallMacroMatches = [regex]::Matches(
    $storageHooksText,
    '(?s)!macro NSIS_HOOK_PREINSTALL(?<body>.*?)!macroend'
)
if ($preInstallMacroMatches.Count -ne 1) {
    throw "安装器 hooks 必须唯一定义 NSIS_HOOK_PREINSTALL。"
}
$preInstallMacroBody = $preInstallMacroMatches[0].Groups['body'].Value
if ($preInstallMacroBody.Contains('PetalDeskPersistStoragePath')) {
    throw "数据存储路径不能在 NSIS_HOOK_PREINSTALL 中持久化；此时飞花 - PetalDesk 运行检查尚未通过。"
}
$nativeHostPreInstallKill = 'taskkill.exe" /F /IM petaldesk-browser-host.exe'
$preInstallHostGuardCount = ([regex]::Matches(
    $preInstallMacroBody,
    [regex]::Escape($nativeHostPreInstallKill)
)).Count
if ($preInstallHostGuardCount -ne 1) {
    throw "NSIS_HOOK_PREINSTALL 必须唯一定义 Native Messaging Host 静默结束（taskkill）。"
}
if (-not $storageHooksText.Contains("!macro NSIS_HOOK_POSTINSTALL")) {
    throw "数据存储目录 hooks 未定义 NSIS_HOOK_POSTINSTALL。"
}
if (-not $storageHooksText.Contains("Function PetalDeskRegisterNativeMessagingHost")) {
    throw "安装器 hooks 未定义 Native Messaging 注册函数。"
}
if (-not $storageHooksText.Contains("!macro NSIS_HOOK_PREUNINSTALL")) {
    throw "安装器 hooks 未定义 Native Messaging 卸载清理。"
}
$nativeUninstallGuard = '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PETALDESK_DISPLAYNAME}"'
$nativeHostUninstallGuard = $nativeHostProcessGuard
$nativeUnregisterCall = 'Call un.PetalDeskUnregisterNativeMessagingHost'
$preUninstallMacroMatches = [regex]::Matches(
    $storageHooksText,
    '(?s)!macro NSIS_HOOK_PREUNINSTALL(?<body>.*?)!macroend'
)
if ($preUninstallMacroMatches.Count -ne 1) {
    throw "安装器 hooks 必须唯一定义 NSIS_HOOK_PREUNINSTALL。"
}
$preUninstallMacroBody = $preUninstallMacroMatches[0].Groups['body'].Value
$nativeUninstallGuardIndex = $preUninstallMacroBody.IndexOf(
    $nativeUninstallGuard,
    [System.StringComparison]::Ordinal
)
$nativeHostUninstallGuardIndex = $preUninstallMacroBody.IndexOf(
    $nativeHostUninstallGuard,
    [System.StringComparison]::Ordinal
)
$nativeUnregisterIndex = $preUninstallMacroBody.IndexOf(
    $nativeUnregisterCall,
    [System.StringComparison]::Ordinal
)
if ($nativeUninstallGuardIndex -lt 0) {
    throw "Native Messaging 卸载清理前缺少运行中应用检查。"
}
if ($nativeHostUninstallGuardIndex -le $nativeUninstallGuardIndex) {
    throw "Native Messaging 卸载清理前缺少运行中 Host 检查。"
}
if ($nativeUnregisterIndex -le $nativeHostUninstallGuardIndex) {
    throw "Native Messaging 卸载注册清理必须位于运行中应用检查之后。"
}

$runningAppCheck = '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PETALDESK_DISPLAYNAME}"'
$preInstallHook = '!insertmacro NSIS_HOOK_PREINSTALL'
$postInstallHook = '!insertmacro NSIS_HOOK_POSTINSTALL'
$mainBinaryFile = 'File "${MAINBINARYSRCPATH}"'
$preInstallHookIndex = $installerText.IndexOf(
    $preInstallHook,
    [System.StringComparison]::Ordinal
)
$runningAppCheckIndex = $installerText.IndexOf(
    $runningAppCheck,
    [System.StringComparison]::Ordinal
)
$postInstallHookIndex = $installerText.IndexOf(
    $postInstallHook,
    [System.StringComparison]::Ordinal
)
$mainBinaryFileIndex = $installerText.IndexOf(
    $mainBinaryFile,
    [System.StringComparison]::Ordinal
)
if ($preInstallHookIndex -lt 0) {
    throw "无法定位 NSIS_HOOK_PREINSTALL 调用，Tauri 的 NSIS 模板可能已经变化。"
}
if ($runningAppCheckIndex -lt 0) {
    throw "无法定位安装阶段的飞花 - PetalDesk 运行检查，Tauri 的 NSIS 模板可能已经变化。"
}
if ($postInstallHookIndex -lt 0) {
    throw "无法定位 NSIS_HOOK_POSTINSTALL 调用，Tauri 的 NSIS 模板可能已经变化。"
}
if ($mainBinaryFileIndex -lt 0) {
    throw "无法定位主程序复制指令，Tauri 的 NSIS 模板可能已经变化。"
}
if ($preInstallHookIndex -ge $mainBinaryFileIndex) {
    throw "Native Messaging Host 运行检查必须位于主程序和 Host 文件复制之前。"
}
if ($postInstallHookIndex -le $runningAppCheckIndex) {
    throw "NSIS_HOOK_POSTINSTALL 必须位于飞花 - PetalDesk 运行检查之后，避免失败安装提前切换数据存储。"
}

# Tauri 的 installerHooks 位于模板顶部，适合声明函数和安装阶段 hooks，
# 但不能把自定义页排到目录页之后，因此在生成脚本中精确插入页面声明。
$directoryPageLine = '!insertmacro MUI_PAGE_DIRECTORY'
$directoryPageCount = ([regex]::Matches(
    $installerText,
    [regex]::Escape($directoryPageLine)
)).Count
if ($directoryPageCount -ne 1) {
    throw "无法唯一定位安装目录页面，Tauri 的 NSIS 模板可能已经变化。"
}
$storagePageDeclaration = @'
!insertmacro MUI_PAGE_DIRECTORY

; Choose the PetalDesk business-data root after the application install directory.
Page custom PetalDeskStoragePageCreate PetalDeskStoragePageLeave
'@
$installerText = $installerText.Replace(
    $directoryPageLine,
    $storagePageDeclaration.TrimEnd()
)

if (-not $installerText.Contains($downloadLine)) {
    throw "无法定位 WebView2 官方下载语句，Tauri 的 NSIS 模板可能已经变化。"
}
$installerText = $installerText.Replace($downloadLine, $localizedDownload.TrimEnd())
[System.IO.File]::WriteAllText(
    $installerScript,
    $installerText,
    [System.Text.UTF8Encoding]::new($false)
)

$targetProcess = Get-Process -Name "petaldesk" -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $releaseExe }
if ($targetProcess) {
    throw "Release 版飞花 - PetalDesk 仍在运行，请退出后重新打包：$releaseExe"
}

$metadata = "petaldesk_unpatched_$([DateTime]::UtcNow.Ticks)"
Invoke-CheckedCommand -Command "cargo.exe" -Arguments @(
    "rustc",
    "--manifest-path", $manifestPath,
    "--release",
    "--bin", "petaldesk",
    "--features", "custom-protocol",
    "--",
    "-C", "metadata=$metadata"
) -WorkingDirectory $projectRoot

if (-not (Test-Path -LiteralPath $makensis)) {
    throw "没有找到 Tauri 下载的 makensis：$makensis"
}
Test-LegacyWorkspaceResolver -MakensisPath $makensis -HooksPath $storagePathHooks
$makensisArguments = @("/INPUTCHARSET", "UTF8", "/V2")
if ($null -ne $chromeExtensionId) {
    $makensisArguments += "/DPETALDESK_CHROME_EXTENSION_ID=$chromeExtensionId"
}
if ($null -ne $edgeExtensionId) {
    $makensisArguments += "/DPETALDESK_EDGE_EXTENSION_ID=$edgeExtensionId"
}
$makensisArguments += "installer.nsi"
Invoke-CheckedCommand -Command $makensis -Arguments $makensisArguments -WorkingDirectory $nsisDir

$installerName = "{0}_{1}_x64-setup.exe" -f $config.productName, $config.version
$bundleDir = Join-Path $releaseDir "bundle\nsis"
$finalInstaller = Join-Path $bundleDir $installerName
$finalInstallerSignature = "$finalInstaller.sig"
New-Item -ItemType Directory -Path $bundleDir -Force | Out-Null
Get-ChildItem -LiteralPath $bundleDir -Filter "PetalDesk_*_x64-setup.exe" -File |
    Where-Object { $_.Name.EndsWith(".exe", [System.StringComparison]::OrdinalIgnoreCase) } |
    Where-Object { $_.FullName -ne $finalInstaller } |
    Remove-Item -Force
Get-ChildItem -LiteralPath $bundleDir -Filter "PetalDesk_*_x64-setup.exe.sig" -File |
    Remove-Item -Force
Copy-Item -LiteralPath $nsisOutput -Destination $finalInstaller -Force

# This installer is rebuilt by makensis after Tauri's bundle step, so any
# signature produced before this point would describe the wrong bytes. Sign the
# final customized NSIS executable only after it has been copied into bundle/.
if ([string]::IsNullOrWhiteSpace($updaterPrivateKey)) {
    Write-Warning "未设置 TAURI_SIGNING_PRIVATE_KEY；已生成仅供本地安装测试的未签名安装包。"
}
else {
    # The Tauri CLI prompts on stdin when --password is omitted, even for an
    # unencrypted updater key. Always pass the value explicitly so CI and local
    # non-interactive release builds cannot hang at the signing step. Use the
    # `--password=value` form because Windows PowerShell rejects an empty string
    # inside a mandatory string-array parameter before invoking the command.
    $signerPassword = if ($null -eq $updaterPrivateKeyPassword) { "" } else { $updaterPrivateKeyPassword }
    Invoke-CheckedCommand -Command "pnpm.cmd" -Arguments @(
        "tauri", "signer", "sign", "--password=$signerPassword", $finalInstaller
    ) -WorkingDirectory $projectRoot

    if (-not (Test-Path -LiteralPath $finalInstallerSignature -PathType Leaf)) {
        throw "Tauri signer 未生成最终安装包签名：$finalInstallerSignature"
    }
    $signatureText = [System.IO.File]::ReadAllText($finalInstallerSignature).Trim()
    if ([string]::IsNullOrWhiteSpace($signatureText)) {
        throw "最终安装包签名为空：$finalInstallerSignature"
    }
    Write-Host "自动更新签名已生成：$finalInstallerSignature"
}

$file = Get-Item -LiteralPath $finalInstaller
$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
    $stream = [System.IO.File]::OpenRead($finalInstaller)
    try {
        $hash = ($sha256.ComputeHash($stream) | ForEach-Object { $_.ToString("X2") }) -join ""
    }
    finally {
        $stream.Dispose()
    }
}
finally {
    $sha256.Dispose()
}
Write-Host "联网安装包已生成：$($file.FullName)"
Write-Host "大小：$($file.Length) 字节"
Write-Host "SHA256：$hash"

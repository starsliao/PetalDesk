[CmdletBinding()]
param(
    [Parameter()]
    [string]$SourceRoot = (& {
            $documents = [Environment]::GetFolderPath("MyDocuments")
            $current = Join-Path $documents "PetalDesk"
            $legacyDisplay = [string][char]0x98DE + [char]0x82B1
            $legacy = Join-Path $documents $legacyDisplay
            if ((Test-Path -LiteralPath $legacy -PathType Container) -and
                -not (Test-Path -LiteralPath $current -PathType Container)) {
                $legacy
            }
            else {
                $current
            }
        }),

    [Parameter()]
    [string]$TargetRoot,

    [Parameter()]
    [string]$LocalAppDataRoot = (& {
            $local = [Environment]::GetFolderPath("LocalApplicationData")
            $current = Join-Path $local "PetalDesk"
            $legacyName = @("Fei", "Hua") -join ""
            $legacy = Join-Path $local $legacyName
            if ((Test-Path -LiteralPath $legacy -PathType Container) -and
                -not (Test-Path -LiteralPath $current -PathType Container)) {
                $legacy
            }
            else {
                $current
            }
        })
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$utf8NoBom = New-Object System.Text.UTF8Encoding -ArgumentList $false
$utf16LeBom = [System.Text.Encoding]::Unicode
$pathComparison = [System.StringComparison]::OrdinalIgnoreCase
$migrationStartedAt = [DateTime]::UtcNow
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"

$copyOperations = New-Object System.Collections.ArrayList
$copyResults = New-Object System.Collections.ArrayList
$conflicts = New-Object System.Collections.ArrayList
$warnings = New-Object System.Collections.ArrayList
$directoriesToCreate = New-Object System.Collections.ArrayList
$legacyLocalAppDataArchiveResults = New-Object System.Collections.ArrayList
$reportPath = $null
$sessionDirectory = $null
$notesSourceKind = "none"
$notesSourcePath = $null
$archivedLegacyNotesPath = $null
$archiveLegacyNotesAfterSuccess = $false
$archivedLegacyLocalAppDataPath = $null
$pointerPath = $null

function Get-NormalizedPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw "路径不能为空。"
    }

    $fullPath = [System.IO.Path]::GetFullPath(
        [Environment]::ExpandEnvironmentVariables($Path.Trim())
    )
    $root = [System.IO.Path]::GetPathRoot($fullPath)
    if (-not $fullPath.Equals($root, $pathComparison)) {
        $fullPath = $fullPath.TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        )
    }
    return $fullPath
}

function Get-ComparableStoragePath {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    $value = $Path.Trim([char[]]@([char]0xFEFF, [char]0, [char]13, [char]10, [char]32))
    if ($value.StartsWith("\\?\UNC\", [System.StringComparison]::OrdinalIgnoreCase)) {
        $value = "\\" + $value.Substring(8)
    }
    elseif ($value.StartsWith("\\?\", [System.StringComparison]::OrdinalIgnoreCase)) {
        $value = $value.Substring(4)
    }
    return Get-NormalizedPath -Path $value
}

function Test-PathsEqual {
    param(
        [Parameter(Mandatory = $true)][string]$First,
        [Parameter(Mandatory = $true)][string]$Second
    )

    return (Get-NormalizedPath -Path $First).Equals(
        (Get-NormalizedPath -Path $Second),
        $pathComparison
    )
}

function Test-IsPathInside {
    param(
        [Parameter(Mandatory = $true)][string]$Candidate,
        [Parameter(Mandatory = $true)][string]$Parent
    )

    $candidatePath = Get-NormalizedPath -Path $Candidate
    $parentPath = Get-NormalizedPath -Path $Parent
    if ($candidatePath.Equals($parentPath, $pathComparison)) {
        return $true
    }

    $parentWithSeparator = $parentPath + [System.IO.Path]::DirectorySeparatorChar
    return $candidatePath.StartsWith($parentWithSeparator, $pathComparison)
}

function Test-PathsOverlap {
    param(
        [Parameter(Mandatory = $true)][string]$First,
        [Parameter(Mandatory = $true)][string]$Second
    )

    return (Test-IsPathInside -Candidate $First -Parent $Second) -or
        (Test-IsPathInside -Candidate $Second -Parent $First)
}

function Assert-DirectoryIsSafe {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Description,
        [switch]$AllowMissing
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        if ($AllowMissing) {
            return
        }
        throw "$Description 不存在：$Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer) {
        throw "$Description 必须是目录：$Path"
    }
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Description 不能是 junction、符号链接或其他重解析点：$Path"
    }
}

function Assert-TargetStoragePathsSafe {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [switch]$AllowMissing
    )

    $internal = Join-Path $Root ".petaldesk"
    $paths = @($Root, $internal)
    foreach ($name in @("notes", "state", "tools", "backups", "journal", "trash", "conflicts")) {
        $paths += Join-Path $internal $name
    }
    foreach ($path in $paths) {
        Assert-DirectoryIsSafe -Path $path -Description "飞花 - PetalDesk 数据存储路径" `
            -AllowMissing:$AllowMissing
    }
}

function Assert-MigrationSessionSafe {
    param(
        [Parameter(Mandatory = $true)][string]$SessionPath,
        [Parameter(Mandatory = $true)][string]$BackupsPath
    )

    $session = Get-NormalizedPath -Path $SessionPath
    $backups = Get-NormalizedPath -Path $BackupsPath
    if ($session.Equals($backups, $pathComparison) -or
        -not (Test-IsPathInside -Candidate $session -Parent $backups)) {
        throw "迁移会话目录必须位于 .petaldesk/backups 内：$session"
    }
    Assert-DirectoryIsSafe -Path $backups -Description "飞花 - PetalDesk 迁移备份目录"
    Assert-DirectoryIsSafe -Path $session -Description "飞花 - PetalDesk 迁移会话目录"
}

function Get-Sha256ForFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Get-Sha256ForBytes {
    param(
        [Parameter(Mandatory = $true)][byte[]]$Bytes
    )

    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return (($sha256.ComputeHash($Bytes) | ForEach-Object {
                    $_.ToString("X2")
                }) -join "")
    }
    finally {
        $sha256.Dispose()
    }
}

function Add-TargetDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    $normalized = Get-NormalizedPath -Path $Path
    foreach ($existing in $directoriesToCreate) {
        if ($normalized.Equals([string]$existing, $pathComparison)) {
            return
        }
    }
    [void]$directoriesToCreate.Add($normalized)
}

function Add-Conflict {
    param(
        [Parameter(Mandatory = $true)][string]$Category,
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$Reason,
        [string]$SourceHash,
        [string]$DestinationHash
    )

    [void]$conflicts.Add([pscustomobject]@{
            category        = $Category
            source          = $Source
            destination     = $Destination
            reason          = $Reason
            sourceSha256    = $SourceHash
            destinationSha256 = $DestinationHash
        })
}

function Add-FileOperation {
    param(
        [Parameter(Mandatory = $true)][string]$Category,
        [string]$Source,
        [byte[]]$Content,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $destinationPath = Get-NormalizedPath -Path $Destination
    $sourcePath = $null
    $sourceHash = $null
    if ($null -ne $Content) {
        $sourceHash = Get-Sha256ForBytes -Bytes $Content
    }
    else {
        if ([string]::IsNullOrWhiteSpace($Source) -or -not (Test-Path -LiteralPath $Source -PathType Leaf)) {
            return
        }
        $sourcePath = Get-NormalizedPath -Path $Source
        $sourceItem = Get-Item -LiteralPath $sourcePath -Force
        if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
                -Reason "源文件是重解析点，为避免越界未迁移。"
            return
        }
        if ($sourcePath.Equals($destinationPath, $pathComparison)) {
            [void]$copyResults.Add([pscustomobject]@{
                    category        = $Category
                    description     = $Description
                    source          = $sourcePath
                    destination     = $destinationPath
                    action          = "samePath"
                    sourceSha256    = Get-Sha256ForFile -Path $sourcePath
                    destinationSha256 = Get-Sha256ForFile -Path $destinationPath
                    verified        = $true
                })
            return
        }
        $sourceHash = Get-Sha256ForFile -Path $sourcePath
    }

    if (Test-Path -LiteralPath $destinationPath) {
        $destinationItem = Get-Item -LiteralPath $destinationPath -Force
        if ($destinationItem.PSIsContainer) {
            Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
                -Reason "目标路径已存在且是目录。" -SourceHash $sourceHash
            return
        }
        if (($destinationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
                -Reason "目标文件是重解析点，拒绝覆盖。" -SourceHash $sourceHash
            return
        }

        $destinationHash = Get-Sha256ForFile -Path $destinationPath
        if ($sourceHash -eq $destinationHash) {
            [void]$copyResults.Add([pscustomobject]@{
                    category        = $Category
                    description     = $Description
                    source          = $sourcePath
                    destination     = $destinationPath
                    action          = "identical"
                    sourceSha256    = $sourceHash
                    destinationSha256 = $destinationHash
                    verified        = $true
                })
            return
        }

        Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
            -Reason "目标文件已存在且内容不同，未覆盖。" -SourceHash $sourceHash `
            -DestinationHash $destinationHash
        return
    }

    Add-TargetDirectory -Path ([System.IO.Path]::GetDirectoryName($destinationPath))
    [void]$copyOperations.Add([pscustomobject]@{
            category    = $Category
            description = $Description
            source      = $sourcePath
            content     = $Content
            destination = $destinationPath
            sourceSha256 = $sourceHash
        })
}

function Add-DirectoryOperations {
    param(
        [Parameter(Mandatory = $true)][string]$Category,
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
        return
    }

    $sourcePath = Get-NormalizedPath -Path $Source
    $destinationPath = Get-NormalizedPath -Path $Destination
    if ($sourcePath.Equals($destinationPath, $pathComparison)) {
        [void]$copyResults.Add([pscustomobject]@{
                category          = $Category
                description       = "目录已处于目标位置"
                source            = $sourcePath
                destination       = $destinationPath
                action            = "samePathDirectory"
                sourceSha256      = $null
                destinationSha256 = $null
                verified          = $false
            })
        return
    }

    if (Test-Path -LiteralPath $destinationPath) {
        $destinationItem = Get-Item -LiteralPath $destinationPath -Force
        if (-not $destinationItem.PSIsContainer) {
            Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
                -Reason "源条目是目录，但目标路径已存在为文件。"
            return
        }
        if (($destinationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
                -Reason "目标目录是重解析点，为避免越界未迁移。"
            return
        }
    }

    if (Test-IsPathInside -Candidate $destinationPath -Parent $sourcePath) {
        Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
            -Reason "迁移目标位于源目录内部，可能造成递归复制。"
        return
    }

    $sourceItem = Get-Item -LiteralPath $sourcePath -Force
    if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Add-Conflict -Category $Category -Source $sourcePath -Destination $destinationPath `
            -Reason "源目录是重解析点，为避免越界未迁移。"
        return
    }
    Add-TargetDirectory -Path $destinationPath

    $entries = @(Get-ChildItem -LiteralPath $sourcePath -Force | Sort-Object -Property Name)
    foreach ($entry in $entries) {
        $target = Join-Path $destinationPath $entry.Name
        if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category $Category -Source $entry.FullName -Destination $target `
                -Reason "源条目是重解析点，为避免越界未迁移。"
            continue
        }
        if ($entry.PSIsContainer) {
            if (Test-Path -LiteralPath $target -PathType Leaf) {
                Add-Conflict -Category $Category -Source $entry.FullName -Destination $target `
                    -Reason "源条目是目录，但目标路径已存在为文件。"
                continue
            }
            Add-DirectoryOperations -Category $Category -Source $entry.FullName -Destination $target
        }
        else {
            Add-FileOperation -Category $Category -Source $entry.FullName -Destination $target `
                -Description "迁移 $Category 文件"
        }
    }
}

function Add-LegacyNotesArchiveVerification {
    param(
        [Parameter(Mandatory = $true)][string]$LegacyDirectory,
        [Parameter(Mandatory = $true)][string]$NewNotesDirectory,
        [string]$CurrentDirectory
    )

    $legacyRoot = Get-NormalizedPath -Path $LegacyDirectory
    $newNotesRoot = Get-NormalizedPath -Path $NewNotesDirectory
    $newNotesItem = Get-Item -LiteralPath $newNotesRoot -Force
    if (($newNotesItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Add-Conflict -Category "notesLegacyArchive" -Source $legacyRoot `
            -Destination $newNotesRoot -Reason "新 .petaldesk/notes 是重解析目录，旧 notes 未归档。"
        return
    }
    if ([string]::IsNullOrWhiteSpace($CurrentDirectory)) {
        $currentPath = $legacyRoot
    }
    else {
        $currentPath = Get-NormalizedPath -Path $CurrentDirectory
    }

    $currentItem = Get-Item -LiteralPath $currentPath -Force
    if (($currentItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Add-Conflict -Category "notesLegacyArchive" -Source $currentPath `
            -Destination $newNotesRoot -Reason "旧 notes 包含重解析目录，未归档。"
        return
    }

    foreach ($entry in @(Get-ChildItem -LiteralPath $currentPath -Force | Sort-Object -Property Name)) {
        $relative = $entry.FullName.Substring($legacyRoot.Length).TrimStart(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        )
        $newPath = Join-Path $newNotesRoot $relative
        if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category "notesLegacyArchive" -Source $entry.FullName `
                -Destination $newPath -Reason "旧 notes 包含重解析条目，未归档。"
            continue
        }
        if ($entry.PSIsContainer) {
            Add-LegacyNotesArchiveVerification -LegacyDirectory $legacyRoot `
                -NewNotesDirectory $newNotesRoot -CurrentDirectory $entry.FullName
            continue
        }

        $sourceHash = Get-Sha256ForFile -Path $entry.FullName
        if (-not (Test-Path -LiteralPath $newPath -PathType Leaf)) {
            Add-Conflict -Category "notesLegacyArchive" -Source $entry.FullName `
                -Destination $newPath -Reason "新 .petaldesk/notes 中缺少对应文件，旧 notes 未归档。" `
                -SourceHash $sourceHash
            continue
        }
        $newItem = Get-Item -LiteralPath $newPath -Force
        if (($newItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Add-Conflict -Category "notesLegacyArchive" -Source $entry.FullName `
                -Destination $newPath -Reason "新 .petaldesk/notes 对应文件是重解析点，旧 notes 未归档。" `
                -SourceHash $sourceHash
            continue
        }

        $destinationHash = Get-Sha256ForFile -Path $newPath
        if ($sourceHash -ne $destinationHash) {
            Add-Conflict -Category "notesLegacyArchive" -Source $entry.FullName `
                -Destination $newPath -Reason "新旧 notes 对应文件内容不同，旧 notes 未归档。" `
                -SourceHash $sourceHash -DestinationHash $destinationHash
            continue
        }

        [void]$copyResults.Add([pscustomobject]@{
                category          = "notesLegacyArchive"
                description       = "归档前核验旧 notes"
                source            = $entry.FullName
                destination       = $newPath
                action            = "archiveVerified"
                sourceSha256      = $sourceHash
                destinationSha256 = $destinationHash
                verified          = $true
            })
    }
}

function Convert-LegacySettingsToConfigBytes {
    param(
        [Parameter(Mandatory = $true)][string]$SettingsPath
    )

    $settingsItem = Get-Item -LiteralPath $SettingsPath -Force
    if ($settingsItem.PSIsContainer -or
        ($settingsItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "旧 settings.json 不是可安全读取的普通文件：$SettingsPath"
    }
    try {
        $settingsText = [System.IO.File]::ReadAllText($SettingsPath, [System.Text.Encoding]::UTF8)
        $settings = $settingsText | ConvertFrom-Json
    }
    catch {
        throw "无法读取旧 settings.json：$SettingsPath。$($_.Exception.Message)"
    }

    $mode = "typora"
    $candidate = $null
    if ($settings.PSObject.Properties.Name -contains "defaultEditorMode") {
        $candidate = [string]$settings.defaultEditorMode
    }
    elseif ($settings.PSObject.Properties.Name -contains "editorMode") {
        $candidate = [string]$settings.editorMode
    }
    if ($candidate -eq "plain") {
        $mode = "plain"
    }

    $config = [ordered]@{
        schemaVersion      = 1
        defaultEditorMode = $mode
    }
    $json = ($config | ConvertTo-Json -Depth 4) + [Environment]::NewLine
    return $utf8NoBom.GetBytes($json)
}

function New-DefaultConfigBytes {
    $config = [ordered]@{
        schemaVersion      = 1
        defaultEditorMode = "typora"
    }
    $json = ($config | ConvertTo-Json -Depth 4) + [Environment]::NewLine
    return $utf8NoBom.GetBytes($json)
}

function Copy-OperationAtomically {
    param(
        [Parameter(Mandatory = $true)]$Operation
    )

    $destination = [string]$Operation.destination
    $parent = [System.IO.Path]::GetDirectoryName($destination)
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    Assert-DirectoryIsSafe -Path $parent -Description "迁移文件目标目录"
    if (Test-Path -LiteralPath $destination) {
        throw "执行迁移时目标文件突然出现，已中止且未覆盖：$destination"
    }

    $temporary = Join-Path $parent (".{0}.migration-{1}.tmp" -f `
            [System.IO.Path]::GetFileName($destination), [Guid]::NewGuid().ToString("N"))
    try {
        if ($null -ne $Operation.content) {
            [System.IO.File]::WriteAllBytes($temporary, [byte[]]$Operation.content)
        }
        else {
            Copy-Item -LiteralPath ([string]$Operation.source) -Destination $temporary
        }

        $temporaryHash = Get-Sha256ForFile -Path $temporary
        if ($temporaryHash -ne [string]$Operation.sourceSha256) {
            throw "复制后的临时文件 SHA256 校验失败：$destination"
        }
        [System.IO.File]::Move($temporary, $destination)
        $destinationHash = Get-Sha256ForFile -Path $destination
        if ($destinationHash -ne [string]$Operation.sourceSha256) {
            throw "目标文件 SHA256 校验失败：$destination"
        }

        [void]$copyResults.Add([pscustomobject]@{
                category          = [string]$Operation.category
                description       = [string]$Operation.description
                source            = [string]$Operation.source
                destination       = $destination
                action            = "copied"
                sourceSha256      = [string]$Operation.sourceSha256
                destinationSha256 = $destinationHash
                verified          = $true
            })
    }
    finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

function Test-StoragePointerMatches {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$StorageRoot
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $false
    }
    try {
        $item = Get-Item -LiteralPath $Path -Force
        if ($item.PSIsContainer -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            return $false
        }
        $bytes = [System.IO.File]::ReadAllBytes($Path)
        if ($bytes.Length -lt 2 -or $bytes[0] -ne 0xFF -or $bytes[1] -ne 0xFE) {
            return $false
        }
        $value = $utf16LeBom.GetString($bytes, 2, $bytes.Length - 2)
        $pointerPath = Get-ComparableStoragePath -Path $value
        $targetPath = Get-ComparableStoragePath -Path $StorageRoot
        return $pointerPath.Equals($targetPath, $pathComparison)
    }
    catch {
        return $false
    }
}

function Copy-FileToLegacyArchive {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $sourcePath = Get-NormalizedPath -Path $Source
    $destinationPath = Get-NormalizedPath -Path $Destination
    if (Test-Path -LiteralPath $destinationPath) {
        throw "旧 LocalAppData 归档目标已存在，未覆盖：$destinationPath"
    }
    $sourceItem = Get-Item -LiteralPath $sourcePath -Force
    if ($sourceItem.PSIsContainer -or
        ($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "旧 LocalAppData 条目不是可安全归档的普通文件：$sourcePath"
    }

    $sourceHash = Get-Sha256ForFile -Path $sourcePath
    $parent = [System.IO.Path]::GetDirectoryName($destinationPath)
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = Join-Path $parent (".{0}.archive-{1}.tmp" -f `
            [System.IO.Path]::GetFileName($destinationPath), [Guid]::NewGuid().ToString("N"))
    try {
        Copy-Item -LiteralPath $sourcePath -Destination $temporary
        $temporaryHash = Get-Sha256ForFile -Path $temporary
        if ($temporaryHash -ne $sourceHash) {
            throw "旧 LocalAppData 文件复制后 SHA256 校验失败：$sourcePath"
        }
        [System.IO.File]::Move($temporary, $destinationPath)
        $destinationHash = Get-Sha256ForFile -Path $destinationPath
        if ($destinationHash -ne $sourceHash) {
            throw "旧 LocalAppData 归档文件 SHA256 校验失败：$destinationPath"
        }

        $result = [pscustomobject]@{
            category          = "legacyLocalAppData"
            description       = $Description
            source            = $sourcePath
            destination       = $destinationPath
            action            = "archiveCopied"
            sourceSha256      = $sourceHash
            destinationSha256 = $destinationHash
            verified          = $true
        }
        [void]$legacyLocalAppDataArchiveResults.Add($result)
        return $result
    }
    finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

function Restore-ArchivedLocalFiles {
    param(
        [Parameter(Mandatory = $true)][object[]]$ArchiveResults
    )

    $restoredAll = $true
    foreach ($result in $ArchiveResults) {
        $source = [string]$result.source
        $archive = [string]$result.destination
        try {
            if (Test-Path -LiteralPath $source -PathType Leaf) {
                if ((Get-Sha256ForFile -Path $source) -eq [string]$result.sourceSha256) {
                    continue
                }
                [void]$warnings.Add("回滚时源位置已有不同内容，未覆盖：$source")
                $restoredAll = $false
                continue
            }

            $parent = [System.IO.Path]::GetDirectoryName($source)
            [System.IO.Directory]::CreateDirectory($parent) | Out-Null
            $temporary = Join-Path $parent (".{0}.restore-{1}.tmp" -f `
                    [System.IO.Path]::GetFileName($source), [Guid]::NewGuid().ToString("N"))
            try {
                Copy-Item -LiteralPath $archive -Destination $temporary
                if ((Get-Sha256ForFile -Path $temporary) -ne [string]$result.sourceSha256) {
                    throw "恢复临时文件 SHA256 校验失败"
                }
                [System.IO.File]::Move($temporary, $source)
                if ((Get-Sha256ForFile -Path $source) -ne [string]$result.sourceSha256) {
                    throw "恢复后的源文件 SHA256 校验失败"
                }
                $result.action = "rollbackRestored"
            }
            finally {
                if (Test-Path -LiteralPath $temporary) {
                    Remove-Item -LiteralPath $temporary -Force
                }
            }
        }
        catch {
            $restoredAll = $false
            [void]$warnings.Add("旧 LocalAppData 文件回滚失败，归档副本仍保留：$source；$($_.Exception.Message)")
        }
    }
    return $restoredAll
}

function Restore-StoragePointerFromArchive {
    param(
        [Parameter(Mandatory = $true)][string]$Destination,
        [string]$ArchivedPointer,
        [Parameter(Mandatory = $true)][bool]$OriginallyExisted
    )

    if (-not $OriginallyExisted) {
        if (Test-Path -LiteralPath $Destination -PathType Leaf) {
            [System.IO.File]::Delete($Destination)
        }
        return
    }
    if ([string]::IsNullOrWhiteSpace($ArchivedPointer) -or
        -not (Test-Path -LiteralPath $ArchivedPointer -PathType Leaf)) {
        throw "缺少旧路径指针归档，无法回滚。"
    }

    $parent = [System.IO.Path]::GetDirectoryName($Destination)
    $temporary = Join-Path $parent (".storage-path.restore-{0}.tmp" -f `
            [Guid]::NewGuid().ToString("N"))
    $displaced = Join-Path $parent (".storage-path.displaced-{0}.tmp" -f `
            [Guid]::NewGuid().ToString("N"))
    try {
        Copy-Item -LiteralPath $ArchivedPointer -Destination $temporary
        $archiveHash = Get-Sha256ForFile -Path $ArchivedPointer
        if ((Get-Sha256ForFile -Path $temporary) -ne $archiveHash) {
            throw "旧路径指针恢复副本 SHA256 校验失败。"
        }
        if (Test-Path -LiteralPath $Destination -PathType Leaf) {
            [System.IO.File]::Replace($temporary, $Destination, $displaced, $true)
        }
        else {
            [System.IO.File]::Move($temporary, $Destination)
        }
        if ((Get-Sha256ForFile -Path $Destination) -ne $archiveHash) {
            throw "旧路径指针恢复后 SHA256 校验失败。"
        }
        if (Test-Path -LiteralPath $displaced -PathType Leaf) {
            [System.IO.File]::Delete($displaced)
        }
    }
    finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

function Write-StoragePointer {
    param(
        [Parameter(Mandatory = $true)][string]$LocalRoot,
        [Parameter(Mandatory = $true)][string]$StorageRoot,
        [string]$ArchivedPointer
    )

    [System.IO.Directory]::CreateDirectory($LocalRoot) | Out-Null
    Assert-DirectoryIsSafe -Path $LocalRoot -Description "旧 LocalAppData 目录"
    $destination = Join-Path $LocalRoot "storage-path.txt"
    if (Test-StoragePointerMatches -Path $destination -StorageRoot $StorageRoot) {
        return $destination
    }

    $originallyExisted = Test-Path -LiteralPath $destination -PathType Leaf
    if ($originallyExisted) {
        if ([string]::IsNullOrWhiteSpace($ArchivedPointer) -or
            -not (Test-Path -LiteralPath $ArchivedPointer -PathType Leaf)) {
            throw "替换旧路径指针前必须先将其归档到迁移备份目录。"
        }
        if ((Get-Sha256ForFile -Path $destination) -ne
            (Get-Sha256ForFile -Path $ArchivedPointer)) {
            throw "旧路径指针归档副本 SHA256 校验失败，未替换路径指针。"
        }
    }

    $temporary = Join-Path $LocalRoot (".storage-path.{0}.tmp" -f [Guid]::NewGuid().ToString("N"))
    $displacedPointer = Join-Path $LocalRoot (".storage-path.displaced-{0}.tmp" -f `
            [Guid]::NewGuid().ToString("N"))
    try {
        # 与安装器保持一致：UTF-16LE，并显式包含 FF FE BOM。
        [System.IO.File]::WriteAllText($temporary, $StorageRoot, $utf16LeBom)
        $bytes = [System.IO.File]::ReadAllBytes($temporary)
        if ($bytes.Length -lt 2 -or $bytes[0] -ne 0xFF -or $bytes[1] -ne 0xFE) {
            throw "飞花 - PetalDesk 数据存储路径指针未写入 UTF-16LE BOM。"
        }
        $decoded = $utf16LeBom.GetString($bytes, 2, $bytes.Length - 2)
        if (-not $decoded.Equals($StorageRoot, $pathComparison)) {
            throw "飞花 - PetalDesk 数据存储路径指针写入后校验失败。"
        }

        if ($originallyExisted) {
            [System.IO.File]::Replace($temporary, $destination, $displacedPointer, $true)
            if ((Get-Sha256ForFile -Path $displacedPointer) -ne
                (Get-Sha256ForFile -Path $ArchivedPointer)) {
                throw "被替换的旧路径指针与迁移备份 SHA256 不一致。"
            }
        }
        else {
            [System.IO.File]::Move($temporary, $destination)
        }

        if (-not (Test-StoragePointerMatches -Path $destination -StorageRoot $StorageRoot)) {
            throw "飞花 - PetalDesk 数据存储路径指针落盘后校验失败。"
        }
        if (Test-Path -LiteralPath $displacedPointer -PathType Leaf) {
            [System.IO.File]::Delete($displacedPointer)
        }
        return $destination
    }
    catch {
        $writeError = $_
        try {
            Restore-StoragePointerFromArchive -Destination $destination `
                -ArchivedPointer $ArchivedPointer -OriginallyExisted $originallyExisted
        }
        catch {
            [void]$warnings.Add("路径指针回滚失败：$($_.Exception.Message)")
        }
        throw $writeError
    }
    finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
        if (Test-Path -LiteralPath $displacedPointer -PathType Leaf) {
            if (-not [string]::IsNullOrWhiteSpace($ArchivedPointer) -and
                (Test-Path -LiteralPath $ArchivedPointer -PathType Leaf) -and
                (Get-Sha256ForFile -Path $displacedPointer) -eq
                (Get-Sha256ForFile -Path $ArchivedPointer)) {
                Remove-Item -LiteralPath $displacedPointer -Force
            }
            else {
                [void]$warnings.Add("路径指针替换产生的安全副本仍保留：$displacedPointer")
            }
        }
    }
}

function Move-LegacyLocalAppDataToArchive {
    param(
        [Parameter(Mandatory = $true)][string]$LocalRoot,
        [Parameter(Mandatory = $true)][string]$MigrationSessionDirectory,
        [Parameter(Mandatory = $true)][string]$StorageRoot
    )

    [System.IO.Directory]::CreateDirectory($LocalRoot) | Out-Null
    Assert-DirectoryIsSafe -Path $LocalRoot -Description "旧 LocalAppData 目录"
    $pointer = Join-Path $LocalRoot "storage-path.txt"
    if ((Test-Path -LiteralPath $pointer) -and
        -not (Test-Path -LiteralPath $pointer -PathType Leaf)) {
        throw "路径指针必须是普通文件：$pointer"
    }
    $pointerMatches = Test-StoragePointerMatches -Path $pointer -StorageRoot $StorageRoot
    $pointerNeedsArchive = (Test-Path -LiteralPath $pointer -PathType Leaf) -and -not $pointerMatches

    $legacySources = New-Object System.Collections.ArrayList
    foreach ($name in @("settings.json", "windows.json", "reminders.json", "gantt.json")) {
        $candidate = Join-Path $LocalRoot $name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            [void]$legacySources.Add($candidate)
        }
    }
    foreach ($oldPointerBackup in @(Get-ChildItem -LiteralPath $LocalRoot `
            -Filter "storage-path.txt.migration-*.bak*" -File -ErrorAction SilentlyContinue)) {
        [void]$legacySources.Add($oldPointerBackup.FullName)
    }

    if ($legacySources.Count -eq 0 -and -not $pointerNeedsArchive) {
        $script:pointerPath = Write-StoragePointer -LocalRoot $LocalRoot -StorageRoot $StorageRoot
        return
    }

    $archiveDirectory = Join-Path $MigrationSessionDirectory "legacy-localappdata"
    $script:archivedLegacyLocalAppDataPath = $archiveDirectory
    [System.IO.Directory]::CreateDirectory($archiveDirectory) | Out-Null
    Assert-DirectoryIsSafe -Path $archiveDirectory -Description "旧 LocalAppData 迁移备份目录"
    if (-not (Test-IsPathInside -Candidate $archiveDirectory -Parent $MigrationSessionDirectory)) {
        throw "旧 LocalAppData 迁移备份目录越界：$archiveDirectory"
    }

    $businessResults = New-Object System.Collections.ArrayList
    $allArchiveResults = New-Object System.Collections.ArrayList
    $archivedPointerPath = $null
    foreach ($source in @($legacySources)) {
        $destination = Join-Path $archiveDirectory ([System.IO.Path]::GetFileName([string]$source))
        $result = Copy-FileToLegacyArchive -Source ([string]$source) -Destination $destination `
            -Description "归档旧 LocalAppData 业务文件"
        [void]$businessResults.Add($result)
        [void]$allArchiveResults.Add($result)
    }
    if ($pointerNeedsArchive) {
        $archivedPointerPath = Join-Path $archiveDirectory "storage-path.txt"
        $pointerResult = Copy-FileToLegacyArchive -Source $pointer `
            -Destination $archivedPointerPath -Description "归档被替换的旧路径指针"
        [void]$allArchiveResults.Add($pointerResult)
    }

    $removedResults = New-Object System.Collections.ArrayList
    try {
        foreach ($result in @($businessResults)) {
            [System.IO.File]::Delete([string]$result.source)
            if (Test-Path -LiteralPath ([string]$result.source)) {
                throw "旧 LocalAppData 文件移动归档后仍存在：$($result.source)"
            }
            [void]$removedResults.Add($result)
        }
    }
    catch {
        [void](Restore-ArchivedLocalFiles -ArchiveResults @($removedResults))
        throw "旧 LocalAppData 文件归档失败，已尽量恢复源文件：$($_.Exception.Message)"
    }

    try {
        $script:pointerPath = Write-StoragePointer -LocalRoot $LocalRoot -StorageRoot $StorageRoot `
            -ArchivedPointer $archivedPointerPath
    }
    catch {
        [void](Restore-ArchivedLocalFiles -ArchiveResults @($removedResults))
        throw "路径指针更新失败，旧 LocalAppData 文件已尽量恢复：$($_.Exception.Message)"
    }

    foreach ($result in @($allArchiveResults)) {
        $result.action = "archived"
        [void]$copyResults.Add($result)
    }
}

function Write-MigrationReport {
    param(
        [Parameter(Mandatory = $true)][string]$Status,
        [Parameter(Mandatory = $true)][string]$Summary,
        [string]$FailureMessage
    )

    if ($null -eq $sessionDirectory) {
        $fallbackInternal = Join-Path $TargetRoot ".petaldesk"
        $fallbackBackups = Join-Path $fallbackInternal "backups"
        [System.IO.Directory]::CreateDirectory($fallbackBackups) | Out-Null
        $script:sessionDirectory = Join-Path $fallbackBackups ("migration-$timestamp")
        if (Test-Path -LiteralPath $script:sessionDirectory) {
            $script:sessionDirectory = "$script:sessionDirectory-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
        }
        [System.IO.Directory]::CreateDirectory($script:sessionDirectory) | Out-Null
    }
    $script:reportPath = Join-Path $script:sessionDirectory "migration-report.json"

    $report = [ordered]@{
        schemaVersion           = 1
        status                  = $Status
        summary                 = $Summary
        startedAtUtc            = $migrationStartedAt.ToString("o")
        completedAtUtc          = [DateTime]::UtcNow.ToString("o")
        sourceRoot              = $SourceRoot
        targetRoot              = $TargetRoot
        localAppDataRoot        = $LocalAppDataRoot
        notesSourceKind         = $notesSourceKind
        notesSourcePath         = $notesSourcePath
        archivedLegacyNotesPath = $archivedLegacyNotesPath
        archivedLegacyLocalAppDataPath = $archivedLegacyLocalAppDataPath
        archivedLegacyLocalAppDataFiles = @($legacyLocalAppDataArchiveResults)
        storagePointerPath      = $pointerPath
        storagePointerEncoding  = "UTF-16LE with BOM"
        copiedFileCount         = @($copyResults | Where-Object { $_.action -eq "copied" }).Count
        archivedFileCount       = @($copyResults | Where-Object { $_.action -eq "archived" }).Count
        verifiedFileCount       = @($copyResults | Where-Object { $_.verified }).Count
        unchangedFileCount      = @($copyResults | Where-Object {
                $_.action -in @("identical", "samePath", "samePathDirectory", "archiveVerified")
            }).Count
        conflictCount           = $conflicts.Count
        failure                 = $FailureMessage
        warnings                = @($warnings)
        files                   = @($copyResults)
        conflicts               = @($conflicts)
    }
    $json = $report | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText($script:reportPath, $json + [Environment]::NewLine, $utf8NoBom)
    return $script:reportPath
}

$legacyProcessName = @("fei", "hua") -join ""
$runningProcesses = @(Get-Process -Name "petaldesk", $legacyProcessName -ErrorAction SilentlyContinue)
if ($runningProcesses.Count -gt 0) {
    $details = ($runningProcesses | ForEach-Object {
            "{0} (PID {1})" -f $_.ProcessName, $_.Id
        }) -join "、"
    throw "检测到飞花 - PetalDesk 仍在运行：$details。请先从托盘显式退出飞花 - PetalDesk，再执行迁移。"
}

$SourceRoot = Get-NormalizedPath -Path $SourceRoot
if ([string]::IsNullOrWhiteSpace($TargetRoot)) {
    $TargetRoot = $SourceRoot
}
else {
    $TargetRoot = Get-NormalizedPath -Path $TargetRoot
}
$LocalAppDataRoot = Get-NormalizedPath -Path $LocalAppDataRoot

if (-not (Test-Path -LiteralPath $SourceRoot -PathType Container)) {
    throw "飞花 - PetalDesk 源数据目录不存在：$SourceRoot"
}

$legacyInternalName = "." + (@("fei", "hua") -join "")
$currentSourceInternal = Join-Path $SourceRoot ".petaldesk"
$legacySourceInternal = Join-Path $SourceRoot $legacyInternalName
$sourceInternal = if ((Test-Path -LiteralPath $legacySourceInternal -PathType Container) -and
    -not (Test-Path -LiteralPath $currentSourceInternal -PathType Container)) {
    $legacySourceInternal
}
else {
    $currentSourceInternal
}
$targetInternal = Join-Path $TargetRoot ".petaldesk"
$sameRoot = Test-PathsEqual -First $SourceRoot -Second $TargetRoot
Assert-DirectoryIsSafe -Path $SourceRoot -Description "飞花 - PetalDesk 源数据目录"
Assert-DirectoryIsSafe -Path $LocalAppDataRoot -Description "旧 LocalAppData 目录" -AllowMissing
Assert-TargetStoragePathsSafe -Root $TargetRoot -AllowMissing

if (Test-PathsOverlap -First $LocalAppDataRoot -Second $SourceRoot) {
    throw "LocalAppDataRoot 不能与 SourceRoot 或其内部目录重叠：$LocalAppDataRoot"
}
if (Test-PathsOverlap -First $LocalAppDataRoot -Second $TargetRoot) {
    throw "LocalAppDataRoot 不能与 TargetRoot、.petaldesk/notes 或 .petaldesk/backups 重叠：$LocalAppDataRoot"
}
if (-not $sameRoot -and (Test-IsPathInside -Candidate $SourceRoot -Parent $targetInternal)) {
    throw "SourceRoot 不能位于目标 .petaldesk 内部：$SourceRoot"
}

$legacyNotes = Join-Path $SourceRoot "notes"
$newNotes = Join-Path $sourceInternal "notes"
$sourceNewNotesExists = Test-Path -LiteralPath $newNotes -PathType Container
$sourceLegacyNotesExists = Test-Path -LiteralPath $legacyNotes -PathType Container
$sourceConfig = Join-Path $sourceInternal "config.json"
$sourceConfigExists = Test-Path -LiteralPath $sourceConfig -PathType Leaf
$newDirectoryAvailability = @{}
foreach ($name in @("state", "tools", "backups", "journal", "trash", "conflicts")) {
    $newDirectoryAvailability[$name] = Test-Path -LiteralPath (Join-Path $sourceInternal $name) -PathType Container
}

if ($sourceNewNotesExists) {
    $notesSourceKind = "newLayout"
    $notesSourcePath = $newNotes
}
elseif ($sourceLegacyNotesExists) {
    $notesSourceKind = "legacyNotes"
    $notesSourcePath = $legacyNotes
}
else {
    [void]$warnings.Add("没有找到 .petaldesk\\notes 或旧 notes 目录，将只迁移配置和小工具数据。")
}

if (-not $sameRoot -and (Test-IsPathInside -Candidate $TargetRoot -Parent $SourceRoot)) {
    throw "飞花 - PetalDesk 目标数据存储不能位于源目录内部：$TargetRoot"
}
if ($sameRoot -and $sourceLegacyNotesExists) {
    $archiveLegacyNotesAfterSuccess = $true
}

foreach ($name in @("notes", "state", "tools", "backups", "journal", "trash", "conflicts")) {
    [System.IO.Directory]::CreateDirectory((Join-Path $targetInternal $name)) | Out-Null
}
Assert-TargetStoragePathsSafe -Root $TargetRoot

$sessionDirectory = Join-Path (Join-Path $targetInternal "backups") ("migration-$timestamp")
if (Test-Path -LiteralPath $sessionDirectory) {
    $sessionDirectory = "$sessionDirectory-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
}
[System.IO.Directory]::CreateDirectory($sessionDirectory) | Out-Null
Assert-MigrationSessionSafe -SessionPath $sessionDirectory `
    -BackupsPath (Join-Path $targetInternal "backups")

try {
    if ($null -ne $notesSourcePath) {
        Add-DirectoryOperations -Category "notes" -Source $notesSourcePath `
            -Destination (Join-Path $targetInternal "notes")
    }
    if ($sameRoot -and $sourceNewNotesExists -and $sourceLegacyNotesExists) {
        Add-LegacyNotesArchiveVerification -LegacyDirectory $legacyNotes `
            -NewNotesDirectory $newNotes
    }

    foreach ($name in @("state", "tools", "backups", "journal", "trash", "conflicts")) {
        if ([bool]$newDirectoryAvailability[$name]) {
            Add-DirectoryOperations -Category $name -Source (Join-Path $sourceInternal $name) `
                -Destination (Join-Path $targetInternal $name)
        }
    }

    $targetConfig = Join-Path $targetInternal "config.json"
    if ($sourceConfigExists) {
        Add-FileOperation -Category "config" -Source $sourceConfig -Destination $targetConfig `
            -Description "迁移飞花 - PetalDesk 数据配置"
    }
    else {
        $legacySettings = Join-Path $LocalAppDataRoot "settings.json"
        if (Test-Path -LiteralPath $legacySettings -PathType Leaf) {
            $configBytes = Convert-LegacySettingsToConfigBytes -SettingsPath $legacySettings
            Add-FileOperation -Category "config" -Content $configBytes -Destination $targetConfig `
                -Description "从旧 settings.json 迁移默认编辑样式"
        }
        else {
            $configBytes = New-DefaultConfigBytes
            Add-FileOperation -Category "config" -Content $configBytes -Destination $targetConfig `
                -Description "创建飞花 - PetalDesk 默认数据配置"
        }
    }

    $legacyFileMappings = @(
        [pscustomobject]@{
            name = "windows.json"; category = "state";
            destination = Join-Path (Join-Path $targetInternal "state") "windows.json"
        },
        [pscustomobject]@{
            name = "reminders.json"; category = "tools";
            destination = Join-Path (Join-Path $targetInternal "tools") "reminders.json"
        },
        [pscustomobject]@{
            name = "gantt.json"; category = "tools";
            destination = Join-Path (Join-Path $targetInternal "tools") "gantt.json"
        }
    )
    foreach ($mapping in $legacyFileMappings) {
        $newLayoutCandidate = Join-Path (Join-Path $sourceInternal ([string]$mapping.category)) `
            ([string]$mapping.name)
        if (-not (Test-Path -LiteralPath $newLayoutCandidate -PathType Leaf)) {
            $legacySource = Join-Path $LocalAppDataRoot ([string]$mapping.name)
            Add-FileOperation -Category ([string]$mapping.category) -Source $legacySource `
                -Destination ([string]$mapping.destination) `
                -Description ("迁移旧 {0}" -f [string]$mapping.name)
        }
    }

    if ($conflicts.Count -gt 0) {
        $summary = "迁移未执行：发现 $($conflicts.Count) 个目标内容冲突，所有冲突文件均未覆盖。"
        $writtenReport = Write-MigrationReport -Status "conflict" -Summary $summary `
            -FailureMessage "目标存在不同内容"
        Write-Host $summary -ForegroundColor Red
        Write-Host "JSON 迁移报告：$writtenReport"
        throw "发现迁移冲突，请查看报告后处理：$writtenReport"
    }

    foreach ($directory in @($directoriesToCreate | Sort-Object { ([string]$_).Length })) {
        [System.IO.Directory]::CreateDirectory([string]$directory) | Out-Null
        Assert-DirectoryIsSafe -Path ([string]$directory) -Description "迁移目标目录"
    }
    foreach ($operation in @($copyOperations)) {
        Copy-OperationAtomically -Operation $operation
    }

    $inPlace = Test-PathsEqual -First $SourceRoot -Second $TargetRoot
    if ($inPlace -and $archiveLegacyNotesAfterSuccess -and `
        (Test-Path -LiteralPath $legacyNotes -PathType Container)) {
        $archiveDestination = Join-Path $sessionDirectory "legacy-notes"
        if (Test-Path -LiteralPath $archiveDestination) {
            throw "旧 notes 归档目标已存在，未移动源数据：$archiveDestination"
        }

        Move-Item -LiteralPath $legacyNotes -Destination $archiveDestination
        $archivedLegacyNotesPath = $archiveDestination

        foreach ($result in @($copyResults | Where-Object {
                    $_.category -in @("notes", "notesLegacyArchive") -and
                    $_.source -and $_.sourceSha256 -and
                    ([string]$_.source).StartsWith(
                        $legacyNotes + [System.IO.Path]::DirectorySeparatorChar,
                        $pathComparison
                    )
                })) {
            $relative = ([string]$result.source).Substring($legacyNotes.Length).TrimStart(
                [System.IO.Path]::DirectorySeparatorChar,
                [System.IO.Path]::AltDirectorySeparatorChar
            )
            $archivedFile = Join-Path $archiveDestination $relative
            if (-not (Test-Path -LiteralPath $archivedFile -PathType Leaf)) {
                throw "旧 notes 移动归档后缺少文件：$archivedFile"
            }
            $archiveHash = Get-Sha256ForFile -Path $archivedFile
            if ($archiveHash -ne [string]$result.sourceSha256) {
                throw "旧 notes 归档后 SHA256 校验失败：$archivedFile"
            }
        }
    }

    Move-LegacyLocalAppDataToArchive -LocalRoot $LocalAppDataRoot `
        -MigrationSessionDirectory $sessionDirectory -StorageRoot $TargetRoot
    $copiedCount = @($copyResults | Where-Object { $_.action -eq "copied" }).Count
    $archivedCount = @($copyResults | Where-Object { $_.action -eq "archived" }).Count
    $verifiedCount = @($copyResults | Where-Object { $_.verified }).Count
    $summary = "迁移成功：复制 $copiedCount 个文件，归档 $archivedCount 个旧文件，SHA256 校验 $verifiedCount 个文件，未覆盖任何已有不同内容。"
    if ($null -ne $archivedLegacyNotesPath) {
        $summary += " 旧 notes 已移动保留到迁移备份目录。"
    }
    $writtenReport = Write-MigrationReport -Status "success" -Summary $summary
    Write-Host $summary -ForegroundColor Green
    Write-Host "飞花 - PetalDesk 数据存储：$TargetRoot"
    Write-Host "路径指针：$pointerPath（UTF-16LE with BOM）"
    Write-Host "JSON 迁移报告：$writtenReport"
    Write-Output $writtenReport
}
catch {
    if ($null -eq $reportPath -or -not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
        $failure = $_.Exception.Message
        try {
            $writtenReport = Write-MigrationReport -Status "failed" `
                -Summary "迁移失败；已复制或归档的数据仍保留在目标目录或迁移备份中，部分旧位置可能已完成安全归档。" `
                -FailureMessage $failure
            Write-Host "迁移失败；已复制或归档的数据仍保留，详情请查看迁移报告。" -ForegroundColor Red
            Write-Host "JSON 迁移报告：$writtenReport"
        }
        catch {
            Write-Host "迁移报告写入失败：$($_.Exception.Message)" -ForegroundColor Red
        }
    }
    throw
}

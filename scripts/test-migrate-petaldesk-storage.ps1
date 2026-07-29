[CmdletBinding()]
param(
    [switch]$KeepArtifacts
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$migrationScript = Join-Path $PSScriptRoot "migrate-petaldesk-storage.ps1"
$utf8NoBom = New-Object System.Text.UTF8Encoding -ArgumentList $false
$sandbox = Join-Path ([System.IO.Path]::GetTempPath()) `
    ("petaldesk-migration-test-{0}" -f [Guid]::NewGuid().ToString("N"))

function Assert-True {
    param(
        [Parameter(Mandatory = $true)][bool]$Condition,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if (-not $Condition) {
        throw "断言失败：$Message"
    }
}

function Assert-Equal {
    param(
        $Expected,
        $Actual,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if ($Expected -ne $Actual) {
        throw "断言失败：$Message。期望：$Expected；实际：$Actual"
    }
}

function Write-Utf8Json {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)]$Value
    )
    [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($Path)) | Out-Null
    $json = ($Value | ConvertTo-Json -Depth 8) + [Environment]::NewLine
    [System.IO.File]::WriteAllText($Path, $json, $utf8NoBom)
}

function Write-Utf8Text {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Value
    )
    [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($Path)) | Out-Null
    [System.IO.File]::WriteAllText($Path, $Value, $utf8NoBom)
}

function Assert-Pointer {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedRoot
    )
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    Assert-True -Condition ($bytes.Length -ge 2) -Message "路径指针至少包含 BOM"
    Assert-Equal -Expected 255 -Actual ([int]$bytes[0]) -Message "路径指针 BOM 第一个字节"
    Assert-Equal -Expected 254 -Actual ([int]$bytes[1]) -Message "路径指针 BOM 第二个字节"
    $decoded = [System.Text.Encoding]::Unicode.GetString($bytes, 2, $bytes.Length - 2)
    Assert-Equal -Expected ([System.IO.Path]::GetFullPath($ExpectedRoot)) -Actual $decoded `
        -Message "路径指针内容"
}

function Invoke-Migration {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [string]$Target,
        [Parameter(Mandatory = $true)][string]$Local
    )
    if ([string]::IsNullOrWhiteSpace($Target)) {
        $output = @(& $migrationScript -SourceRoot $Source -LocalAppDataRoot $Local)
    }
    else {
        $output = @(& $migrationScript -SourceRoot $Source -TargetRoot $Target `
                -LocalAppDataRoot $Local)
    }
    $path = [string]$output[-1]
    Assert-True -Condition (Test-Path -LiteralPath $path -PathType Leaf) `
        -Message "迁移脚本返回 JSON 报告路径"
    return $path
}

function Assert-StorageStructure {
    param(
        [Parameter(Mandatory = $true)][string]$Root
    )
    foreach ($name in @("notes", "state", "tools", "backups", "journal", "trash", "conflicts")) {
        Assert-True -Condition (Test-Path -LiteralPath (Join-Path $Root ".petaldesk\$name") `
                -PathType Container) -Message "目标结构包含 .petaldesk\\$name"
    }
}

New-Item -ItemType Directory -Path $sandbox -Force | Out-Null
try {
    # 场景一：旧布局在原目录升级，并把根 notes 移动到带时间戳的保留备份。
    $inPlaceRoot = Join-Path $sandbox "in-place\PetalDesk数据"
    $inPlaceLocal = Join-Path $sandbox "in-place\local\PetalDesk"
    $noteId = "11111111-1111-4111-8111-111111111111"
    $legacyNote = Join-Path (Join-Path $inPlaceRoot "notes") $noteId
    Write-Utf8Text -Path (Join-Path $legacyNote "note.md") -Value "# 原地迁移便签`n`n中文内容"
    Write-Utf8Json -Path (Join-Path $legacyNote "meta.json") -Value ([ordered]@{
            id = $noteId; color = "yellow"; revision = 1
        })
    Write-Utf8Text -Path (Join-Path $legacyNote "assets\image.txt") -Value "asset-content"
    Write-Utf8Json -Path (Join-Path $inPlaceLocal "settings.json") -Value ([ordered]@{
            workspacePath = $inPlaceRoot; defaultEditorMode = "plain"
        })
    Write-Utf8Json -Path (Join-Path $inPlaceLocal "windows.json") -Value ([ordered]@{
            windows = [ordered]@{}; openNotes = @($noteId); lastNoteId = $noteId
        })
    Write-Utf8Json -Path (Join-Path $inPlaceLocal "reminders.json") -Value @(
        [ordered]@{ id = "reminder-1"; title = "测试提醒" }
    )
    Write-Utf8Json -Path (Join-Path $inPlaceLocal "gantt.json") -Value @(
        [ordered]@{ id = "task-1"; name = "测试任务" }
    )
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "storage-path.txt") -Value "C:\旧PetalDesk目录"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal `
            "storage-path.txt.migration-legacy.bak") -Value "C:\更早的PetalDesk目录"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "search-index.sqlite3") -Value "index-main"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "search-index.sqlite3-wal") -Value "index-wal"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "search-index.sqlite3-shm") -Value "index-shm"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "activation-trace.log") -Value "trace-log"
    Write-Utf8Text -Path (Join-Path $inPlaceLocal "cache\cache.bin") -Value "cache-data"

    $nonBusinessHashes = @{}
    foreach ($relative in @("search-index.sqlite3", "search-index.sqlite3-wal",
            "search-index.sqlite3-shm", "activation-trace.log", "cache\cache.bin")) {
        $nonBusinessHashes[$relative] = (Get-FileHash -LiteralPath (Join-Path $inPlaceLocal $relative) `
                -Algorithm SHA256).Hash
    }

    $legacyNoteHash = (Get-FileHash -LiteralPath (Join-Path $legacyNote "note.md") `
            -Algorithm SHA256).Hash
    $inPlaceReportPath = Invoke-Migration -Source $inPlaceRoot -Local $inPlaceLocal
    $inPlaceReport = Get-Content -LiteralPath $inPlaceReportPath -Raw -Encoding UTF8 |
        ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $inPlaceReport.status -Message "原地迁移状态"
    Assert-Equal -Expected "legacyNotes" -Actual $inPlaceReport.notesSourceKind `
        -Message "原地迁移选择旧 notes"
    Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $inPlaceRoot "notes"))) `
        -Message "旧根 notes 已移动归档"
    $migratedNote = Join-Path $inPlaceRoot ".petaldesk\notes\$noteId\note.md"
    Assert-True -Condition (Test-Path -LiteralPath $migratedNote -PathType Leaf) `
        -Message "原地迁移后的 note.md"
    Assert-Equal -Expected $legacyNoteHash `
        -Actual (Get-FileHash -LiteralPath $migratedNote -Algorithm SHA256).Hash `
        -Message "原地迁移后的 note.md SHA256"
    Assert-True -Condition (Test-Path -LiteralPath $inPlaceReport.archivedLegacyNotesPath `
            -PathType Container) -Message "原地迁移旧 notes 归档存在"
    $archivedNote = Join-Path $inPlaceReport.archivedLegacyNotesPath "$noteId\note.md"
    Assert-Equal -Expected $legacyNoteHash `
        -Actual (Get-FileHash -LiteralPath $archivedNote -Algorithm SHA256).Hash `
        -Message "旧 notes 归档 SHA256"
    $config = Get-Content -LiteralPath (Join-Path $inPlaceRoot ".petaldesk\config.json") `
        -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "plain" -Actual $config.defaultEditorMode `
        -Message "settings.json 默认编辑样式"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $inPlaceRoot `
                ".petaldesk\state\windows.json") -PathType Leaf) -Message "windows.json 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $inPlaceRoot `
                ".petaldesk\tools\reminders.json") -PathType Leaf) -Message "reminders.json 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $inPlaceRoot `
                ".petaldesk\tools\gantt.json") -PathType Leaf) -Message "gantt.json 已迁移"
    Assert-StorageStructure -Root $inPlaceRoot
    Assert-Pointer -Path (Join-Path $inPlaceLocal "storage-path.txt") -ExpectedRoot $inPlaceRoot
    Assert-True -Condition (Test-Path -LiteralPath $inPlaceReport.archivedLegacyLocalAppDataPath `
            -PathType Container) -Message "旧 LocalAppData 归档目录存在"
    Assert-Equal -Expected 6 -Actual ([int]$inPlaceReport.archivedFileCount) `
        -Message "四个旧业务文件、旧指针及遗留指针备份均归档"
    Assert-Equal -Expected 6 -Actual @($inPlaceReport.archivedLegacyLocalAppDataFiles).Count `
        -Message "迁移报告逐文件记录旧 LocalAppData 归档"
    foreach ($name in @("settings.json", "windows.json", "reminders.json", "gantt.json",
            "storage-path.txt", "storage-path.txt.migration-legacy.bak")) {
        Assert-True -Condition (Test-Path -LiteralPath (Join-Path `
                    $inPlaceReport.archivedLegacyLocalAppDataPath $name) -PathType Leaf) `
            -Message "迁移备份包含 $name"
    }
    foreach ($name in @("settings.json", "windows.json", "reminders.json", "gantt.json")) {
        Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $inPlaceLocal $name))) `
            -Message "LocalAppData 已移除旧业务文件 $name"
    }
    Assert-Equal -Expected 0 -Actual @(Get-ChildItem -LiteralPath $inPlaceLocal `
            -Filter "storage-path.txt.migration-*.bak*" -File).Count `
        -Message "LocalAppData 不遗留旧路径指针备份"
    foreach ($relative in $nonBusinessHashes.Keys) {
        Assert-True -Condition (Test-Path -LiteralPath (Join-Path $inPlaceLocal $relative) `
                -PathType Leaf) -Message "非业务文件保持存在：$relative"
        Assert-Equal -Expected $nonBusinessHashes[$relative] `
            -Actual (Get-FileHash -LiteralPath (Join-Path $inPlaceLocal $relative) `
                -Algorithm SHA256).Hash -Message "非业务文件内容保持不变：$relative"
    }
    $allowedLocalFiles = @("storage-path.txt", "search-index.sqlite3", "search-index.sqlite3-wal",
        "search-index.sqlite3-shm", "activation-trace.log")
    $unexpectedLocalFiles = @(Get-ChildItem -LiteralPath $inPlaceLocal -File -Force |
        Where-Object { $_.Name -notin $allowedLocalFiles })
    Assert-Equal -Expected 0 -Actual $unexpectedLocalFiles.Count `
        -Message "LocalAppData 根目录只保留当前指针、索引和日志"

    # 旧版本可能留下扩展路径前缀；迁移应视为同一路径并修复显示格式。
    $extendedPointerValue = "\\?\" + [System.IO.Path]::GetFullPath($inPlaceRoot)
    [System.IO.File]::WriteAllText(
        (Join-Path $inPlaceLocal "storage-path.txt"),
        $extendedPointerValue,
        [System.Text.Encoding]::Unicode
    )

    # 原地重复执行应保持成功，不覆盖、不复制或归档出不同内容。
    $inPlaceSecondReportPath = Invoke-Migration -Source $inPlaceRoot -Local $inPlaceLocal
    $inPlaceSecondReport = Get-Content -LiteralPath $inPlaceSecondReportPath -Raw -Encoding UTF8 |
        ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $inPlaceSecondReport.status -Message "原地幂等重跑状态"
    Assert-Equal -Expected 0 -Actual ([int]$inPlaceSecondReport.conflictCount) `
        -Message "原地幂等重跑无冲突"
    Assert-True -Condition ($null -eq $inPlaceSecondReport.archivedLegacyLocalAppDataPath) `
        -Message "扩展前缀指针与普通目标等价，不创建新的旧 LocalAppData 归档"
    Assert-Equal -Expected 0 -Actual ([int]$inPlaceSecondReport.archivedFileCount) `
        -Message "扩展前缀指针重跑不触发归档"
    $savedExtendedPointerBytes = [System.IO.File]::ReadAllBytes(
        (Join-Path $inPlaceLocal "storage-path.txt")
    )
    $savedExtendedPointer = [System.Text.Encoding]::Unicode.GetString(
        $savedExtendedPointerBytes,
        2,
        $savedExtendedPointerBytes.Length - 2
    )
    Assert-Equal -Expected ([System.IO.Path]::GetFullPath($inPlaceRoot)) `
        -Actual $savedExtendedPointer -Message "等价的扩展前缀指针已改写为普通路径"
    Assert-True -Condition (-not $savedExtendedPointer.StartsWith("\\?\")) `
        -Message "迁移后的路径指针不包含 Windows 内部扩展前缀"
    Assert-Equal -Expected 1 -Actual @(Get-ChildItem -LiteralPath (Join-Path $inPlaceRoot `
            ".petaldesk\backups") -Directory -Filter "legacy-localappdata" -Recurse).Count `
        -Message "路径指针相同时幂等重跑不重复归档"

    # 场景二：应用已自动复制到新布局，但旧根 notes 尚未归档。
    $dualRoot = Join-Path $sandbox "dual-layout\source"
    $dualLocal = Join-Path $sandbox "dual-layout\local\PetalDesk"
    $dualNoteId = "55555555-5555-4555-8555-555555555555"
    $dualLegacyNote = Join-Path $dualRoot "notes\$dualNoteId"
    $dualNewNote = Join-Path $dualRoot ".petaldesk\notes\$dualNoteId"
    Write-Utf8Text -Path (Join-Path $dualLegacyNote "note.md") -Value "自动迁移后待归档"
    Write-Utf8Json -Path (Join-Path $dualLegacyNote "meta.json") `
        -Value ([ordered]@{ id = $dualNoteId; revision = 7 })
    Write-Utf8Text -Path (Join-Path $dualLegacyNote "assets\same.txt") -Value "same-asset"
    Write-Utf8Text -Path (Join-Path $dualNewNote "note.md") -Value "自动迁移后待归档"
    Write-Utf8Json -Path (Join-Path $dualNewNote "meta.json") `
        -Value ([ordered]@{ id = $dualNoteId; revision = 7 })
    Write-Utf8Text -Path (Join-Path $dualNewNote "assets\same.txt") -Value "same-asset"

    $dualReportPath = Invoke-Migration -Source $dualRoot -Local $dualLocal
    $dualReport = Get-Content -LiteralPath $dualReportPath -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $dualReport.status -Message "新旧 notes 并存迁移状态"
    Assert-Equal -Expected "newLayout" -Actual $dualReport.notesSourceKind `
        -Message "新旧 notes 并存时仍以新布局为真相"
    Assert-Equal -Expected 3 -Actual @($dualReport.files | Where-Object {
            $_.action -eq "archiveVerified"
        }).Count -Message "旧 notes 每个文件均完成 SHA256 对照"
    Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $dualRoot "notes"))) `
        -Message "SHA256 全部一致后旧 notes 已归档"
    Assert-True -Condition (Test-Path -LiteralPath $dualReport.archivedLegacyNotesPath `
            -PathType Container) -Message "新旧 notes 并存归档目录存在"
    Assert-Equal -Expected (Get-FileHash -LiteralPath (Join-Path $dualNewNote "note.md") `
            -Algorithm SHA256).Hash `
        -Actual (Get-FileHash -LiteralPath (Join-Path $dualReport.archivedLegacyNotesPath `
                "$dualNoteId\note.md") -Algorithm SHA256).Hash `
        -Message "新真相与归档 note.md SHA256 一致"

    $dualSecondReportPath = Invoke-Migration -Source $dualRoot -Local $dualLocal
    $dualSecondReport = Get-Content -LiteralPath $dualSecondReportPath -Raw -Encoding UTF8 |
        ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $dualSecondReport.status `
        -Message "新旧 notes 并存归档后幂等重跑"
    Assert-Equal -Expected 1 -Actual @(Get-ChildItem -LiteralPath (Join-Path $dualRoot `
            ".petaldesk\backups") -Directory -Filter "legacy-notes" -Recurse).Count `
        -Message "幂等重跑不重复归档旧 notes"

    # 场景三：新旧 notes 并存但缺文件或内容不同时，必须保留两边并报告冲突。
    $dualConflictRoot = Join-Path $sandbox "dual-conflict\source"
    $dualConflictLocal = Join-Path $sandbox "dual-conflict\local\PetalDesk"
    $dualConflictId = "66666666-6666-4666-8666-666666666666"
    $dualConflictLegacy = Join-Path $dualConflictRoot "notes\$dualConflictId"
    $dualConflictNew = Join-Path $dualConflictRoot ".petaldesk\notes\$dualConflictId"
    Write-Utf8Text -Path (Join-Path $dualConflictLegacy "note.md") -Value "旧内容"
    Write-Utf8Text -Path (Join-Path $dualConflictLegacy "missing.txt") -Value "新布局缺少我"
    Write-Utf8Text -Path (Join-Path $dualConflictNew "note.md") -Value "新内容"
    Write-Utf8Json -Path (Join-Path $dualConflictLocal "settings.json") `
        -Value ([ordered]@{ workspacePath = $dualConflictRoot; defaultEditorMode = "plain" })
    Write-Utf8Json -Path (Join-Path $dualConflictLocal "windows.json") `
        -Value ([ordered]@{ windows = [ordered]@{} })
    Write-Utf8Text -Path (Join-Path $dualConflictLocal `
            "storage-path.txt.migration-old.bak") -Value "旧指针备份"
    $dualConflictThrew = $false
    try {
        & $migrationScript -SourceRoot $dualConflictRoot -LocalAppDataRoot $dualConflictLocal |
            Out-Null
    }
    catch {
        $dualConflictThrew = $true
    }
    Assert-True -Condition $dualConflictThrew -Message "新旧 notes 不一致时迁移失败"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $dualConflictLegacy "note.md") `
            -PathType Leaf) -Message "冲突时旧 notes 未移动"
    Assert-Equal -Expected "新内容" `
        -Actual ([System.IO.File]::ReadAllText((Join-Path $dualConflictNew "note.md"), `
                [System.Text.Encoding]::UTF8)) -Message "冲突时新 notes 未覆盖"
    $dualConflictReports = @(Get-ChildItem -LiteralPath (Join-Path $dualConflictRoot `
                ".petaldesk\backups") -Filter "migration-report.json" -File -Recurse)
    Assert-Equal -Expected 1 -Actual $dualConflictReports.Count `
        -Message "新旧 notes 冲突生成一份报告"
    $dualConflictReport = Get-Content -LiteralPath $dualConflictReports[0].FullName `
        -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "conflict" -Actual $dualConflictReport.status `
        -Message "新旧 notes 冲突报告状态"
    Assert-Equal -Expected 2 -Actual ([int]$dualConflictReport.conflictCount) `
        -Message "内容不同和文件缺失均记录冲突"
    Assert-Equal -Expected 2 -Actual @($dualConflictReport.conflicts | Where-Object {
            $_.category -eq "notesLegacyArchive"
        }).Count -Message "新旧 notes 冲突类别"
    Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $dualConflictLocal `
                    "storage-path.txt"))) -Message "新旧 notes 冲突时不更新路径指针"
    foreach ($name in @("settings.json", "windows.json", "storage-path.txt.migration-old.bak")) {
        Assert-True -Condition (Test-Path -LiteralPath (Join-Path $dualConflictLocal $name) `
                -PathType Leaf) -Message "冲突时不归档 LocalAppData 文件 $name"
    }
    Assert-Equal -Expected 0 -Actual @(Get-ChildItem -LiteralPath (Join-Path $dualConflictRoot `
            ".petaldesk\backups") -Directory -Filter "legacy-localappdata" -Recurse).Count `
        -Message "冲突时不创建旧 LocalAppData 归档"

    # 场景四：仅存在旧内部目录时，原地升级到 .petaldesk 并保留旧源数据。
    $legacyInternalRoot = Join-Path $sandbox "legacy-internal\source"
    $legacyInternalLocal = Join-Path $sandbox "legacy-internal\local\PetalDesk"
    $legacyInternalName = "." + (@("fei", "hua") -join "")
    $legacyInternalPath = Join-Path $legacyInternalRoot $legacyInternalName
    $legacyInternalNoteId = "88888888-8888-4888-8888-888888888888"
    $legacyInternalNote = Join-Path $legacyInternalPath "notes\$legacyInternalNoteId"
    Write-Utf8Text -Path (Join-Path $legacyInternalNote "note.md") `
        -Value "旧内部目录便签"
    Write-Utf8Json -Path (Join-Path $legacyInternalNote "meta.json") `
        -Value ([ordered]@{ id = $legacyInternalNoteId; revision = 9 })
    Write-Utf8Json -Path (Join-Path $legacyInternalPath "config.json") `
        -Value ([ordered]@{ schemaVersion = 1; defaultEditorMode = "plain" })
    Write-Utf8Json -Path (Join-Path $legacyInternalPath "state\windows.json") `
        -Value ([ordered]@{ openNotes = @($legacyInternalNoteId) })
    Write-Utf8Json -Path (Join-Path $legacyInternalPath "tools\timer.json") `
        -Value ([ordered]@{ elapsedMilliseconds = 5678 })
    Write-Utf8Json -Path (Join-Path $legacyInternalPath "journal\draft.json") `
        -Value ([ordered]@{ noteId = $legacyInternalNoteId; markdown = "draft" })
    $legacyInternalNoteHash = (Get-FileHash -LiteralPath `
            (Join-Path $legacyInternalNote "note.md") -Algorithm SHA256).Hash

    $legacyInternalReportPath = Invoke-Migration -Source $legacyInternalRoot `
        -Local $legacyInternalLocal
    $legacyInternalReport = Get-Content -LiteralPath $legacyInternalReportPath `
        -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $legacyInternalReport.status `
        -Message "旧内部目录迁移状态"
    Assert-Equal -Expected "newLayout" -Actual $legacyInternalReport.notesSourceKind `
        -Message "旧内部目录作为结构化 notes 来源"
    $migratedLegacyInternalNote = Join-Path $legacyInternalRoot `
        ".petaldesk\notes\$legacyInternalNoteId\note.md"
    Assert-Equal -Expected $legacyInternalNoteHash `
        -Actual (Get-FileHash -LiteralPath $migratedLegacyInternalNote -Algorithm SHA256).Hash `
        -Message "旧内部目录 note.md SHA256"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $legacyInternalRoot `
                ".petaldesk\state\windows.json") -PathType Leaf) `
        -Message "旧内部目录 windows.json 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $legacyInternalRoot `
                ".petaldesk\tools\timer.json") -PathType Leaf) `
        -Message "旧内部目录 timer.json 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $legacyInternalRoot `
                ".petaldesk\journal\draft.json") -PathType Leaf) `
        -Message "旧内部目录 journal 已迁移"
    $legacyInternalConfig = Get-Content -LiteralPath (Join-Path $legacyInternalRoot `
            ".petaldesk\config.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "plain" -Actual $legacyInternalConfig.defaultEditorMode `
        -Message "旧内部目录 config.json 已迁移"
    Assert-StorageStructure -Root $legacyInternalRoot
    Assert-Pointer -Path (Join-Path $legacyInternalLocal "storage-path.txt") `
        -ExpectedRoot $legacyInternalRoot
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $legacyInternalNote "note.md") `
            -PathType Leaf) -Message "旧内部目录源数据保留"

    # 场景五：跨目录迁移。新布局 notes 优先，旧根 notes 保留且不参与复制。
    $crossSource = Join-Path $sandbox "cross\source"
    $crossTarget = Join-Path $sandbox "cross\target"
    $crossLocal = Join-Path $sandbox "cross\local\PetalDesk"
    $newNoteId = "22222222-2222-4222-8222-222222222222"
    $ignoredLegacyId = "33333333-3333-4333-8333-333333333333"
    Write-Utf8Text -Path (Join-Path $crossSource ".petaldesk\notes\$newNoteId\note.md") `
        -Value "新布局优先"
    Write-Utf8Json -Path (Join-Path $crossSource ".petaldesk\notes\$newNoteId\meta.json") `
        -Value ([ordered]@{ id = $newNoteId; revision = 3 })
    Write-Utf8Text -Path (Join-Path $crossSource "notes\$ignoredLegacyId\note.md") `
        -Value "该旧便签不应覆盖新布局选择"
    Write-Utf8Json -Path (Join-Path $crossSource ".petaldesk\tools\timer.json") `
        -Value ([ordered]@{ elapsedMilliseconds = 1234; digitOpacity = 0.8 })
    Write-Utf8Json -Path (Join-Path $crossSource ".petaldesk\journal\draft.json") `
        -Value ([ordered]@{ noteId = $newNoteId; markdown = "draft" })
    Write-Utf8Json -Path (Join-Path $crossSource ".petaldesk\config.json") `
        -Value ([ordered]@{ schemaVersion = 1; defaultEditorMode = "typora" })
    Write-Utf8Json -Path (Join-Path $crossLocal "settings.json") -Value ([ordered]@{
            workspacePath = $crossSource; defaultEditorMode = "plain"
        })
    Write-Utf8Json -Path (Join-Path $crossLocal "windows.json") -Value ([ordered]@{
            windows = [ordered]@{}; openNotes = @($newNoteId)
        })
    Write-Utf8Json -Path (Join-Path $crossLocal "reminders.json") -Value @()
    Write-Utf8Json -Path (Join-Path $crossLocal "gantt.json") -Value @()

    $crossReportPath = Invoke-Migration -Source $crossSource -Target $crossTarget `
        -Local $crossLocal
    $crossReport = Get-Content -LiteralPath $crossReportPath -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "success" -Actual $crossReport.status -Message "跨目录迁移状态"
    Assert-Equal -Expected "newLayout" -Actual $crossReport.notesSourceKind `
        -Message "跨目录迁移优先选择新布局 notes"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossTarget `
                ".petaldesk\notes\$newNoteId\note.md") -PathType Leaf) `
        -Message "跨目录迁移新布局便签"
    Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $crossTarget `
                    ".petaldesk\notes\$ignoredLegacyId\note.md"))) `
        -Message "跨目录迁移未混入旧 notes"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossSource `
                "notes\$ignoredLegacyId\note.md") -PathType Leaf) -Message "跨目录源旧 notes 保留"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossSource `
                ".petaldesk\notes\$newNoteId\note.md") -PathType Leaf) -Message "跨目录源新 notes 保留"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossTarget `
                ".petaldesk\tools\timer.json") -PathType Leaf) -Message "timer.json 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossTarget `
                ".petaldesk\journal\draft.json") -PathType Leaf) -Message "journal 已迁移"
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $crossTarget `
                ".petaldesk\state\windows.json") -PathType Leaf) -Message "跨目录旧 windows 已迁移"
    $crossConfig = Get-Content -LiteralPath (Join-Path $crossTarget ".petaldesk\config.json") `
        -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-Equal -Expected "typora" -Actual $crossConfig.defaultEditorMode `
        -Message "跨目录优先复制新布局 config.json"
    Assert-StorageStructure -Root $crossTarget
    Assert-Pointer -Path (Join-Path $crossLocal "storage-path.txt") -ExpectedRoot $crossTarget
    Assert-True -Condition (Test-Path -LiteralPath $crossReport.archivedLegacyLocalAppDataPath `
            -PathType Container) -Message "跨目录迁移归档旧 LocalAppData"
    Assert-Equal -Expected 4 -Actual ([int]$crossReport.archivedFileCount) `
        -Message "跨目录归档四个旧业务文件"
    foreach ($name in @("settings.json", "windows.json", "reminders.json", "gantt.json")) {
        Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $crossLocal $name))) `
            -Message "跨目录迁移后 LocalAppData 不再保留 $name"
        Assert-True -Condition (Test-Path -LiteralPath (Join-Path `
                    $crossReport.archivedLegacyLocalAppDataPath $name) -PathType Leaf) `
            -Message "跨目录迁移备份包含 $name"
    }

    # 场景六：目标内容不同时必须报告冲突，不覆盖文件，也不更新路径指针。
    $conflictSource = Join-Path $sandbox "conflict\source"
    $conflictTarget = Join-Path $sandbox "conflict\target"
    $conflictLocal = Join-Path $sandbox "conflict\local\PetalDesk"
    $conflictNoteId = "44444444-4444-4444-8444-444444444444"
    $conflictSourceNote = Join-Path $conflictSource "notes\$conflictNoteId\note.md"
    $conflictTargetNote = Join-Path $conflictTarget ".petaldesk\notes\$conflictNoteId\note.md"
    Write-Utf8Text -Path $conflictSourceNote -Value "源内容"
    Write-Utf8Text -Path $conflictTargetNote -Value "目标已有不同内容"
    $threw = $false
    try {
        & $migrationScript -SourceRoot $conflictSource -TargetRoot $conflictTarget `
            -LocalAppDataRoot $conflictLocal | Out-Null
    }
    catch {
        $threw = $true
    }
    Assert-True -Condition $threw -Message "冲突迁移以失败状态结束"
    Assert-Equal -Expected "目标已有不同内容" `
        -Actual ([System.IO.File]::ReadAllText($conflictTargetNote, [System.Text.Encoding]::UTF8)) `
        -Message "冲突目标文件未覆盖"
    Assert-True -Condition (Test-Path -LiteralPath $conflictSourceNote -PathType Leaf) `
        -Message "冲突源文件保留"
    Assert-True -Condition (-not (Test-Path -LiteralPath (Join-Path $conflictLocal `
                    "storage-path.txt"))) -Message "冲突时未更新路径指针"
    $conflictReports = @(Get-ChildItem -LiteralPath (Join-Path $conflictTarget `
                ".petaldesk\backups") -Filter "migration-report.json" -File -Recurse)
    Assert-Equal -Expected 1 -Actual $conflictReports.Count -Message "冲突迁移生成一份报告"
    $conflictReport = Get-Content -LiteralPath $conflictReports[0].FullName -Raw -Encoding UTF8 |
        ConvertFrom-Json
    Assert-Equal -Expected "conflict" -Actual $conflictReport.status -Message "冲突报告状态"
    Assert-Equal -Expected 1 -Actual ([int]$conflictReport.conflictCount) `
        -Message "冲突报告数量"

    # 场景七：目标 notes 内的任务目录是 junction 时必须拒绝越界写入。
    $junctionRoot = Join-Path $sandbox "junction\source"
    $junctionTarget = Join-Path $sandbox "junction\target"
    $junctionOutside = Join-Path $sandbox "junction\outside"
    $junctionLocal = Join-Path $sandbox "junction\local\PetalDesk"
    $junctionNoteId = "77777777-7777-4777-8777-777777777777"
    Write-Utf8Text -Path (Join-Path $junctionRoot "notes\$junctionNoteId\note.md") `
        -Value "不得写入 junction"
    [System.IO.Directory]::CreateDirectory((Join-Path $junctionTarget ".petaldesk\notes")) | Out-Null
    [System.IO.Directory]::CreateDirectory($junctionOutside) | Out-Null
    Write-Utf8Text -Path (Join-Path $junctionOutside "marker.txt") -Value "不得写入此目录"
    $junctionPath = Join-Path $junctionTarget ".petaldesk\notes\$junctionNoteId"
    New-Item -ItemType Junction -Path $junctionPath -Target $junctionOutside | Out-Null
    $junctionThrew = $false
    try {
        & $migrationScript -SourceRoot $junctionRoot -TargetRoot $junctionTarget `
            -LocalAppDataRoot $junctionLocal | Out-Null
    }
    catch {
        $junctionThrew = $true
    }
    Assert-True -Condition $junctionThrew -Message "目标 notes 子目录 junction 被拒绝"
    Assert-Equal -Expected 1 -Actual @(Get-ChildItem -LiteralPath $junctionOutside -File).Count `
        -Message "junction 外部目录未写入新文件"
    Assert-Equal -Expected "不得写入此目录" `
        -Actual ([System.IO.File]::ReadAllText((Join-Path $junctionOutside "marker.txt"), `
                [System.Text.Encoding]::UTF8)) -Message "junction 外部文件未修改"
    [System.IO.Directory]::Delete($junctionPath)

    # 场景八：LocalAppDataRoot 与数据根重叠时必须拒绝。
    $overlapRoot = Join-Path $sandbox "overlap\source"
    Write-Utf8Text -Path (Join-Path $overlapRoot "notes\note\note.md") -Value "overlap"
    $overlapLocal = Join-Path $overlapRoot ".petaldesk\backups\local-app-data"
    $overlapThrew = $false
    try {
        & $migrationScript -SourceRoot $overlapRoot -LocalAppDataRoot $overlapLocal | Out-Null
    }
    catch {
        $overlapThrew = $true
    }
    Assert-True -Condition $overlapThrew -Message "LocalAppDataRoot 与数据根重叠被拒绝"
    Assert-True -Condition (-not (Test-Path -LiteralPath $overlapLocal)) `
        -Message "重叠路径校验失败时未创建 LocalAppDataRoot"

    Write-Host "迁移脚本演练通过：旧布局、旧内部目录、LocalAppData 收尾、双 notes 幂等、跨目录、冲突及 junction 防护。" `
        -ForegroundColor Green
    Write-Host "临时测试目录：$sandbox"
}
finally {
    if (-not $KeepArtifacts -and (Test-Path -LiteralPath $sandbox)) {
        Remove-Item -LiteralPath $sandbox -Recurse -Force
    }
}

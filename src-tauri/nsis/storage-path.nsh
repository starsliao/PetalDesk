; PetalDesk data-root selection for the Tauri NSIS installer.
; The pointer is intentionally UTF-16LE with BOM: Unicode NSIS can write it
; without lossy path conversion, and Rust can detect FF FE and decode u16 LE.

!include nsDialogs.nsh

; NSIS language IDs are available only after MUI_LANGUAGE is inserted, while
; Tauri includes installer hooks near the top of installer.nsi.
!define PETALDESK_LANG_SIMPCHINESE 2052

; Build probes override this directory so they exercise the same resolver and
; persistence functions without reading or modifying the user's real pointer.
!ifndef PETALDESK_STORAGE_POINTER_DIR
!define PETALDESK_STORAGE_POINTER_DIR "$LOCALAPPDATA\PetalDesk"
!endif

Var PetalDeskStoragePath
Var PetalDeskStoragePathInput
Var PetalDeskStorageBrowseButton
Var PetalDeskStoragePathError

; Resolve the pre-unified-storage workspace without putting the path itself on
; a command line. Windows PowerShell reads the fixed legacy settings file,
; parses JSON (including escapes and Unicode), and writes a UTF-16LE result in
; $PLUGINSDIR. Any failure leaves $PetalDeskStoragePath unchanged so callers can
; safely keep the Documents\PetalDesk default.
Function PetalDeskResolveLegacyWorkspacePath
  InitPluginsDir
  StrCpy $0 "$PLUGINSDIR\petaldesk-resolve-legacy-workspace.ps1"
  StrCpy $1 "$PLUGINSDIR\petaldesk-legacy-workspace-path.txt"
  Delete "$0"
  Delete "$1"

  ClearErrors
  FileOpen $2 "$0" w
  ${If} ${Errors}
    Goto petaldesk_legacy_workspace_cleanup
  ${EndIf}

  FileWriteUTF16LE /BOM $2 '$$ErrorActionPreference = "Stop"$\r$\n'
  FileWriteUTF16LE $2 'try {$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyName = "Fei" + "Hua"$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyDisplayName = [string][char]0x98DE + [char]0x82B1$\r$\n'
  FileWriteUTF16LE $2 '  $$documents = [Environment]::GetFolderPath("MyDocuments")$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyDefault = Join-Path $$documents $$legacyDisplayName$\r$\n'
  FileWriteUTF16LE $2 '  $$currentDefault = Join-Path $$documents "PetalDesk"$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyRoot = Join-Path $$env:LOCALAPPDATA $$legacyName$\r$\n'
  FileWriteUTF16LE $2 '  $$pointerPath = Join-Path $$legacyRoot "storage-path.txt"$\r$\n'
  FileWriteUTF16LE $2 '  $$settingsPath = Join-Path $$legacyRoot "settings.json"$\r$\n'
  FileWriteUTF16LE $2 '  $$outputPath = Join-Path $$PSScriptRoot "petaldesk-legacy-workspace-path.txt"$\r$\n'
  FileWriteUTF16LE $2 '  $$candidate = $$null$\r$\n'
  FileWriteUTF16LE $2 '  if ([IO.File]::Exists($$pointerPath)) {$\r$\n'
  FileWriteUTF16LE $2 '    $$bytes = [IO.File]::ReadAllBytes($$pointerPath)$\r$\n'
  FileWriteUTF16LE $2 '    if ($$bytes.Length -ge 2 -and $$bytes[0] -eq 0xFF -and $$bytes[1] -eq 0xFE) {$\r$\n'
  FileWriteUTF16LE $2 '      $$candidate = [Text.Encoding]::Unicode.GetString($$bytes, 2, $$bytes.Length - 2)$\r$\n'
  FileWriteUTF16LE $2 '    } else { $$candidate = [Text.Encoding]::UTF8.GetString($$bytes) }$\r$\n'
  FileWriteUTF16LE $2 '  } elseif ([IO.File]::Exists($$settingsPath)) {$\r$\n'
  FileWriteUTF16LE $2 '    $$settings = [IO.File]::ReadAllText($$settingsPath) | ConvertFrom-Json -ErrorAction Stop$\r$\n'
  FileWriteUTF16LE $2 '    $$candidate = $$settings.workspacePath$\r$\n'
  FileWriteUTF16LE $2 '  } else {$\r$\n'
  FileWriteUTF16LE $2 '    $$candidate = $$legacyDefault$\r$\n'
  FileWriteUTF16LE $2 '  }$\r$\n'
  FileWriteUTF16LE $2 '  if ($$null -eq $$candidate -or -not ($$candidate -is [string])) { exit 11 }$\r$\n'
  FileWriteUTF16LE $2 '  $$candidate = $$candidate.Trim().Trim([char]0xFEFF)$\r$\n'
  FileWriteUTF16LE $2 '  if ([string]::IsNullOrWhiteSpace($$candidate)) { exit 12 }$\r$\n'
  ; Rust canonical paths from older builds may use \\?\C:\ or
  ; \\?\UNC\server\share. Normalize those before NSIS displays or creates them.
  FileWriteUTF16LE $2 '  if ($$candidate.StartsWith("\\?\UNC\", [StringComparison]::OrdinalIgnoreCase)) {$\r$\n'
  FileWriteUTF16LE $2 '    $$candidate = "\\" + $$candidate.Substring(8)$\r$\n'
  FileWriteUTF16LE $2 '  } elseif ($$candidate.StartsWith("\\?\", [StringComparison]::OrdinalIgnoreCase)) {$\r$\n'
  FileWriteUTF16LE $2 '    $$localPath = $$candidate.Substring(4)$\r$\n'
  FileWriteUTF16LE $2 '    if ($$localPath.Length -lt 3 -or -not [char]::IsLetter($$localPath[0]) -or $$localPath[1] -ne ":" -or ($$localPath[2] -ne "\" -and $$localPath[2] -ne "/")) { exit 13 }$\r$\n'
  FileWriteUTF16LE $2 '    $$candidate = $$localPath$\r$\n'
  FileWriteUTF16LE $2 '  } elseif ($$candidate.StartsWith("\\.\", [StringComparison]::OrdinalIgnoreCase)) {$\r$\n'
  FileWriteUTF16LE $2 '    exit 13$\r$\n'
  FileWriteUTF16LE $2 '  }$\r$\n'
  FileWriteUTF16LE $2 '  if (-not [IO.Path]::IsPathRooted($$candidate)) { exit 13 }$\r$\n'
  FileWriteUTF16LE $2 '  if ($$candidate.IndexOfAny([IO.Path]::GetInvalidPathChars()) -ge 0) { exit 14 }$\r$\n'
  FileWriteUTF16LE $2 '  $$fullPath = [IO.Path]::GetFullPath($$candidate)$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyDefaultPath = [IO.Path]::GetFullPath($$legacyDefault)$\r$\n'
  FileWriteUTF16LE $2 '  $$comparisonPath = $$fullPath.TrimEnd([char]92, [char]47)$\r$\n'
  FileWriteUTF16LE $2 '  $$legacyComparisonPath = $$legacyDefaultPath.TrimEnd([char]92, [char]47)$\r$\n'
  FileWriteUTF16LE $2 '  if ([string]::Equals($$comparisonPath, $$legacyComparisonPath, [StringComparison]::OrdinalIgnoreCase)) {$\r$\n'
  FileWriteUTF16LE $2 '    $$fullPath = [IO.Path]::GetFullPath($$currentDefault)$\r$\n'
  FileWriteUTF16LE $2 '  } elseif (-not [IO.Directory]::Exists($$fullPath)) { exit 15 }$\r$\n'
  FileWriteUTF16LE $2 '  $$encoding = New-Object Text.UnicodeEncoding($$false, $$true)$\r$\n'
  FileWriteUTF16LE $2 '  [IO.File]::WriteAllText($$outputPath, $$fullPath, $$encoding)$\r$\n'
  FileWriteUTF16LE $2 '  exit 0$\r$\n'
  FileWriteUTF16LE $2 '} catch {$\r$\n'
  FileWriteUTF16LE $2 '  exit 20$\r$\n'
  FileWriteUTF16LE $2 '}$\r$\n'
  FileClose $2

  ClearErrors
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "$0"'
  Pop $2
  Pop $3
  ${If} ${Errors}
  ${OrIf} $2 != 0
    Goto petaldesk_legacy_workspace_cleanup
  ${EndIf}

  ClearErrors
  FileOpen $2 "$1" r
  ${If} ${Errors}
    Goto petaldesk_legacy_workspace_cleanup
  ${EndIf}
  ClearErrors
  FileReadUTF16LE $2 $3
  FileClose $2
  ${If} ${Errors}
  ${OrIf} $3 == ""
    Goto petaldesk_legacy_workspace_cleanup
  ${EndIf}

  ; Repeat a root check on the NSIS side before trusting the helper. Do not use
  ; GetFullPathName here: the mapped Documents\PetalDesk default may not exist
  ; until the storage page validates and creates it.
  ClearErrors
  ${GetRoot} "$3" $4
  ${IfNot} ${Errors}
  ${AndIf} $4 != ""
    StrCpy $PetalDeskStoragePath $3
  ${EndIf}

  petaldesk_legacy_workspace_cleanup:
  Delete "$0"
  Delete "$1"
FunctionEnd

; Strip Windows' internal extended-length namespace before a path reaches an
; installer control, log line, or persisted pointer. Device namespace paths
; are not valid PetalDesk data roots and remain rejected by the caller.
Function PetalDeskNormalizeStoragePath
  StrCpy $PetalDeskStoragePathError ""
  StrCpy $0 "$PetalDeskStoragePath" 8
  ${If} $0 == "\\?\UNC\"
    StrCpy $0 "$PetalDeskStoragePath" "" 8
    StrCpy $PetalDeskStoragePath "\\$0"
    Return
  ${EndIf}

  StrCpy $0 "$PetalDeskStoragePath" 4
  ${If} $0 == "\\?\"
    StrCpy $1 "$PetalDeskStoragePath" "" 4
    StrCpy $0 "$1" 1 1
    StrCpy $2 "$1" 1 2
    ${If} $0 != ":"
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
    ${If} $2 != "\"
    ${AndIf} $2 != "/"
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
    StrCpy $PetalDeskStoragePath "$1"
  ${ElseIf} $0 == "\\.\"
    StrCpy $PetalDeskStoragePathError "invalid"
  ${EndIf}
FunctionEnd

Function PetalDeskResolveStoragePath
  ${If} $PetalDeskStoragePath != ""
    Call PetalDeskNormalizeStoragePath
    Return
  ${EndIf}

  StrCpy $PetalDeskStoragePath "$DOCUMENTS\PetalDesk"

  ; A new unified-storage pointer always wins. Only installations that do not
  ; have one may inherit the workspacePath from the legacy settings file.
  IfFileExists "${PETALDESK_STORAGE_POINTER_DIR}\storage-path.txt" 0 petaldesk_try_legacy_workspace

  ClearErrors
  FileOpen $0 "${PETALDESK_STORAGE_POINTER_DIR}\storage-path.txt" r
  ${If} ${Errors}
    Goto petaldesk_storage_path_resolved
  ${EndIf}

  ClearErrors
  FileReadUTF16LE $0 $1
  FileClose $0
  ${IfNot} ${Errors}
  ${AndIf} $1 != ""
    StrCpy $PetalDeskStoragePath $1
  ${EndIf}
  Goto petaldesk_storage_path_resolved

  petaldesk_try_legacy_workspace:
  Call PetalDeskResolveLegacyWorkspacePath

  petaldesk_storage_path_resolved:
  Call PetalDeskNormalizeStoragePath
FunctionEnd

Function PetalDeskStoragePageCreate
  ${If} ${Silent}
    Abort
  ${EndIf}

  ; Tauri's /P mode is passive: keep all custom pages non-interactive too.
  ClearErrors
  ${GetOptions} $CMDLINE "/P" $0
  ${IfNot} ${Errors}
    Abort
  ${EndIf}

  Call PetalDeskResolveStoragePath

  ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
    !insertmacro MUI_HEADER_TEXT "飞花 - PetalDesk 数据存储" "选择便签、附件和备份的存储根目录"
    StrCpy $1 "飞花 - PetalDesk 会把便签、附件、备份和恢复数据保存到这个目录。"
    StrCpy $2 "浏览..."
    StrCpy $3 "以后仍可在应用中切换目录；安装器只记录本次选择，不会移动已有数据。"
  ${Else}
    !insertmacro MUI_HEADER_TEXT "飞花 - PetalDesk data storage" "Choose the root folder for notes, attachments, and backups"
    StrCpy $1 "飞花 - PetalDesk stores notes, attachments, backups, and recovery data in this folder."
    StrCpy $2 "Browse..."
    StrCpy $3 "You can change it later in the app. The installer records this choice but does not move existing data."
  ${EndIf}

  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}

  ${IfThen} $(^RTL) = 1 ${|} nsDialogs::SetRTL $(^RTL) ${|}

  ${NSD_CreateLabel} 0 0 100% 24u "$1"
  Pop $0

  ${NSD_CreateDirRequest} 0 32u 78% 13u "$PetalDeskStoragePath"
  Pop $PetalDeskStoragePathInput

  ${NSD_CreateBrowseButton} 80% 32u 20% 14u "$2"
  Pop $PetalDeskStorageBrowseButton
  ${NSD_OnClick} $PetalDeskStorageBrowseButton PetalDeskStorageBrowse

  ${NSD_CreateLabel} 0 58u 100% 28u "$3"
  Pop $0

  nsDialogs::Show
FunctionEnd

Function PetalDeskStorageBrowse
  ; nsDialogs pushes the control handle for click callbacks.
  Pop $0
  ${NSD_GetText} $PetalDeskStoragePathInput $1

  ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
    StrCpy $2 "选择飞花 - PetalDesk 数据存储根目录"
  ${Else}
    StrCpy $2 "Choose the data storage folder for 飞花 - PetalDesk"
  ${EndIf}

  nsDialogs::SelectFolderDialog "$2" "$1"
  Pop $3
  ${If} $3 != error
  ${AndIf} $3 != ""
    ${NSD_SetText} $PetalDeskStoragePathInput "$3"
  ${EndIf}
FunctionEnd

; NSIS GetFullPathName reports an error when the final directory does not yet
; exist. Create the user-selected root first, then normalize it. This also
; verifies that the installer can actually create the directory before the
; user proceeds.
Function PetalDeskPrepareStoragePath
  ; Normalize supported extended-length paths, then accept only a drive-rooted
  ; path (C:\...) or a normal UNC share (\\server\share\...). Root-relative,
  ; drive-relative, and device namespace paths are unsafe for a portable data
  ; root and must not be resolved against the installer's current directory.
  Call PetalDeskNormalizeStoragePath
  ${If} $PetalDeskStoragePathError != ""
    Return
  ${EndIf}

  StrCpy $0 "$PetalDeskStoragePath" 2
  ${If} $0 == "\\"
    StrCpy $0 "$PetalDeskStoragePath" 1 2
    ${If} $0 == "?"
    ${OrIf} $0 == "."
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
    ${GetRoot} "$PetalDeskStoragePath" $0
    ${If} $0 == ""
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
  ${Else}
    StrCpy $0 "$PetalDeskStoragePath" 1 1
    StrCpy $1 "$PetalDeskStoragePath" 1 2
    ${If} $0 != ":"
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
    ${If} $1 != "\"
    ${AndIf} $1 != "/"
      StrCpy $PetalDeskStoragePathError "invalid"
      Return
    ${EndIf}
  ${EndIf}

  ${If} $PetalDeskStoragePath == ""
    StrCpy $PetalDeskStoragePathError "invalid"
    Return
  ${EndIf}

  ClearErrors
  CreateDirectory "$PetalDeskStoragePath"
  ${If} ${Errors}
    StrCpy $PetalDeskStoragePathError "create"
    Return
  ${EndIf}

  ClearErrors
  GetFullPathName $0 "$PetalDeskStoragePath"
  ${If} ${Errors}
  ${OrIf} $0 == ""
    StrCpy $PetalDeskStoragePathError "invalid"
    Return
  ${EndIf}

  StrCpy $PetalDeskStoragePath $0

  ; Existing directories can make CreateDirectory succeed even when the user
  ; cannot write there. Exercise the same create/write/rename/delete operations
  ; required by PetalDesk's atomic storage before accepting the selection.
  System::Call 'kernel32::GetCurrentProcessId() i.r1'
  System::Call 'kernel32::GetTickCount() i.r2'
  StrCpy $3 "$PetalDeskStoragePath\.petaldesk-write-test-$1-$2.tmp"
  StrCpy $4 "$PetalDeskStoragePath\.petaldesk-write-test-$1-$2.moved"

  ClearErrors
  FileOpen $0 "$3" w
  ${If} ${Errors}
    StrCpy $PetalDeskStoragePathError "write"
    Return
  ${EndIf}
  ClearErrors
  FileWrite $0 "PetalDesk storage write test"
  ${If} ${Errors}
    FileClose $0
    Delete "$3"
    StrCpy $PetalDeskStoragePathError "write"
    Return
  ${EndIf}
  FileClose $0

  ClearErrors
  Rename "$3" "$4"
  ${If} ${Errors}
    Delete "$3"
    StrCpy $PetalDeskStoragePathError "write"
    Return
  ${EndIf}
  ClearErrors
  Delete "$4"
  ${If} ${Errors}
    StrCpy $PetalDeskStoragePathError "write"
    Return
  ${EndIf}
FunctionEnd

Function PetalDeskStoragePageLeave
  ${NSD_GetText} $PetalDeskStoragePathInput $PetalDeskStoragePath

  ${If} $PetalDeskStoragePath == ""
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      MessageBox MB_ICONEXCLAMATION|MB_OK "请选择飞花 - PetalDesk 数据存储根目录。"
    ${Else}
      MessageBox MB_ICONEXCLAMATION|MB_OK "Choose the data storage folder for 飞花 - PetalDesk."
    ${EndIf}
    Abort
  ${EndIf}

  Call PetalDeskPrepareStoragePath
  ${If} $PetalDeskStoragePathError == "create"
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      MessageBox MB_ICONEXCLAMATION|MB_OK "无法创建飞花 - PetalDesk 数据存储目录，请检查父目录是否存在以及当前用户是否有写入权限。"
    ${Else}
      MessageBox MB_ICONEXCLAMATION|MB_OK "Could not create the data storage folder for 飞花 - PetalDesk. Check that its parent exists and that you have write permission."
    ${EndIf}
    Abort
  ${ElseIf} $PetalDeskStoragePathError == "write"
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      MessageBox MB_ICONEXCLAMATION|MB_OK "飞花 - PetalDesk 数据存储目录不可写，请选择当前用户有写入权限的目录。"
    ${Else}
      MessageBox MB_ICONEXCLAMATION|MB_OK "The data storage folder for 飞花 - PetalDesk is not writable. Choose a folder where the current user has write permission."
    ${EndIf}
    Abort
  ${ElseIf} $PetalDeskStoragePathError != ""
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      MessageBox MB_ICONEXCLAMATION|MB_OK "数据存储路径无效，请输入完整路径或重新选择。"
    ${Else}
      MessageBox MB_ICONEXCLAMATION|MB_OK "The data storage path is invalid. Enter a full path or choose another folder."
    ${EndIf}
    Abort
  ${EndIf}
FunctionEnd

Function PetalDeskPersistStoragePath
  Call PetalDeskResolveStoragePath
  Call PetalDeskNormalizeStoragePath
  ${If} $PetalDeskStoragePathError != ""
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      Abort "飞花 - PetalDesk 数据存储路径无效。"
    ${Else}
      Abort "The data storage path for 飞花 - PetalDesk is invalid."
    ${EndIf}
  ${EndIf}

  ClearErrors
  CreateDirectory "$PetalDeskStoragePath"
  ${If} ${Errors}
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      Abort "无法创建飞花 - PetalDesk 数据存储目录：$PetalDeskStoragePath"
    ${Else}
      Abort "Could not create the data storage folder for 飞花 - PetalDesk: $PetalDeskStoragePath"
    ${EndIf}
  ${EndIf}

  ClearErrors
  CreateDirectory "${PETALDESK_STORAGE_POINTER_DIR}"
  ${If} ${Errors}
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      Abort "无法创建飞花 - PetalDesk 本地配置目录。"
    ${Else}
      Abort "Could not create the local configuration folder for 飞花 - PetalDesk."
    ${EndIf}
  ${EndIf}

  ClearErrors
  FileOpen $0 "${PETALDESK_STORAGE_POINTER_DIR}\storage-path.txt" w
  ${If} ${Errors}
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      Abort "无法写入飞花 - PetalDesk 数据存储路径。"
    ${Else}
      Abort "Could not write the data storage path for 飞花 - PetalDesk."
    ${EndIf}
  ${EndIf}

  ClearErrors
  FileWriteUTF16LE /BOM $0 "$PetalDeskStoragePath"
  ${If} ${Errors}
    FileClose $0
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      Abort "无法写入飞花 - PetalDesk 数据存储路径。"
    ${Else}
      Abort "Could not write the data storage path for 飞花 - PetalDesk."
    ${EndIf}
  ${EndIf}
  FileClose $0

  ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
    DetailPrint "飞花 - PetalDesk 数据存储：$PetalDeskStoragePath"
  ${Else}
    DetailPrint "Data storage for 飞花 - PetalDesk: $PetalDeskStoragePath"
  ${EndIf}
FunctionEnd

Function PetalDeskShowNativeMessagingWarning
  IfSilent petaldesk_native_warning_done 0
  ClearErrors
  ${GetOptions} $CMDLINE "/P" $4
  ${If} ${Errors}
    MessageBox MB_ICONEXCLAMATION|MB_OK "$3"
  ${EndIf}
  petaldesk_native_warning_done:
FunctionEnd

; Register the browser bridge after Tauri has copied the host and helper script.
; Firefox uses a stable Gecko ID. Chromium registrations are compiled in only
; when the release build provides the corresponding store extension ID.
Function PetalDeskRegisterNativeMessagingHost
  IfFileExists "$INSTDIR\petaldesk-browser-host.exe" 0 petaldesk_native_host_missing
  IfFileExists "$INSTDIR\native-messaging\Register-PetalDeskNativeHost.ps1" 0 petaldesk_native_host_missing

  StrCpy $0 '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\native-messaging\Register-PetalDeskNativeHost.ps1" -HostExecutable "$INSTDIR\petaldesk-browser-host.exe"'
  !ifdef PETALDESK_CHROME_EXTENSION_ID
    StrCpy $0 '$0 -ChromeExtensionId "${PETALDESK_CHROME_EXTENSION_ID}"'
  !endif
  !ifdef PETALDESK_EDGE_EXTENSION_ID
    StrCpy $0 '$0 -EdgeExtensionId "${PETALDESK_EDGE_EXTENSION_ID}"'
  !endif

  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" $0'
  Pop $1
  Pop $2
  ${If} $1 == 0
    DetailPrint "PetalDesk Native Messaging host registered."
  ${Else}
    ; Browser enhancement is optional; a registration failure must not roll
    ; back the desktop application installation.
    DetailPrint "PetalDesk Native Messaging registration skipped or failed (exit $1): $2"
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      StrCpy $3 "飞花 - PetalDesk 已安装，但 Firefox 浏览器增强注册失败。请重新运行安装器或查看安装日志。"
    ${Else}
      StrCpy $3 "PetalDesk was installed, but Firefox browser integration could not be registered. Run the installer again or review the installation log."
    ${EndIf}
    Call PetalDeskShowNativeMessagingWarning
  ${EndIf}
  Return

  petaldesk_native_host_missing:
    DetailPrint "PetalDesk Native Messaging host files are missing; browser enhancement was not registered."
    ${If} $LANGUAGE == ${PETALDESK_LANG_SIMPCHINESE}
      StrCpy $3 "飞花 - PetalDesk 已安装，但 Firefox 浏览器增强组件缺失。请重新运行安装器。"
    ${Else}
      StrCpy $3 "PetalDesk was installed, but the Firefox browser integration component is missing. Run the installer again."
    ${EndIf}
    Call PetalDeskShowNativeMessagingWarning
FunctionEnd

Function un.PetalDeskUnregisterNativeMessagingHost
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\com.petaldesk.capture"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\com.petaldesk.capture"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\com.petaldesk.capture"

  Delete "$LOCALAPPDATA\PetalDesk\NativeMessaging\com.petaldesk.capture.chrome.json"
  Delete "$LOCALAPPDATA\PetalDesk\NativeMessaging\com.petaldesk.capture.edge.json"
  Delete "$LOCALAPPDATA\PetalDesk\NativeMessaging\com.petaldesk.capture.chromium.json"
  Delete "$LOCALAPPDATA\PetalDesk\NativeMessaging\com.petaldesk.capture.firefox.json"
  RMDir "$LOCALAPPDATA\PetalDesk\NativeMessaging"
FunctionEnd

; Stop before file copying if Firefox still owns the long-lived Native Messaging
; host. This hook intentionally changes no storage or browser registration state.
!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckIfAppIsRunning "petaldesk-browser-host.exe" "PetalDesk browser integration"
!macroend

; Persist only after Tauri's running-app guard and application file/registry
; writes have completed. Keeping persistence out of PREINSTALL prevents a
; cancelled upgrade from switching the next startup's data root.
!macro NSIS_HOOK_POSTINSTALL
  Call PetalDeskPersistStoragePath
  Call PetalDeskRegisterNativeMessagingHost
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Tauri expands this hook before its own running-app guard. Repeat that guard
  ; here so cancelling the prompt cannot leave an installed app without browser
  ; registration.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PETALDESK_DISPLAYNAME}"
  ; Firefox keeps the Native Messaging port open. Use Tauri's normal process
  ; prompt so an interactive uninstall never kills an active capture silently.
  !insertmacro CheckIfAppIsRunning "petaldesk-browser-host.exe" "PetalDesk browser integration"
  Call un.PetalDeskUnregisterNativeMessagingHost
!macroend

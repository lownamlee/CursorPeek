Unicode true
ManifestSupportedOS all
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetDateSave off
CRCCheck on
AutoCloseWindow true
ShowInstDetails show
ShowUninstDetails show

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "Sections.nsh"

!ifndef PRODUCT_VERSION
  !error "PRODUCT_VERSION is required."
!endif
!ifndef PRODUCT_FILE_VERSION
  !error "PRODUCT_FILE_VERSION is required."
!endif
!ifndef PRODUCT_ICON
  !error "PRODUCT_ICON is required."
!endif
!ifndef OUTPUT_FILE
  !error "OUTPUT_FILE is required."
!endif
!ifndef INSTALL_FILES_INCLUDE
  !error "INSTALL_FILES_INCLUDE is required."
!endif
!ifndef UNINSTALL_FILES_INCLUDE
  !error "UNINSTALL_FILES_INCLUDE is required."
!endif
!ifndef ESTIMATED_SIZE_KIB
  !error "ESTIMATED_SIZE_KIB is required."
!endif

!define PRODUCT_NAME "CursorPeek"
!define PRODUCT_EXE "CursorPeek.exe"
!define PRODUCT_UNINSTALLER "Uninstall.exe"
!define PRODUCT_URL "https://github.com/lownamlee/CursorPeek"
!define PRODUCT_HELP_URL "https://github.com/lownamlee/CursorPeek/issues"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\CursorPeek"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
Caption "${PRODUCT_NAME} ${PRODUCT_VERSION} Setup"
UninstallCaption "Uninstall ${PRODUCT_NAME}"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\CursorPeek"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
Icon "${PRODUCT_ICON}"
UninstallIcon "${PRODUCT_ICON}"
BrandingText "${PRODUCT_NAME}"

VIProductVersion "${PRODUCT_FILE_VERSION}"
VIAddVersionKey /LANG=1033 "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=1033 "FileDescription" "${PRODUCT_NAME} per-user installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=1033 "OriginalFilename" "CursorPeek-${PRODUCT_VERSION}-windows-x64-setup.exe"
VIAddVersionKey /LANG=1033 "LegalCopyright" "CursorPeek contributors"

!define MUI_ICON "${PRODUCT_ICON}"
!define MUI_UNICON "${PRODUCT_ICON}"
!define MUI_ABORTWARNING
!define MUI_UNABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_COMPONENTS
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Var ExistingInstall
Var ExistingUninstaller
Var Parameters

Section "!CursorPeek application" SecCore
  SectionIn RO
  SetShellVarContext current
  SetRegView 64

  ${If} $ExistingInstall != ""
    StrCpy $ExistingUninstaller "$ExistingInstall\${PRODUCT_UNINSTALLER}"
    IfFileExists "$ExistingUninstaller" 0 upgrade_complete
      ExecWait \
        '"$ExistingUninstaller" /S /KEEPSETTINGS _?=$ExistingInstall' \
        $0
      ${If} $0 != 0
        MessageBox MB_OK|MB_ICONSTOP \
          "The existing CursorPeek installation could not be removed safely." \
          /SD IDOK
        Abort
      ${EndIf}
      Delete "$ExistingUninstaller"
      RMDir "$ExistingInstall"
  ${EndIf}

upgrade_complete:
  !include "${INSTALL_FILES_INCLUDE}"
  WriteUninstaller "$INSTDIR\${PRODUCT_UNINSTALLER}"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${PRODUCT_EXE},0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "CursorPeek project"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "${PRODUCT_URL}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "HelpLink" "${PRODUCT_HELP_URL}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" \
    '"$INSTDIR\${PRODUCT_UNINSTALLER}"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" \
    '"$INSTDIR\${PRODUCT_UNINSTALLER}" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" ${ESTIMATED_SIZE_KIB}
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Start Menu shortcuts" SecStartMenu
  SetShellVarContext current
  CreateDirectory "$SMPROGRAMS\CursorPeek"
  CreateShortcut "$SMPROGRAMS\CursorPeek\CursorPeek.lnk" \
    "$INSTDIR\${PRODUCT_EXE}" "" "$INSTDIR\${PRODUCT_EXE}" 0
  CreateShortcut "$SMPROGRAMS\CursorPeek\Uninstall CursorPeek.lnk" \
    "$INSTDIR\${PRODUCT_UNINSTALLER}"
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  SetShellVarContext current
  CreateShortcut "$DESKTOP\CursorPeek.lnk" \
    "$INSTDIR\${PRODUCT_EXE}" "" "$INSTDIR\${PRODUCT_EXE}" 0
SectionEnd

Section /o "Start with Windows" SecStartup
SectionEnd

Section -Finalize
  SectionGetFlags ${SecStartup} $0
  IntOp $0 $0 & ${SF_SELECTED}
  ${If} $0 == ${SF_SELECTED}
    ExecWait '"$INSTDIR\${PRODUCT_EXE}" --set-startup-enabled' $1
  ${Else}
    ExecWait '"$INSTDIR\${PRODUCT_EXE}" --set-startup-disabled' $1
  ${EndIf}
  ${If} $1 != 0
    MessageBox MB_OK|MB_ICONSTOP \
      "CursorPeek could not save its startup setting." /SD IDOK
    Abort
  ${EndIf}

  SectionGetFlags ${SecStartMenu} $0
  IntOp $0 $0 & ${SF_SELECTED}
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "StartMenuShortcut" $0
  SectionGetFlags ${SecDesktop} $0
  IntOp $0 $0 & ${SF_SELECTED}
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "DesktopShortcut" $0
  SectionGetFlags ${SecStartup} $0
  IntOp $0 $0 & ${SF_SELECTED}
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "StartWithWindows" $0
SectionEnd

Function ApplyComponentOverrides
  ${GetParameters} $Parameters

  ClearErrors
  ${GetOptions} $Parameters "/STARTMENU=" $0
  ${IfNot} ${Errors}
    ${If} $0 == "1"
      !insertmacro SelectSection ${SecStartMenu}
    ${ElseIf} $0 == "0"
      !insertmacro UnselectSection ${SecStartMenu}
    ${Else}
      MessageBox MB_OK|MB_ICONSTOP \
        "/STARTMENU must be 0 or 1." /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}

  ClearErrors
  ${GetOptions} $Parameters "/DESKTOP=" $0
  ${IfNot} ${Errors}
    ${If} $0 == "1"
      !insertmacro SelectSection ${SecDesktop}
    ${ElseIf} $0 == "0"
      !insertmacro UnselectSection ${SecDesktop}
    ${Else}
      MessageBox MB_OK|MB_ICONSTOP \
        "/DESKTOP must be 0 or 1." /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}

  ClearErrors
  ${GetOptions} $Parameters "/STARTUP=" $0
  ${IfNot} ${Errors}
    ${If} $0 == "1"
      !insertmacro SelectSection ${SecStartup}
    ${ElseIf} $0 == "0"
      !insertmacro UnselectSection ${SecStartup}
    ${Else}
      MessageBox MB_OK|MB_ICONSTOP \
        "/STARTUP must be 0 or 1." /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}
FunctionEnd

Function .onInit
  SetShellVarContext current
  SetRegView 64

  ReadRegStr $ExistingInstall HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $ExistingInstall != ""
    StrCpy $INSTDIR $ExistingInstall
  ${EndIf}

  ClearErrors
  ReadRegDWORD $0 HKCU "${UNINSTALL_KEY}" "StartMenuShortcut"
  ${If} ${Errors}
    !insertmacro SelectSection ${SecStartMenu}
  ${ElseIf} $0 == 0
    !insertmacro UnselectSection ${SecStartMenu}
  ${Else}
    !insertmacro SelectSection ${SecStartMenu}
  ${EndIf}

  ClearErrors
  ReadRegDWORD $0 HKCU "${UNINSTALL_KEY}" "DesktopShortcut"
  ${If} ${Errors}
    !insertmacro UnselectSection ${SecDesktop}
  ${ElseIf} $0 == 0
    !insertmacro UnselectSection ${SecDesktop}
  ${Else}
    !insertmacro SelectSection ${SecDesktop}
  ${EndIf}

  ClearErrors
  ReadRegDWORD $0 HKCU "${UNINSTALL_KEY}" "StartWithWindows"
  ${If} ${Errors}
    !insertmacro UnselectSection ${SecStartup}
  ${ElseIf} $0 == 0
    !insertmacro UnselectSection ${SecStartup}
  ${Else}
    !insertmacro SelectSection ${SecStartup}
  ${EndIf}

  Call ApplyComponentOverrides
FunctionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} \
    "Install the CursorPeek application, documentation, and license notices."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartMenu} \
    "Add CursorPeek and uninstall shortcuts to the current user's Start Menu."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} \
    "Add a CursorPeek shortcut to the current user's desktop."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartup} \
    "Start CursorPeek automatically when the current user signs in."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "!Remove CursorPeek" un.SecUninstall
  SectionIn RO
  SetShellVarContext current
  SetRegView 64

  IfFileExists "$INSTDIR\${PRODUCT_EXE}" 0 app_stopped
stop_retry:
    ExecWait '"$INSTDIR\${PRODUCT_EXE}" --shutdown-existing' $0
    ${If} $0 != 0
      MessageBox MB_RETRYCANCEL|MB_ICONSTOP \
        "CursorPeek is still running. Close it before uninstalling." \
        /SD IDCANCEL IDRETRY stop_retry
      Abort
    ${EndIf}
app_stopped:

  IfFileExists "$INSTDIR\${PRODUCT_EXE}" 0 startup_removed
    ExecWait '"$INSTDIR\${PRODUCT_EXE}" --set-startup-disabled' $0
startup_removed:
  ReadRegStr $0 HKCU "${RUN_KEY}" "CursorPeek"
  ${If} $0 == '"$INSTDIR\${PRODUCT_EXE}"'
    DeleteRegValue HKCU "${RUN_KEY}" "CursorPeek"
  ${EndIf}

  Delete "$SMPROGRAMS\CursorPeek\CursorPeek.lnk"
  Delete "$SMPROGRAMS\CursorPeek\Uninstall CursorPeek.lnk"
  RMDir "$SMPROGRAMS\CursorPeek"
  Delete "$DESKTOP\CursorPeek.lnk"

  !include "${UNINSTALL_FILES_INCLUDE}"
  Delete "$INSTDIR\${PRODUCT_UNINSTALLER}"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd

Section "Remove user settings" un.SecRemoveSettings
  Delete "$LOCALAPPDATA\CursorPeek\config.ini"
  RMDir "$LOCALAPPDATA\CursorPeek"
SectionEnd

Function un.onInit
  SetShellVarContext current
  SetRegView 64
  ${GetParameters} $Parameters
  ClearErrors
  ${GetOptions} $Parameters "/KEEPSETTINGS" $0
  ${IfNot} ${Errors}
    !insertmacro UnselectSection ${un.SecRemoveSettings}
  ${EndIf}
FunctionEnd

!insertmacro MUI_UNFUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${un.SecUninstall} \
    "Remove CursorPeek files, shortcuts, and startup registration."
  !insertmacro MUI_DESCRIPTION_TEXT ${un.SecRemoveSettings} \
    "Delete this user's CursorPeek configuration. Clear this option to keep settings."
!insertmacro MUI_UNFUNCTION_DESCRIPTION_END

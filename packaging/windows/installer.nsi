; Page2Cam installer (NSIS 3, 64-bit). Built by scripts/package-windows.sh:
;   makensis -DVERSION=x.y.z -DSRC=<staged files> -DOUT=<setup.exe> installer.nsi
; Silent install: Page2Cam-Setup-x.y.z.exe /S   (add /RELAUNCH to start Page2Cam afterwards;
; the in-app updater uses both).

Unicode true
Target amd64-unicode
SetCompressor /SOLID lzma
RequestExecutionLevel admin
ManifestDPIAware true

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif

!define APP "Page2Cam"
!define PUBLISHER "Prasad Vittaldev"
!define URL "https://github.com/prasadvittaldev/Page2Cam"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define CLSID "{7C2F9A41-3B6E-4D8A-9C1F-5E2B8D4A6F10}"

Name "${APP} ${VERSION}"
OutFile "${OUT}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
BrandingText "${APP} ${VERSION}"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"
VIAddVersionKey "FileDescription" "${APP} installer"

!define MUI_ABORTWARNING
!define MUI_ICON "${BRAND}\page2cam.ico"
!define MUI_UNICON "${BRAND}\page2cam.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${BRAND}\installer-welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "${BRAND}\installer-welcome.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "${BRAND}\installer-header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "${BRAND}\installer-header.bmp"
!define MUI_WELCOMEPAGE_TEXT "This will install ${APP} ${VERSION}.$\r$\n$\r$\n${APP} shows any website as a camera in Teams, Zoom or Google Meet, and can stream it to YouTube Live, with you on top of the page.$\r$\n$\r$\nPage2Cam waits in the tray until you click Start.$\r$\n$\r$\nPlease close ${APP} and any app that is using a camera before you continue."
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP} now"
!define MUI_FINISHPAGE_RUN_FUNCTION StartAppAsUser
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${SRC}\LICENSE.txt"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

LangString DESC_Install ${LANG_ENGLISH} "${APP} and its virtual camera. ${APP} waits in the tray until you click Start."
LangString DESC_Autostart ${LANG_ENGLISH} "Put ${APP} in the tray when you sign in to Windows. It stays stopped until you click Start. You can change this later in the tray menu."

; Start Page2Cam as the signed-in user, not as administrator (via Explorer).
Function StartAppAsUser
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\page2cam.exe"'
FunctionEnd

; Only ever install into (and so uninstall from) a folder named Page2Cam, so
; uninstalling can never delete a folder like C:\Program Files or D:\.
Function .onVerifyInstDir
  ${GetFileName} "$INSTDIR" $0
  StrCmp $0 "${APP}" +2
    Abort
FunctionEnd

; Let Page2Cam close by itself (the in-app updater asks it to), then make sure.
!macro StopEverything
  StrCpy $R9 0
  ${Do}
    nsExec::ExecToStack 'cmd /c tasklist /FI "IMAGENAME eq page2cam.exe" /NH | find /I "page2cam.exe"'
    Pop $R8
    Pop $R7
    ${If} $R8 != 0
      ${Break}
    ${EndIf}
    IntOp $R9 $R9 + 1
    ${If} $R9 > 30
      nsExec::Exec 'taskkill /IM page2cam.exe /T /F'
      Sleep 1500
      ${Break}
    ${EndIf}
    Sleep 500
  ${Loop}
  nsExec::Exec 'net stop FrameServerMonitor /y'
  nsExec::Exec 'net stop FrameServer /y'
!macroend

Section "!${APP}" SecInstall
  SectionIn RO
  SetShellVarContext all
  InitPluginsDir
  SetOutPath "$INSTDIR"
  !insertmacro StopEverything

  ; The camera service may still hold the old driver: move it aside and
  ; delete it at the next restart if it can't be deleted now.
  ${If} ${FileExists} "$INSTDIR\p2c_vcam_win_source.dll"
    ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\p2c_vcam_win_source.dll"'
    Delete "$INSTDIR\p2c_vcam_win_source.dll"
    ${If} ${FileExists} "$INSTDIR\p2c_vcam_win_source.dll"
      ; Still locked: move it aside under a unique name, delete at restart.
      System::Call 'kernel32::GetTickCount()i.r0'
      Rename "$INSTDIR\p2c_vcam_win_source.dll" "$INSTDIR\p2c_vcam_win_source.$0.old"
      Delete /REBOOTOK "$INSTDIR\p2c_vcam_win_source.$0.old"
    ${EndIf}
  ${EndIf}
  ; A settings file from an old portable copy here would override (and block
  ; saving) the user's settings in %APPDATA%.
  Delete "$INSTDIR\page2cam.toml"

  ; Files from an older version that are no longer shipped (models, locales).
  RMDir /r "$INSTDIR\models"
  RMDir /r "$INSTDIR\locales"
  File /r "${SRC}\*.*"

  ; Microsoft Visual C++ runtime (needed by the cut-out).
  SetOutPath "$PLUGINSDIR"
  File "${VCREDIST}"
  DetailPrint "Installing the Microsoft Visual C++ runtime..."
  ExecWait '"$PLUGINSDIR\vc_redist.x64.exe" /install /quiet /norestart'
  SetOutPath "$INSTDIR"

  ; Log folder for the camera driver, writable by the camera service.
  CreateDirectory "$APPDATA\${APP}"
  nsExec::Exec 'icacls "$APPDATA\${APP}" /grant *S-1-5-19:(OI)(CI)M *S-1-5-32-545:(OI)(CI)M'

  ; Register the virtual camera driver.
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\p2c_vcam_win_source.dll"' $0
  ${If} $0 != 0
    MessageBox MB_ICONEXCLAMATION|MB_OK "The Page2Cam camera could not be registered (code $0). Page2Cam will still run, but the camera may not appear in other apps. Restart your PC and run this installer again." /SD IDOK
  ${EndIf}

  ; Shortcuts.
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\page2cam.exe"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\page2cam.exe"

  ; Apps & features entry.
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\page2cam.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" "$0"

  ; Restart the camera service so the new driver is used.
  nsExec::Exec 'net start FrameServer'

  ; Silent update from inside Page2Cam: start it again when done.
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/RELAUNCH" $R1
  ${IfNot} ${Errors}
    Call StartAppAsUser
  ${EndIf}
SectionEnd

; Selected by default (and so on for silent installs). This installer runs
; elevated and can't reliably write the signed-in user's HKCU Run key, so it
; only records the choice; the tray applies it to the Run key on its first
; start, after which the Run key is the only source of truth.
Section "Start ${APP} (stopped) when I sign in" SecAutostart
  ${If} ${SectionIsSelected} ${SecAutostart}
    FileOpen $0 "$INSTDIR\autostart.default" w
    FileWrite $0 "1"
    FileClose $0
  ${Else}
    FileOpen $0 "$INSTDIR\autostart.default" w
    FileWrite $0 "0"
    FileClose $0
  ${EndIf}
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecInstall} $(DESC_Install)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} $(DESC_Autostart)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  SetShellVarContext all
  ; Refuse to touch a folder that isn't a Page2Cam install.
  ${IfNot} ${FileExists} "$INSTDIR\page2cam.exe"
    MessageBox MB_ICONSTOP|MB_OK "This folder doesn't look like a Page2Cam installation; nothing was removed." /SD IDOK
    Abort
  ${EndIf}
  !insertmacro StopEverything
  ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\p2c_vcam_win_source.dll"'
  DeleteRegKey HKLM "Software\Classes\CLSID\${CLSID}"
  Delete /REBOOTOK "$INSTDIR\p2c_vcam_win_source.dll"
  Delete /REBOOTOK "$INSTDIR\p2c_vcam_win_source.*.old"
  RMDir /r /REBOOTOK "$INSTDIR"
  RMDir /r "$APPDATA\${APP}"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKLM "${UNINST_KEY}"
  ; Sign-in start: only the Run value of the (elevated) uninstalling user can be
  ; reached from here, usually the same user. Other users' values are left; a
  ; Run value pointing at a missing exe does nothing.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APP}"
  nsExec::Exec 'net start FrameServer'
  ; Your settings (%APPDATA%\page2cam) are kept in case you reinstall.
SectionEnd

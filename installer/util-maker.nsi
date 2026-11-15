; Установщик прототипа Util Maker для Windows 11 x64.
;
; Устанавливается для текущего пользователя без административных прав:
;   %LOCALAPPDATA%\Util Maker
; Файлы передаются из docker/windows/build-windows.sh через -D...
;
;   APP_EXE          — util-maker-desktop.exe
;   MODULE_EXE       — control-module.exe
;   MODULE_MANIFEST  — manifest.json контрольного модуля
;   MODULE_ICON      — иконка модуля из assets
;   PACKAGE_DIR      — подписанные .umpkg и открытый ключ издателя
;   SCRIPTS_DIR      — measure-windows.ps1 и kill-plugin.ps1
;   README_FILE      — краткая инструкция
;   OUT_FILE         — путь к итоговому .exe
;   VERSION          — версия продукта
;   BUILD_KIND       — тип сборки (dev/release)

Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
    !define VERSION "0.4.30"
!endif
!ifndef BUILD_KIND
    !define BUILD_KIND "dev"
!endif
!ifndef APP_EXE
    !error "Не задан APP_EXE"
!endif
!ifndef MODULE_EXE
    !error "Не задан MODULE_EXE"
!endif
!ifndef MODULE_MANIFEST
    !error "Не задан MODULE_MANIFEST"
!endif
!ifndef MODULE_ICON
    !error "Не задан MODULE_ICON"
!endif
!ifndef PACKAGE_DIR
    !error "Не задан PACKAGE_DIR"
!endif
!ifndef SCRIPTS_DIR
    !error "Не задан SCRIPTS_DIR"
!endif
!ifndef README_FILE
    !error "Не задан README_FILE"
!endif
!ifndef OUT_FILE
    !error "Не задан OUT_FILE"
!endif

!define APP_NAME "Util Maker"
!define APP_ID "UtilMakerPrototype"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_ID}"

Name "${APP_NAME} ${VERSION} (${BUILD_KIND})"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Util Maker"
InstallDirRegKey HKCU "Software\Util Maker" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\util-maker-desktop.exe"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchUtilMaker
!define MUI_FINISHPAGE_RUN_TEXT "Запустить Util Maker"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Russian"

Function LaunchUtilMaker
    Exec '"$INSTDIR\util-maker-desktop.exe" --open-settings'
FunctionEnd

Section "Util Maker" SEC_MAIN
    SectionIn RO

    ; Предыдущая версия может работать и держать exe. Закрываем её без
    ; ручного удаления, затем при необходимости запускаем старый деинсталлятор.
    nsExec::ExecToStack '"$SYSDIR\taskkill.exe" /F /IM util-maker-desktop.exe'
    Pop $0
    Pop $1
    nsExec::ExecToStack '"$SYSDIR\taskkill.exe" /F /IM control-module.exe'
    Pop $0
    Pop $1
    Sleep 400

    IfSilent clean_data_done
    MessageBox MB_YESNO|MB_ICONEXCLAMATION "Чистая установка: удалить сочетания, настройки и установленные пакеты?$\r$\nБез этого настройки сохранятся." IDNO clean_data_done
    RMDir /r "$LOCALAPPDATA\org.utilmaker.prototype"
    RMDir /r "$LOCALAPPDATA\Util Maker\logs"
    clean_data_done:

    ; Если автозапуск был включён, восстановим его для нового пути.
    ReadRegStr $2 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UtilMaker"

    ReadRegStr $0 HKCU "Software\Util Maker" "InstallDir"
    StrCmp $0 "" previous_removed
    StrCmp $0 "$INSTDIR" previous_removed
    IfFileExists "$0\uninstall.exe" 0 previous_removed
    ExecWait '"$0\uninstall.exe" /S _?=$0' $1
    ; Старый деинсталлятор запускает удаление асинхронно; даём ему завершить
    ; удаление, иначе он снесёт только что установленный control-модуль.
    Sleep 2500
    Delete "$0\uninstall.exe"
    RMDir "$0"
previous_removed:

    SetOutPath "$INSTDIR"
    File "${APP_EXE}"
    File "/oname=README.txt" "${README_FILE}"

    SetOutPath "$INSTDIR\modules\org.utilmaker.control"
    File "${MODULE_EXE}"
    File "${MODULE_MANIFEST}"
    SetOutPath "$INSTDIR\modules\org.utilmaker.control\assets"
    File "${MODULE_ICON}"

    ; Пакеты прошлых версий не должны оставаться в каталоге поставки:
    ; иначе хост при старте пытается ставить устаревшие и битые архивы.
    RMDir /r "$INSTDIR\packages"
    SetOutPath "$INSTDIR\packages"
    File "${PACKAGE_DIR}\*.umpkg"
    File "${PACKAGE_DIR}\keys\org.utilmaker-public.json"

    SetOutPath "$INSTDIR\scripts"
    File "${SCRIPTS_DIR}\measure-windows.ps1"
    File "${SCRIPTS_DIR}\kill-plugin.ps1"

    SetOutPath "$INSTDIR"
    CreateShortCut "$DESKTOP\Util Maker.lnk" "$INSTDIR\util-maker-desktop.exe"
    CreateDirectory "$SMPROGRAMS\Util Maker"
    CreateShortCut "$SMPROGRAMS\Util Maker\Util Maker.lnk" "$INSTDIR\util-maker-desktop.exe"
    CreateShortCut "$SMPROGRAMS\Util Maker\Удалить Util Maker.lnk" "$INSTDIR\uninstall.exe"

    WriteUninstaller "$INSTDIR\uninstall.exe"

    WriteRegStr HKCU "Software\Util Maker" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME} (прототип)"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION} (${BUILD_KIND})"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\util-maker-desktop.exe"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Util Maker"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" 12000

    StrCmp $2 "" autostart_done
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UtilMaker" "$\"$INSTDIR\util-maker-desktop.exe$\""
autostart_done:
SectionEnd

Section "Uninstall"
    Delete "$DESKTOP\Util Maker.lnk"
    Delete "$SMPROGRAMS\Util Maker\Util Maker.lnk"
    Delete "$SMPROGRAMS\Util Maker\Удалить Util Maker.lnk"
    RMDir "$SMPROGRAMS\Util Maker"

    RMDir /r "$INSTDIR\modules"
    RMDir /r "$INSTDIR\packages"
    RMDir /r "$INSTDIR\scripts"
    RMDir /r "$INSTDIR\logs"
    Delete "$INSTDIR\util-maker-desktop.exe"
    Delete "$INSTDIR\README.txt"
    Delete "$INSTDIR\uninstall.exe"
    ; Сам деинсталлятор запущен из $INSTDIR и не может удалить свой файл.
    ; Передаём удаление каталога отдельному cmd после выхода процесса.
    SetOutPath "$TEMP"
    FileOpen $0 "$TEMP\util-maker-cleanup.bat" w
    FileWrite $0 "@echo off$\r$\n"
    FileWrite $0 "timeout /t 2 /nobreak >nul$\r$\n"
    FileWrite $0 "rmdir /s /q $\"$INSTDIR$\"$\r$\n"
    FileWrite $0 "del $\"%~f0$\"$\r$\n"
    FileClose $0
    Exec '"$TEMP\util-maker-cleanup.bat"' 
    RMDir "$INSTDIR"

    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UtilMaker"
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
    DeleteRegKey HKCU "Software\Util Maker"
    ; Пользовательские настройки и журнал остаются до ручного удаления:
    ; %LOCALAPPDATA%\org.utilmaker.prototype и %LOCALAPPDATA%\Util Maker\logs.
SectionEnd

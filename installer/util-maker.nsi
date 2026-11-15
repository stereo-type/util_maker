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
    !define VERSION "0.4.24"
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
Page custom CleanInstallPage CleanInstallLeave
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Russian"

Var CleanInstallCheck
Var CleanInstallDialog

Function CleanInstallPage
    !insertmacro MUI_HEADER_TEXT "Параметры установки" "Выберите, сохранять ли настройки прошлой версии."
    nsDialogs::Create 1018
    Pop $CleanInstallDialog
    ${If} $CleanInstallDialog == error
        Abort
    ${EndIf}
    ${NSD_CreateCheckBox} 0 0 100% 24u "Чистая установка: удалить сочетания, настройки плагинов и установленные пакеты"
    Pop $CleanInstallCheck
    ${NSD_SetState} $CleanInstallCheck ${BST_UNCHECKED}
    ${NSD_CreateLabel} 0 30u 100% 40u "Если галочка не стоит, профиль, сочетания и настройки сохранятся. Журнал и секреты также останутся."
    Pop $0
    nsDialogs::Show
FunctionEnd

Function CleanInstallLeave
    ${NSD_GetState} $CleanInstallCheck $0
    StrCmp $0 ${BST_CHECKED} 0 clean_install_done
    MessageBox MB_YESNO|MB_ICONEXCLAMATION "Удалить все сочетания, настройки и установленные пакеты? Это действие необратимо." IDYES clean_install_done
    Abort
    clean_install_done:
FunctionEnd

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

    StrCmp $CleanInstallCheck "" clean_data_done
    ${NSD_GetState} $CleanInstallCheck $0
    StrCmp $0 ${BST_CHECKED} 0 clean_data_done
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

Function un.ReportLeftovers
    ; Показываем пользователю, что именно помешало удалить каталог установки.
    StrCpy $R0 ""
    StrCpy $R1 0
    FindFirst $R2 $R3 "$INSTDIR\*.*"
    ${If} $R3 == ""
        FindClose $R2
        MessageBox MB_ICONSTOP|MB_OK "Не удалось удалить каталог установки: $INSTDIR. Он занят другой программой или недоступен для записи. Закройте Util Maker и повторите удаление."
        Return
    ${EndIf}
    leftovers_loop:
        StrCmp $R3 "" leftovers_done
        StrCmp $R3 "." leftovers_next
        StrCmp $R3 ".." leftovers_next
        IntOp $R1 $R1 + 1
        ${If} $R1 <= 6
            StrCpy $R0 "$R0$\r$\n• $R3"
        ${EndIf}
    leftovers_next:
        FindNext $R2 $R3
        Goto leftovers_loop
    leftovers_done:
        FindClose $R2
        ${If} $R1 > 6
            StrCpy $R0 "$R0$\r$\n• …"
        ${EndIf}
        MessageBox MB_ICONSTOP|MB_OK "Каталог установки удалён не полностью: $INSTDIR$\r$\n$\r$\nНе удалось удалить:$R0$\r$\n$\r$\nЗакройте Util Maker, проверьте права и удалите оставшиеся файлы вручную."
FunctionEnd

Section "Uninstall"
    ; Приложение могло остаться в трее и держать exe, модуль или журнал.
    nsExec::ExecToStack '"$SYSDIR\taskkill.exe" /F /IM util-maker-desktop.exe'
    Pop $0
    Pop $1
    nsExec::ExecToStack '"$SYSDIR\taskkill.exe" /F /IM control-module.exe'
    Pop $0
    Pop $1
    Sleep 600

    Delete "$DESKTOP\Util Maker.lnk"
    Delete "$SMPROGRAMS\Util Maker\Util Maker.lnk"
    Delete "$SMPROGRAMS\Util Maker\Удалить Util Maker.lnk"
    RMDir "$SMPROGRAMS\Util Maker"

    ; Сначала убираем установленные каталоги целиком, затем известные файлы.
    RMDir /r "$INSTDIR\modules"
    RMDir /r "$INSTDIR\packages"
    RMDir /r "$INSTDIR\scripts"
    RMDir /r "$INSTDIR\logs"
    ; При нестандартном каталоге установки журнал мог уйти в путь по умолчанию.
    StrCmp "$INSTDIR" "$LOCALAPPDATA\Util Maker" uninstall_logs_done
    RMDir /r "$LOCALAPPDATA\Util Maker\logs"
    RMDir "$LOCALAPPDATA\Util Maker"
    uninstall_logs_done:
    Delete "$INSTDIR\util-maker-desktop.exe"
    Delete "$INSTDIR\README.txt"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"

    ; При silent-обновлении деинсталлятор запущен из $INSTDIR через _?=
    ; и не может удалить собственный exe; его уберёт установщик.
    StrCmp $EXEDIR $INSTDIR uninstall_registry
    IfFileExists "$INSTDIR" 0 uninstall_registry
    Call un.ReportLeftovers
    uninstall_registry:

    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UtilMaker"
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
    DeleteRegKey HKCU "Software\Util Maker"
    ; Профиль %LOCALAPPDATA%\org.utilmaker.prototype сохраняется до отдельного
    ; подтверждения. Журнал входит в каталог установки и удаляется выше.
SectionEnd

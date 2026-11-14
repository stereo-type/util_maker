# Занять сочетание Ctrl+Shift+U другой программой для проверки отказа.
#
# Запустите в отдельном окне PowerShell до запуска Util Maker (или до нажатия
# «Перерегистрировать сочетание» в трее). Скрипт держит регистрацию, пока вы не
# нажмёте Enter. После его завершения сочетание можно назначить снова.

param(
    [string]$Combo = "Ctrl+Shift+U"
)

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class UtilMakerHotkeyHolder {
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint modifiers, uint virtualKey);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
}
"@

if ($Combo -ne "Ctrl+Shift+U") {
    throw "Скрипт прототипа занимает только Ctrl+Shift+U"
}

$modifiers = 0x0002 -bor 0x0004 # MOD_CONTROL | MOD_SHIFT
$virtualKey = 0x55             # VK_U
$id = 42

if (-not [UtilMakerHotkeyHolder]::RegisterHotKey([IntPtr]::Zero, $id, $modifiers, $virtualKey)) {
    $error = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "Не удалось занять сочетание, код Win32 $error. Возможно, оно уже занято."
}

Write-Host "Сочетание $Combo занято этим процессом."
Write-Host "Запустите Util Maker: в настройках и подсказке трея должно быть «занято другой программой»."
try {
    Read-Host "Нажмите Enter, чтобы освободить сочетание"
}
finally {
    [void][UtilMakerHotkeyHolder]::UnregisterHotKey([IntPtr]::Zero, $id)
    Write-Host "Сочетание освобождено."
}

# Замеры прототипа Util Maker на Windows 11 x64.
#
# Запуск из обычной сессии PowerShell без административных прав:
#   .\scripts\measure-windows.ps1                     # 60 с простоя
#   .\scripts\measure-windows.ps1 -Seconds 600        # 10 минут
#   .\scripts\measure-windows.ps1 -InvokeBenchmark -InvokeCount 50
#                                                     # 50 холодных вызовов и p95
#
# Private Working Set снимается счётчиком Win32_PerfRawData_PerfProc_Process
# (WorkingSetPrivate) для хоста и всех его потомков: процесса модуля и
# WebView2. Чужие экземпляры msedgewebview2 в замер не попадают.

param(
    [int]$Seconds = 60,
    [int]$IntervalSeconds = 1,
    [switch]$InvokeBenchmark,
    # Сколько холодных вызовов сделать перед разбором журнала. 0 — только разобрать готовый журнал.
    [int]$InvokeCount = 0,
    # Сочетание, которое нажимает скрипт; по умолчанию сочетание контрольного модуля.
    [string]$Hotkey = "Ctrl+Shift+U",
    # Путь виден в настройках приложения: <папка установки>\logs\util-maker.log.
    [string]$LogPath = "$env:LOCALAPPDATA\Util Maker\logs\util-maker.log",
    # Структурированный журнал с firstFrameMs, moduleId, actionId и cold.
    [string]$MetricsPath = "$env:LOCALAPPDATA\Util Maker\logs\util-maker.jsonl"
)

$ErrorActionPreference = "Stop"

if ($InvokeCount -gt 0) {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class UtilMakerKeyboard {
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
}
"@

    function Send-GlobalHotkey([string]$combo) {
        $modifiers = @{
            "CTRL" = 0x11; "CONTROL" = 0x11; "SHIFT" = 0x10;
            "ALT" = 0x12; "WIN" = 0x5B
        }
        $parts = $combo.Split("+") | ForEach-Object { $_.Trim().ToUpperInvariant() }
        $keyName = $parts[-1]
        $keyCode = if ($keyName.Length -eq 1) { [byte][char]$keyName } else {
            switch ($keyName) {
                "ENTER" { 0x0D } "ESC" { 0x1B } "TAB" { 0x09 }
                default { throw "Неизвестная клавиша в Hotkey: $keyName" }
            }
        }
        $pressed = @()
        foreach ($part in $parts[0..($parts.Count - 2)]) {
            if (-not $modifiers.ContainsKey($part)) { throw "Неизвестный модификатор: $part" }
            $code = [byte]$modifiers[$part]
            [UtilMakerKeyboard]::keybd_event($code, 0, 0, [UIntPtr]::Zero)
            $pressed += $code
        }
        [UtilMakerKeyboard]::keybd_event([byte]$keyCode, 0, 0, [UIntPtr]::Zero)
        [UtilMakerKeyboard]::keybd_event([byte]$keyCode, 0, 2, [UIntPtr]::Zero)
        [array]::Reverse($pressed)
        foreach ($code in $pressed) {
            [UtilMakerKeyboard]::keybd_event([byte]$code, 0, 2, [UIntPtr]::Zero)
        }
    }

    if (-not (Get-Process util-maker-desktop -ErrorAction SilentlyContinue)) {
        throw "Util Maker не запущен. Запустите приложение и повторите замер."
    }
    "Холодных вызовов: $InvokeCount, сочетание: $Hotkey"
    for ($index = 1; $index -le $InvokeCount; $index++) {
        $before = if (Test-Path $MetricsPath) {
            (Get-Content -Path $MetricsPath | Measure-Object).Count
        } else { 0 }
        Send-GlobalHotkey $Hotkey
        $deadline = (Get-Date).AddSeconds(10)
        do {
            Start-Sleep -Milliseconds 50
            $after = if (Test-Path $MetricsPath) {
                (Get-Content -Path $MetricsPath | Measure-Object).Count
            } else { 0 }
        } while ($after -le $before -and (Get-Date) -lt $deadline)
        if ($after -le $before) {
            throw "Вызов $index не появился в журнале $MetricsPath за 10 с."
        }
        Start-Sleep -Milliseconds 100
    }
}

function Get-ProductProcessIds {
    $all = Get-CimInstance Win32_Process
    $roots = @(
        $all |
            Where-Object { $_.Name -eq "util-maker-desktop.exe" } |
            Select-Object -ExpandProperty ProcessId
    )
    if ($roots.Count -eq 0) {
        return @()
    }
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($rootId in $roots) {
        [void]$ids.Add([int]$rootId)
    }
    $changed = $true
    while ($changed) {
        $changed = $false
        foreach ($process in $all) {
            if ($ids.Contains([int]$process.ParentProcessId) -and
                -not $ids.Contains([int]$process.ProcessId)) {
                [void]$ids.Add([int]$process.ProcessId)
                $changed = $true
            }
        }
    }
    @($ids)
}

function Get-PrivateWorkingSetBytes {
    $ids = Get-ProductProcessIds
    if ($ids.Count -eq 0) {
        return 0
    }
    $counters = Get-CimInstance Win32_PerfRawData_PerfProc_Process |
        Where-Object { $ids -contains $_.IDProcess }
    ($counters | Measure-Object -Property WorkingSetPrivate -Sum).Sum
}

function Get-TotalCpuSeconds {
    $ids = Get-ProductProcessIds
    if ($ids.Count -eq 0) {
        return 0.0
    }
    $total = 0.0
    foreach ($processId in $ids) {
        try {
            $process = Get-Process -Id $processId -ErrorAction Stop
            $total += $process.TotalProcessorTime.TotalSeconds
        } catch {
            # Процесс завершился между снимками — пропускаем.
        }
    }
    $total
}

if ($InvokeBenchmark) {
    $records = @()
    if (Test-Path $MetricsPath) {
        foreach ($line in Get-Content -Path $MetricsPath) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            try { $record = $line | ConvertFrom-Json } catch { continue }
            if ($null -ne $record.firstFrameMs -and $record.moduleId -and $record.actionId) {
                $records += $record
            }
        }
    }
    if ($records.Count -gt 0) {
        "Записей с первым кадром: {0}. Всего записей: {1}." -f `
            $records.Count, (Get-Content -Path $MetricsPath | Measure-Object).Count
        $groups = $records | Group-Object {
            "{0}/{1} cold={2}" -f $_.moduleId, $_.actionId, [bool]$_.cold
        }
        foreach ($group in $groups) {
            $sorted = @($group.Group | ForEach-Object { [int]$_.firstFrameMs } | Sort-Object)
            $p95Index = [Math]::Min($sorted.Count - 1, [Math]::Ceiling($sorted.Count * 0.95) - 1)
            "{0}: выборка {1}, медиана {2} мс, p95 {3} мс, максимум {4} мс" -f `
                $group.Name, $sorted.Count, $sorted[[Math]::Floor($sorted.Count / 2)], `
                $sorted[$p95Index], $sorted[-1]
        }
        "Облачный запрос измеряется отдельно: firstFrameMs не заменяется durationMs."
        exit 0
    }

    if (-not (Test-Path $LogPath)) {
        throw "Не найден структурированный журнал $MetricsPath или старый $LogPath. Сначала вызовите действие сочетанием 50 раз."
    }
    $values = Select-String -Path $LogPath -Pattern "since_event_ms=(\d+)" |
        ForEach-Object { [int]$_.Matches[0].Groups[1].Value }
    if ($values.Count -eq 0) {
        throw "В журнале нет firstFrameMs или since_event_ms. Проверьте, что действие вызывалось сочетанием или из трея."
    }
    $sorted = $values | Sort-Object
    $p95Index = [Math]::Min($sorted.Count - 1, [Math]::Ceiling($sorted.Count * 0.95) - 1)
    "Старый текстовый журнал: вызовов {0}" -f $values.Count
    "Минимум, мс: {0}" -f $sorted[0]
    "Медиана, мс: {0}" -f $sorted[[Math]::Floor($sorted.Count / 2)]
    "p95, мс: {0}" -f $sorted[$p95Index]
    "Максимум, мс: {0}" -f $sorted[-1]
    exit 0
}

$cores = [Environment]::ProcessorCount
$memory = @()
$cpu = @()
$previousCpu = Get-TotalCpuSeconds
$previousTime = Get-Date

"Замер простоя: {0} с, интервал {1} с, логических ядер {2}" -f $Seconds, $IntervalSeconds, $cores
"Перед замером закройте окна настроек и результата и подождите 60 с."
for ($elapsed = 0; $elapsed -lt $Seconds; $elapsed += $IntervalSeconds) {
    Start-Sleep -Seconds $IntervalSeconds
    $memory += Get-PrivateWorkingSetBytes
    $now = Get-Date
    $currentCpu = Get-TotalCpuSeconds
    $window = ($now - $previousTime).TotalSeconds
    if ($window -gt 0) {
        $cpu += (($currentCpu - $previousCpu) / ($window * $cores)) * 100
    }
    $previousCpu = $currentCpu
    $previousTime = $now
}

$memoryStats = $memory | Measure-Object -Average -Minimum -Maximum
$cpuStats = $cpu | Measure-Object -Average
"Private Working Set, МиБ: среднее {0:N1}, минимум {1:N1}, максимум {2:N1}" -f `
    ($memoryStats.Average / 1MB), ($memoryStats.Minimum / 1MB), ($memoryStats.Maximum / 1MB)
"CPU, % от суммарной мощности: среднее {0:N3}" -f $cpuStats.Average
"Бюджет: память <= 60 МиБ, CPU <= 0,2 %. Для p95 отклика запустите с -InvokeBenchmark."

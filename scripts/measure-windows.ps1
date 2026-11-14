# Замеры прототипа Util Maker на Windows 11 x64.
#
# Запуск из обычной сессии PowerShell без административных прав:
#   .\scripts\measure-windows.ps1                     # 60 с простоя
#   .\scripts\measure-windows.ps1 -Seconds 600        # 10 минут
#   .\scripts\measure-windows.ps1 -InvokeBenchmark    # p95 по журналу
#
# Private Working Set снимается счётчиком Win32_PerfRawData_PerfProc_Process
# (WorkingSetPrivate) для хоста и всех его потомков: процесса модуля и
# WebView2. Чужие экземпляры msedgewebview2 в замер не попадают.

param(
    [int]$Seconds = 60,
    [int]$IntervalSeconds = 1,
    [switch]$InvokeBenchmark,
    [string]$LogPath = "$env:LOCALAPPDATA\Util Maker\logs\util-maker.log"
)

$ErrorActionPreference = "Stop"

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
    if (-not (Test-Path $LogPath)) {
        throw "Журнал не найден: $LogPath. Сначала вызовите действие сочетанием 50 раз."
    }
    $values = Select-String -Path $LogPath -Pattern "since_event_ms=(\d+)" |
        ForEach-Object { [int]$_.Matches[0].Groups[1].Value }
    if ($values.Count -eq 0) {
        throw "В журнале нет строк since_event_ms. Проверьте, что действие вызывалось сочетанием или из трея."
    }
    $sorted = $values | Sort-Object
    $p95Index = [Math]::Min($sorted.Count - 1, [Math]::Ceiling($sorted.Count * 0.95) - 1)
    "Вызовов: {0}" -f $values.Count
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

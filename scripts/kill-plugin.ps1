# Останавливает процесс плагина в момент появления, чтобы воспроизвести сбой.
#
# Запустите в отдельном PowerShell до нажатия «Вызвать»:
#   .\scripts\kill-plugin.ps1
#   .\scripts\kill-plugin.ps1 -Name control-module -TimeoutSeconds 60
#
# Скрипт ждёт процесс и сразу завершает его. Для контрольного модуля окно
# нажатия — доли секунды, поэтому запускать его нужно заранее.

param(
    [string]$Name = "control-module",
    [int]$TimeoutSeconds = 30,
    [int]$PollMilliseconds = 5
)

$ErrorActionPreference = "Stop"
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
Write-Host "Жду процесс $Name до $($deadline.ToString('HH:mm:ss'))..."

while ((Get-Date) -lt $deadline) {
    $process = Get-Process -Name $Name -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $process) {
        Stop-Process -Id $process.Id -Force
        Write-Host "Процесс $Name (PID $($process.Id)) завершён."
        exit 0
    }
    Start-Sleep -Milliseconds $PollMilliseconds
}

throw "Процесс $Name не появился за $TimeoutSeconds с. Запустите приложение и повторите."

# Launch Cyclops several times and tabulate the startup milestones from the developer log
# (ms since process creation). Kills running instances. Usage:
#   powershell -File tools\profile-startup.ps1 -Target <folder or image> -Runs 5
param([string]$Target = "", [int]$Runs = 5, [string]$Exe = "C:\Git\Cyclops\target\release\cyclops.exe")
$log = "$env:LOCALAPPDATA\Cyclops\perf.log"
# Release builds only write the developer log in developer mode.
$env:CYCLOPS_DEV = "1"
$rows = @()
foreach ($run in 1..$Runs) {
    Get-Process cyclops -ErrorAction SilentlyContinue | Stop-Process -Force
    Get-Process cyclops -ErrorAction SilentlyContinue | Wait-Process
    Start-Sleep -Milliseconds 800
    $before = (Get-Content $log).Count
    if ($Target) { Start-Process -FilePath $Exe -ArgumentList "`"$Target`"" } else { Start-Process -FilePath $Exe }
    Start-Sleep -Seconds 5
    $lines = Get-Content $log | Select-Object -Skip $before
    $m = @{}
    foreach ($l in $lines) {
        if ($l -match 'Startup\s+([\d.]+) ms  (\w+)') { $m[$Matches[2]] = [double]$Matches[1] }
        if ($l -match 'Startup\s+([\d.]+) ms  first frame on screen') { $m['onscreen'] = [double]$Matches[1] }
        if ($l -match 'Startup\s+([\d.]+) ms  window and GPU') { $m['gpu'] = [double]$Matches[1] }
        if ($l -match 'Startup\s+([\d.]+) ms  app ready') { $m['app'] = [double]$Matches[1] }
        if ($l -match 'Startup\s+([\d.]+) ms  visible thumbnails') { $m['thumbs'] = [double]$Matches[1] }
        if ($l -match 'Startup\s+([\d.]+) ms  creating window') { $m['create'] = [double]$Matches[1] }
    }
    $rows += [pscustomobject]@{ run = $run; main = $m['main']; create = $m['create']; gpu_ready = $m['gpu']; app_ready = $m['app']; on_screen = $m['onscreen']; thumbs = $m['thumbs'] }
}
Get-Process cyclops -ErrorAction SilentlyContinue | Stop-Process -Force
$rows | Format-Table -AutoSize

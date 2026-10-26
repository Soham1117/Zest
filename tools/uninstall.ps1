#requires -Version 5.1
<#
  Zest uninstaller. Removes the right-click verb, the Send To shortcut and the
  sign-in entry, stops Zest and deletes %LOCALAPPDATA%\Programs\Zest.
  Asks before it deletes your settings in %APPDATA%\Zest (default: keep).
  Usage: powershell -ExecutionPolicy Bypass -File tools\uninstall.ps1
#>
$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "[uninstall] $m" }

$dest = Join-Path $env:LOCALAPPDATA 'Programs\Zest'
$exe  = Join-Path $dest 'zest.exe'

try {
    if (Test-Path $exe) {
        Info 'Removing the right-click verb, the Send To shortcut and start at sign-in'
        $p = Start-Process -FilePath $exe -ArgumentList '--uninstall' -Wait -PassThru -NoNewWindow
        if ($p.ExitCode -ne 0) { Write-Warning "zest.exe --uninstall returned exit code $($p.ExitCode)." }
    } else {
        Write-Warning "zest.exe not found in $dest. The registry entries may remain."
    }

    if (Get-AppxPackage -Name 'Zest.ContextMenu' -ErrorAction SilentlyContinue) {
        Info 'Removing the Windows 11 menu package'
        & (Join-Path $PSScriptRoot 'unregister-win11-menu.ps1')
    }

    $running = Get-Process -Name zest, zest-worker -ErrorAction SilentlyContinue
    if ($running) {
        Info 'Stopping Zest'
        $running | Stop-Process -Force
        $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    }

    if (Test-Path $dest) {
        Info "Deleting $dest"
        Remove-Item $dest -Recurse -Force
    }

    $cfg = Join-Path $env:APPDATA 'Zest'
    if (Test-Path $cfg) {
        $answer = Read-Host "Delete your settings in $cfg ? [y/N]"
        if ($answer -match '^(y|yes)$') {
            Remove-Item $cfg -Recurse -Force
            Info 'Settings deleted'
        } else {
            Info 'Settings kept'
        }
    }
    Info 'Done. Logs, history and state stay in %LOCALAPPDATA%\Zest (delete that folder by hand if you want).'
} catch {
    Write-Host "[uninstall] ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

#requires -Version 5.1
<#
  Zest installer (per user, no admin rights).
  Builds the release exes (zest.exe and zest-worker.exe), copies them with the tools to %LOCALAPPDATA%\Programs\Zest,
  adds the right-click verb, the Send To shortcut and the sign-in entry, and starts Zest.
  Run tools\fetch-deps.ps1 first so that third_party\bin has libvips, FFmpeg and PDFium.
  Usage: powershell -ExecutionPolicy Bypass -File tools\install.ps1
#>
$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "[install] $m" }

$root    = Split-Path -Parent $PSScriptRoot
$dest    = Join-Path $env:LOCALAPPDATA 'Programs\Zest'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

try {
    Push-Location $root
    try {
        Info 'Building (cargo build --release -p zest-app -p zest-worker -p zest-explorer-command)'
        & cargo build --release -p zest-app -p zest-worker -p zest-explorer-command
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit code $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }

    $exeSrc = Join-Path $root 'target\release\zest.exe'
    if (-not (Test-Path $exeSrc)) { throw "Build output not found: $exeSrc" }
    $workerSrc = Join-Path $root 'target\release\zest-worker.exe'
    if (-not (Test-Path $workerSrc)) { throw "Build output not found: $workerSrc" }

    # Stop a running Zest so the exe can be replaced.
    $running = Get-Process -Name zest, zest-worker -ErrorAction SilentlyContinue
    if ($running) {
        Info 'Stopping the running Zest'
        $running | Stop-Process -Force
        $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    }

    New-Item -ItemType Directory -Force $dest | Out-Null
    Info "Copying zest.exe to $dest"
    Copy-Item $exeSrc (Join-Path $dest 'zest.exe') -Force
    Info "Copying zest-worker.exe to $dest"
    Copy-Item $workerSrc (Join-Path $dest 'zest-worker.exe') -Force

    $dllSrc = Join-Path $root 'target\release\zest_explorer_command.dll'
    if (Test-Path $dllSrc) {
        Info "Copying zest_explorer_command.dll to $dest"
        try {
            Copy-Item $dllSrc (Join-Path $dest 'zest_explorer_command.dll') -Force
        } catch {
            Write-Warning 'Could not replace zest_explorer_command.dll (Explorer has it loaded). Run tools\unregister-win11-menu.ps1, install again, then run tools\register-win11-menu.ps1.'
        }
    }

    $tools = Join-Path $root 'third_party\bin'
    if (Test-Path $tools) {
        Info 'Copying tools (third_party\bin)'
        $toolsDest = Join-Path $dest 'third_party\bin'
        New-Item -ItemType Directory -Force $toolsDest | Out-Null
        & robocopy $tools $toolsDest /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy failed (exit code $LASTEXITCODE)" }
    } else {
        Write-Warning 'third_party\bin not found. Run tools\fetch-deps.ps1, then run this script again.'
    }

    $exe = Join-Path $dest 'zest.exe'
    Info 'Adding the right-click verb, the Send To shortcut and start at sign-in'
    $p = Start-Process -FilePath $exe -ArgumentList '--install' -Wait -PassThru -NoNewWindow
    if ($p.ExitCode -ne 0) { Write-Warning "zest.exe --install returned exit code $($p.ExitCode). See the messages above." }

    # Optional Win11 top-level menu (sparse package). Needs Windows SDK + admin once.
    $bins = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $hasSdk = $false
    if (Test-Path $bins) {
        $hasSdk = [bool](Get-ChildItem "$bins\*\x64\makeappx.exe" -ErrorAction SilentlyContinue | Select-Object -First 1)
    }
    if ($hasSdk) {
        Write-Host ''
        Write-Host 'Windows SDK found. Register "Convert with Zest" in the top-level Windows 11 menu?'
        Write-Host '(Needs administrator rights once to trust a local certificate. Classic verb stays as fallback.)'
        $ans = Read-Host 'Register now? [y/N]'
        if ($ans -match '^[Yy]') {
            $reg = Join-Path $PSScriptRoot 'register-win11-menu.ps1'
            Info "Launching $reg (elevation prompt may appear)"
            Start-Process -FilePath 'powershell.exe' -ArgumentList @(
                '-ExecutionPolicy', 'Bypass', '-File', $reg
            ) -Verb RunAs -Wait
        } else {
            Info 'Skipped. Later: tools\register-win11-menu.ps1 or tray "Top-level Convert with Zest".'
        }
    } else {
        Info 'Windows SDK not found — skipping top-level Win11 menu. Classic right-click verb is installed.'
        Info 'Optional later: install the SDK, then tools\register-win11-menu.ps1'
    }

    Info 'Starting Zest'
    Start-Process -FilePath $exe
    Info "Done. Zest is in $dest"
} catch {
    Write-Host "[install] ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

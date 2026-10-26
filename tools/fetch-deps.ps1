#requires -Version 5.1
<#
  Zest dependency fetcher. Downloads libvips, FFmpeg and PDFium into third_party/bin/.
  Usage: powershell -ExecutionPolicy Bypass -File tools\fetch-deps.ps1 [-Force]
  Pins live in tools\deps.lock.json (name, version, url, sha256).
#>
param([switch]$Force, [string[]]$Optional = @())
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without the progress bar

$root     = Split-Path -Parent $PSScriptRoot
$lockPath = Join-Path $PSScriptRoot 'deps.lock.json'
$binRoot  = Join-Path $root 'third_party\bin'
$dlRoot   = Join-Path $root 'third_party\download'

function Info($m) { Write-Host "[fetch-deps] $m" }

function Save-Lock($lock) {
    $json = $lock | ConvertTo-Json -Depth 6
    [IO.File]::WriteAllText($lockPath, $json, (New-Object Text.UTF8Encoding($false)))
}

try {
    $lock = Get-Content $lockPath -Raw | ConvertFrom-Json
    New-Item -ItemType Directory -Force $binRoot, $dlRoot | Out-Null

    foreach ($dep in $lock.dependencies) {
        $isOptional = ($dep.PSObject.Properties.Name -contains 'optional') -and $dep.optional
        if ($isOptional -and ($Optional -notcontains $dep.name)) { continue }
        $dest = Join-Path $binRoot $dep.dest
        $exe  = Join-Path $dest ($dep.exe -replace '/', '\')
        if ((Test-Path $exe) -and -not $Force) {
            Info "$($dep.name) $($dep.version): already present, skipping."
            continue
        }

        $isTgz = ($dep.PSObject.Properties.Name -contains 'archive') -and ($dep.archive -eq 'tgz')
        $tmp = Join-Path $dlRoot ($dep.name + $(if ($isTgz) { '.tgz' } else { '.zip' }))
        $ext = Join-Path $dlRoot ($dep.name + '-extract')
        if (Test-Path $tmp) { Remove-Item $tmp -Force }
        if (Test-Path $ext) { Remove-Item $ext -Recurse -Force }

        Info "$($dep.name): downloading $($dep.url)"
        Invoke-WebRequest -Uri $dep.url -OutFile $tmp -UseBasicParsing
        $size = [math]::Round((Get-Item $tmp).Length / 1MB, 1)
        Info "$($dep.name): downloaded $size MB, verifying SHA-256"

        $actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
        if ([string]::IsNullOrWhiteSpace($dep.sha256)) {
            $dep.sha256 = $actual
            Save-Lock $lock
            Info "$($dep.name): pinned (first use) sha256=$actual"
        } elseif ($dep.sha256.ToLower() -ne $actual) {
            Remove-Item $tmp -Force
            throw "$($dep.name): SHA-256 mismatch. expected $($dep.sha256), got $actual. Download deleted."
        } else {
            Info "$($dep.name): SHA-256 OK"
        }

        Info "$($dep.name): extracting"
        if ($isTgz) {
            # Windows tar.exe (bsdtar) reads .tgz.
            New-Item -ItemType Directory -Force $ext | Out-Null
            $tar = Join-Path $env:SystemRoot 'System32\tar.exe'
            & $tar -xzf $tmp -C $ext
            if ($LASTEXITCODE -ne 0) { throw "$($dep.name): tar.exe failed (exit code $LASTEXITCODE)" }
        } else {
            Expand-Archive -Path $tmp -DestinationPath $ext -Force
        }
        # Archives hold one top-level folder; use it as the root when it holds the exe.
        $src = $ext
        $kids = @(Get-ChildItem $ext)
        if ($kids.Count -eq 1 -and $kids[0].PSIsContainer) { $src = $kids[0].FullName }
        if (-not (Test-Path (Join-Path $src ($dep.exe -replace '/', '\')))) {
            throw "$($dep.name): $($dep.exe) not found in archive."
        }
        if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
        Move-Item $src $dest
        Remove-Item $tmp -Force
        if (Test-Path $ext) { Remove-Item $ext -Recurse -Force }
        Info "$($dep.name): installed to $dest"
    }
    if ((Test-Path $dlRoot) -and -not (Get-ChildItem $dlRoot)) { Remove-Item $dlRoot -Force }
    Info 'done.'
    exit 0
} catch {
    Write-Host "[fetch-deps] ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

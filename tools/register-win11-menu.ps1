#requires -Version 5.1
<#
  Adds "Convert with Zest" to the top-level Windows 11 context menu.
  Needs a sparse package, and a sparse package needs a trusted signature.
  This script makes a self-signed certificate "CN=Zest Local", trusts it for this PC,
  builds and signs a sparse MSIX, and registers it with the Zest install folder.
  Run install.ps1 first (the DLL and zest.exe must be in the install folder).
  The first run needs an elevated PowerShell (only to trust the certificate).
  Usage: powershell -ExecutionPolicy Bypass -File tools\register-win11-menu.ps1
#>
$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "[register-win11-menu] $m" }

$root    = Split-Path -Parent $PSScriptRoot
$dest    = Join-Path $env:LOCALAPPDATA 'Programs\Zest'
$subject = 'CN=Zest Local'
$pkgName = 'Zest.ContextMenu'
$work    = Join-Path $env:TEMP 'zest-sparse'

try {
    foreach ($f in 'zest.exe', 'zest_explorer_command.dll') {
        if (-not (Test-Path (Join-Path $dest $f))) {
            throw "$f not found in $dest. Run tools\install.ps1 first."
        }
    }

    # Windows SDK tools.
    $bins = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $makeappx = $null; $signtool = $null
    if (Test-Path $bins) {
        $makeappx = Get-ChildItem "$bins\*\x64\makeappx.exe" -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
        $signtool = Get-ChildItem "$bins\*\x64\signtool.exe" -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
    }
    if (-not $makeappx -or -not $signtool) {
        Write-Host 'The Windows SDK (MakeAppx.exe and SignTool.exe) is not installed. Install it with:'
        Write-Host '  winget install Microsoft.WindowsSDK.10.0.26100'
        Write-Host 'Then run this script again.'
        exit 1
    }

    # Certificate (CurrentUser\My).
    $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.NotAfter -gt (Get-Date) } | Sort-Object NotAfter -Descending | Select-Object -First 1
    if (-not $cert) {
        Info "Creating the certificate $subject"
        $cert = New-SelfSignedCertificate -Type Custom -Subject $subject -KeyUsage DigitalSignature `
            -FriendlyName 'Zest Local' -CertStoreLocation 'Cert:\CurrentUser\My' -NotAfter (Get-Date).AddYears(5) `
            -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
    }

    # Trust it for this PC (needs elevation).
    $trusted = Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object { $_.Thumbprint -eq $cert.Thumbprint }
    if (-not $trusted) {
        $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        if (-not $admin) {
            Write-Host 'The certificate must be trusted for this PC. This one step needs administrator rights.'
            Write-Host 'Open PowerShell as administrator and run this script again:'
            Write-Host "  powershell -ExecutionPolicy Bypass -File `"$PSCommandPath`""
            exit 1
        }
        Info 'Trusting the certificate (LocalMachine\TrustedPeople)'
        New-Item -ItemType Directory -Force $work | Out-Null
        $cer = Join-Path $work 'zest-local.cer'
        Export-Certificate -Cert $cert -FilePath $cer | Out-Null
        Import-Certificate -FilePath $cer -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    }

    # Stage the package files: manifest + logo only (the rest is external content).
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    $stage = Join-Path $work 'stage'
    New-Item -ItemType Directory -Force (Join-Path $stage 'Assets') | Out-Null
    Copy-Item (Join-Path $PSScriptRoot 'sparse\AppxManifest.xml') $stage
    Add-Type -AssemblyName System.Drawing
    $bmp = New-Object System.Drawing.Bitmap 150, 150
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 140, 0))), 8, 8, 134, 134)
    $g.Dispose()
    $bmp.Save((Join-Path $stage 'Assets\logo.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()

    $msix = Join-Path $work 'zest.msix'
    Info 'Building the package'
    & $makeappx.FullName pack /d $stage /p $msix /nv /o | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "makeappx failed (exit code $LASTEXITCODE)" }

    Info 'Signing the package'
    & $signtool.FullName sign /fd SHA256 /sha1 $cert.Thumbprint $msix | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signtool failed (exit code $LASTEXITCODE)" }

    $old = Get-AppxPackage -Name $pkgName
    if ($old) {
        Info 'Removing the old package'
        $old | Remove-AppxPackage
    }

    Info "Registering the package (external location: $dest)"
    Add-AppxPackage -Path $msix -ExternalLocation $dest

    Info 'Done. Restart Explorer to see the entry at once:'
    Write-Host '  Stop-Process -Name explorer -Force   (Explorer restarts by itself)'
} catch {
    Write-Host "[register-win11-menu] ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

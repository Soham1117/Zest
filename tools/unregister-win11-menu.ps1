#requires -Version 5.1
<#
  Removes the Windows 11 top-level "Convert with Zest" entry (the sparse package).
  The certificate "CN=Zest Local" stays. To remove it (administrator rights needed for the second line):
    Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -eq 'CN=Zest Local' | Remove-Item
    Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq 'CN=Zest Local' | Remove-Item
  Usage: powershell -ExecutionPolicy Bypass -File tools\unregister-win11-menu.ps1
#>
$ErrorActionPreference = 'Stop'

try {
    $pkg = Get-AppxPackage -Name 'Zest.ContextMenu'
    if ($pkg) {
        Write-Host '[unregister-win11-menu] Removing the package Zest.ContextMenu'
        $pkg | Remove-AppxPackage
    } else {
        Write-Host '[unregister-win11-menu] The package is not installed.'
    }
    Write-Host '[unregister-win11-menu] The certificate "CN=Zest Local" was not removed. To remove it, run in PowerShell (the second line needs administrator rights):'
    Write-Host "  Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -eq 'CN=Zest Local' | Remove-Item"
    Write-Host "  Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq 'CN=Zest Local' | Remove-Item"
} catch {
    Write-Host "[unregister-win11-menu] ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

<# Install our signed call-enabled app. No Phone Link, certificate trust changes,
   driver modifications, or silent process termination. #>
[CmdletBinding(DefaultParameterSetName='Signed')]
param(
    [Parameter(Mandatory,ParameterSetName='Signed')][string]$PackagePath,
    [Parameter(Mandatory,ParameterSetName='Development')][string]$DevelopmentDirectory
)
$ErrorActionPreference = 'Stop'
if (Get-Process -Name nexus-app -ErrorAction SilentlyContinue) {
    throw 'Quit Link Tunnel from its tray menu before installing, so the old app cannot handle the new app activation.'
}
if ($PSCmdlet.ParameterSetName -eq 'Development') {
    $enabled = Get-ItemPropertyValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' -Name AllowDevelopmentWithoutDevLicense -ErrorAction SilentlyContinue
    if ($enabled -ne 1) { throw 'Developer Mode is not enabled. Use a signed package instead. This script does not change Windows development settings.' }
    $directory = (Resolve-Path -LiteralPath $DevelopmentDirectory).Path
    $manifestPath = Join-Path $directory 'AppxManifest.xml'
    [xml]$manifest = Get-Content -LiteralPath $manifestPath -Raw
    if ($manifest.Package.Identity.Name -ne 'LinkTunnel.Desktop.Calls' -or -not (Test-Path -LiteralPath (Join-Path $directory 'nexus-app.exe'))) {
        throw 'Choose the development directory produced by build-calls-msix.ps1.'
    }
    Add-AppxPackage -Register -Path $manifestPath
} else {
    $package = (Resolve-Path -LiteralPath $PackagePath).Path
    if ([IO.Path]::GetExtension($package) -ne '.msix') { throw 'Choose the signed Link Tunnel .msix package.' }
    $signature = Get-AuthenticodeSignature -LiteralPath $package
    if ($signature.Status -ne 'Valid') { throw "The package needs a valid, trusted signature ($($signature.Status))." }
    Add-AppxPackage -Path $package
}
Write-Host 'Open Link Tunnel from Start, then Phone > Cellular calls on this computer.'
Write-Host 'Pair the phone in Bluetooth settings, select that Bluetooth phone in Link Tunnel, and connect call audio.'

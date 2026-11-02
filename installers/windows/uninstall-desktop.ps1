param([Parameter(Mandatory)][string]$InstallDirectory)
$ErrorActionPreference = 'Stop'
$service = Get-CimInstance Win32_Service -Filter "Name='NexusAgent'"
$expected = Join-Path $InstallDirectory 'nexus-agent.exe'
if ($service -and $service.PathName.StartsWith(('"' + $expected + '"'), [StringComparison]::OrdinalIgnoreCase)) {
    Stop-Service NexusAgent
    & sc.exe delete NexusAgent | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not remove the Link Tunnel service.' }
}
# Keep state and the owner enrollment so reinstalling retains this device's identity.

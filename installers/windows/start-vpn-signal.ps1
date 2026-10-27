<# Startup launcher: wait for the VPN adapter before binding the private pairing server. #>
param(
    [Parameter(Mandatory)][ipaddress]$Address,
    [ValidateRange(1024, 65535)][int]$Port = 8443
)
$ErrorActionPreference = 'Stop'
$deadline = [DateTime]::UtcNow.AddSeconds(60)
while (-not (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object IPAddress -EQ $Address.ToString())) {
    if ([DateTime]::UtcNow -ge $deadline) { Write-Error 'VPN address is not available yet.'; exit 1 }
    Start-Sleep -Seconds 1
}
$state = Join-Path $env:ProgramData 'Nexus\signal'
$database = 'sqlite://' + (Join-Path $state 'pairings.db').Replace('\', '/')
$signal = Join-Path $env:ProgramFiles 'linktunnel\nexus-signal.exe'
# Accept hosts installed before the product rename.
if (-not (Test-Path -LiteralPath $signal)) { $signal = Join-Path $env:ProgramFiles 'Nexus Remote\nexus-signal.exe' }
& $signal `
    --listen "$($Address):$Port" --audience "$($Address):$Port" `
    --database-url $database --log-dir (Join-Path $state 'logs')
exit $LASTEXITCODE

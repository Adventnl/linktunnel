param(
    [switch]$UpdateDesktopHost,
    [string]$AgentBinary = "$PSScriptRoot\..\..\target\release\nexus-agent.exe",
    [int]$Port = 9443
)
$ErrorActionPreference = 'Stop'
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this script in Administrator PowerShell to configure the Tailscale-only firewall rule and optionally update the SYSTEM desktop host.' }
if ($Port -lt 1024 -or $Port -gt 65535) { throw 'Invalid TCP port.' }
$adapter = Get-NetAdapter -Name 'Tailscale' -ErrorAction Stop
$address = Get-NetIPAddress -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 | Where-Object IPAddress -Like '100.*' | Select-Object -First 1 -ExpandProperty IPAddress
if (-not $address) { throw 'Connect Tailscale before finishing mobile setup.' }
# Validate the update fully before touching the running service.
$install = Join-Path $env:ProgramFiles 'Nexus Remote'
if ($UpdateDesktopHost) {
    if (-not (Test-Path $AgentBinary)) { throw 'Build nexus-agent in release mode first.' }
    if (-not (Test-Path (Join-Path $install 'nexus-agent.exe'))) { throw 'Installed Nexus desktop host was not found.' }
    Get-Service NexusAgent -ErrorAction Stop | Out-Null
}
$ruleName = "Nexus Codex Mobile $Port"
$existing = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
if ($existing) { $existing | Remove-NetFirewallRule }
New-NetFirewallRule -DisplayName $ruleName -Direction Inbound -Action Allow -Protocol TCP -LocalPort $Port -LocalAddress $address -InterfaceAlias $adapter.Name -RemoteAddress '100.64.0.0/10' -Profile Any | Out-Null
if ($UpdateDesktopHost) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $agent = Join-Path $install 'nexus-agent.exe'
    $backupAgent = "$agent.before-mobile-$stamp.bak"
    Copy-Item -LiteralPath $agent -Destination $backupAgent
    Stop-Service NexusAgent
    try {
        Copy-Item -LiteralPath $AgentBinary -Destination $agent -Force
        Start-Service NexusAgent
        (Get-Service NexusAgent).WaitForStatus('Running', [TimeSpan]::FromSeconds(20))
    } catch {
        $updateError = $_
        Stop-Service NexusAgent -ErrorAction SilentlyContinue
        try {
            Copy-Item -LiteralPath $backupAgent -Destination $agent -Force
        } finally { Start-Service NexusAgent }
        throw $updateError
    }
    Write-Output 'Updated the desktop host, retaining identity, configuration and pairings. A live Mac sleep/wake test is still required.'
}
Write-Output "Allowed mobile HTTPS on ${address}:$Port only through the Tailscale interface."

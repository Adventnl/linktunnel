<#
.SYNOPSIS
Moves an installed linktunnel Windows host onto its authenticated Tailscale address.
.DESCRIPTION
Run after Tailscale sign-in. Preserves device identity, pairings and authentication factors.
Backs up host configuration/task and restores them if local activation fails.
#>
param([ValidateRange(1024, 65535)][int]$SignalPort = 8443)
$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run configure-tailscale.ps1 from an Administrator PowerShell window.'
}
$tailscale = Join-Path $env:ProgramFiles 'Tailscale\tailscale.exe'
$install = Join-Path $env:ProgramFiles 'linktunnel'
# Accept hosts installed before the product rename.
if (-not (Test-Path (Join-Path $install 'nexus.exe'))) { $install = Join-Path $env:ProgramFiles 'Nexus Remote' }
$state = Join-Path $env:ProgramData 'Nexus'
$config = Join-Path $state 'config\config.toml'
$cli = Join-Path $install 'nexus.exe'
$launcherSource = Join-Path $PSScriptRoot 'start-vpn-signal.ps1'
foreach ($path in @($tailscale, $config, $cli, $launcherSource)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing prerequisite: $path" }
}
$vpnStatus = & $tailscale status --json | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $vpnStatus.BackendState -ne 'Running') {
    throw 'Sign in to Tailscale on this PC and connect before configuring linktunnel.'
}
$address = @($vpnStatus.TailscaleIPs | Where-Object { $_ -match '^100\.' }) | Select-Object -First 1
if (-not $address) { throw 'Tailscale did not report an IPv4 address.' }
$adapter = Get-NetIPAddress -AddressFamily IPv4 | Where-Object IPAddress -EQ $address | Select-Object -First 1
if (-not $adapter) { throw 'The Tailscale IPv4 address is not assigned to a local adapter.' }
$url = "ws://${address}:$SignalPort/v1/ws"
$originalConfig = Get-Content -LiteralPath $config -Raw
$netSection = [regex]::Match($originalConfig, '(?ms)^\[net\][^\r\n]*\r?\n.*?(?=^\[|\z)')
if (-not $netSection.Success) { throw 'Existing host configuration needs a [net] section.' }
$section = $netSection.Value
foreach ($setting in @(@('signal_url', ('"' + $url + '"')), @('allow_insecure_signal', 'true'))) {
    $pattern = '(?m)^\s*' + $setting[0] + '\s*=[^\r\n]*'
    if ([regex]::Matches($section, $pattern).Count -ne 1) {
        throw "Existing [net] must contain exactly one $($setting[0]) setting."
    }
    $section = [regex]::Replace($section, $pattern, $setting[0] + ' = ' + $setting[1])
}
$newConfig = $originalConfig.Substring(0, $netSection.Index) + $section + $originalConfig.Substring($netSection.Index + $netSection.Length)
$originalTask = Export-ScheduledTask -TaskName NexusSignal
$backup = Join-Path $state ('backup-before-vpn-' + [DateTime]::UtcNow.ToString('yyyyMMddHHmmssfff'))
New-Item -ItemType Directory -Path $backup | Out-Null
Copy-Item -LiteralPath $config -Destination (Join-Path $backup 'config.toml')
$originalTask | Set-Content -LiteralPath (Join-Path $backup 'NexusSignal.xml') -Encoding UTF8
& $tailscale set --unattended=true
if ($LASTEXITCODE -ne 0) { throw 'Could not enable Tailscale unattended mode.' }
$launcher = Join-Path $install 'start-vpn-signal.ps1'
Copy-Item -LiteralPath $launcherSource -Destination $launcher -Force
$vpnRules = @('NexusRemoteHostVPN', 'NexusRemoteSignalVPN')
try {
    [IO.File]::WriteAllText($config, $newConfig, [Text.UTF8Encoding]::new($false))
    # Validate the generated configuration before disrupting the running desktop host.
    & $cli --lang en --system doctor
    if ($LASTEXITCODE -ne 0) { throw 'Generated VPN configuration did not pass diagnostics.' }
    Stop-Service NexusAgent
    (Get-Service NexusAgent).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    Stop-ScheduledTask -TaskName NexusSignal
    foreach ($name in $vpnRules) { Get-NetFirewallRule -Name $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule }
    $firewall = @{
        Direction = 'Inbound'; Action = 'Allow'; Profile = 'Any'
        InterfaceAlias = $adapter.InterfaceAlias; LocalAddress = $address; RemoteAddress = '100.64.0.0/10'
    }
    New-NetFirewallRule @firewall -Name $vpnRules[0] -DisplayName 'linktunnel: encrypted control over Tailscale' -Program (Join-Path $install 'nexus-agent.exe') -Protocol UDP | Out-Null
    New-NetFirewallRule @firewall -Name $vpnRules[1] -DisplayName 'linktunnel: pairing over Tailscale' -Program (Join-Path $install 'nexus-signal.exe') -Protocol TCP -LocalPort $SignalPort | Out-Null
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $arguments = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -Address {1} -Port {2}' -f $launcher, $address, $SignalPort
    $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments -WorkingDirectory $install
    Set-ScheduledTask -TaskName NexusSignal -Action $action | Out-Null
    Start-ScheduledTask -TaskName NexusSignal
    Start-Service NexusAgent
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        try {
            $status = & $cli --lang en --system status 2>$null
            if ($LASTEXITCODE -eq 0 -and ($status -join "`n") -match ('connected \(' + [regex]::Escape($url) + '\)')) {
                $ready = $true; break
            }
        } catch { }
        Start-Sleep -Seconds 1
    }
    if (-not $ready) { throw 'linktunnel did not connect to the VPN pairing server.' }
    & $cli --lang en --system doctor
    if ($LASTEXITCODE -ne 0) { throw 'VPN host diagnostics failed.' }
    Write-Host "VPN host configured. Mac signaling URL: $url"
    Write-Host "Recovery backup: $backup"
    Write-Host 'Connect the MacBook to the same Tailscale network; then pair and test from a hotspot.'
} catch {
    $activationError = $_
    try {
        Stop-Service NexusAgent -ErrorAction SilentlyContinue
        Stop-ScheduledTask -TaskName NexusSignal -ErrorAction SilentlyContinue
        Copy-Item -LiteralPath (Join-Path $backup 'config.toml') -Destination $config -Force
        Register-ScheduledTask -TaskName NexusSignal -Xml $originalTask -Force | Out-Null
        foreach ($name in $vpnRules) { Get-NetFirewallRule -Name $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule }
        Start-ScheduledTask -TaskName NexusSignal
        Start-Service NexusAgent
    } catch {
        throw "VPN activation failed: $activationError. Recovery also failed: $_. Backups: $backup"
    }
    throw "VPN activation failed; previous host configuration restored: $activationError"
}

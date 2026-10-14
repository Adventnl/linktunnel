<#
.SYNOPSIS
Installs the Windows SYSTEM host and, optionally, a private-LAN pairing server.
.EXAMPLE
./install-host.ps1 -LanAddress 192.168.1.32
.EXAMPLE
./install-host.ps1 -SignalUrl wss://signal.example.com/v1/ws
#>
[CmdletBinding(DefaultParameterSetName = 'Lan')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Lan')][ipaddress]$LanAddress,
    [Parameter(Mandatory, ParameterSetName = 'Remote')][uri]$SignalUrl,
    [ValidateRange(1024, 65535)][int]$SignalPort = 8443,
    [switch]$KeepAwakeOnAc,
    [string]$BundleDirectory = $PSScriptRoot
)
$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this installer from an Administrator PowerShell window.'
}
$localSignal = $PSCmdlet.ParameterSetName -eq 'Lan'
if ($localSignal) {
    $bytes = $LanAddress.GetAddressBytes()
    $private = $bytes.Length -eq 4 -and ($bytes[0] -eq 10 -or ($bytes[0] -eq 172 -and $bytes[1] -ge 16 -and $bytes[1] -le 31) -or ($bytes[0] -eq 192 -and $bytes[1] -eq 168))
    if (-not $private) { throw 'The LAN pairing server requires an RFC1918 private IPv4 address.' }
    if (-not (Get-NetIPAddress -AddressFamily IPv4 | Where-Object IPAddress -EQ $LanAddress.ToString())) {
        throw 'LanAddress must be an address assigned to this PC.'
    }
    $SignalUrl = [uri]('ws://{0}:{1}/v1/ws' -f $LanAddress, $SignalPort)
} elseif ($SignalUrl.Scheme -ne 'wss' -or $SignalUrl.UserInfo -or $SignalUrl.AbsolutePath -ne '/v1/ws') {
    throw 'An external signaling server must use wss://host/v1/ws with no embedded credentials.'
}
$install = Join-Path $env:ProgramFiles 'Nexus Remote'
$state = Join-Path $env:ProgramData 'Nexus'
$config = Join-Path $state 'config\config.toml'
$required = @('nexus-agent.exe', 'nexus.exe')
if ($localSignal) { $required += 'nexus-signal.exe' }
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $BundleDirectory $name))) { throw "Missing bundle file: $name" }
}
if (Test-Path -LiteralPath $config) {
    # Preserve paired identities, factor enrollment, and user configuration on repeat installs.
    $old = Get-Content -LiteralPath $config -Raw
    if ($old -notmatch [regex]::Escape($SignalUrl.AbsoluteUri)) {
        throw "Existing configuration uses another server. Edit $config explicitly before changing it."
    }
}
foreach ($path in @($install, $state)) {
    if ((Test-Path -LiteralPath $path) -and ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Refusing a reparse-point installation directory: $path"
    }
}
$service = Get-Service NexusAgent -ErrorAction SilentlyContinue
if ($service) { Stop-Service NexusAgent; $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30)) }
if (Get-ScheduledTask -TaskName 'NexusSignal' -ErrorAction SilentlyContinue) { Stop-ScheduledTask -TaskName 'NexusSignal' }
New-Item -ItemType Directory -Force -Path $install, $state | Out-Null
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetAccessRuleProtection($true, $false)
foreach ($sid in @('S-1-5-18', 'S-1-5-32-544')) {
    $rule = [Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($sid), 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $state -AclObject $acl
foreach ($name in $required) { Copy-Item -LiteralPath (Join-Path $BundleDirectory $name) -Destination $install -Force }
$app = Join-Path $BundleDirectory 'nexus-app.exe'
if (Test-Path -LiteralPath $app) { Copy-Item -LiteralPath $app -Destination $install -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $state 'config'), (Join-Path $state 'logs') | Out-Null
if (-not (Test-Path -LiteralPath $config)) {
    $insecure = if ($localSignal) { 'true' } else { 'false' }
    $logs = (Join-Path $state 'logs').Replace('\', '/')
    @"
[net]
signal_url = "$($SignalUrl.AbsoluteUri)"
allow_insecure_signal = $insecure
lan_discovery = true

[security]
unattended_requires_stepup = true

[terminal]
enabled = false

[log]
ansi = false
file_dir = "$logs"
"@ | Set-Content -LiteralPath $config -Encoding ASCII
}
function Invoke-ServiceCommand([string[]]$Arguments) {
    & sc.exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Service configuration failed: sc.exe $($Arguments -join ' ')" }
}
$agent = Join-Path $install 'nexus-agent.exe'
$binPath = '"{0}" --windows-service' -f $agent
if ($service) {
    $registered = Get-CimInstance Win32_Service -Filter "Name='NexusAgent'"
    $changed = Invoke-CimMethod -InputObject $registered -MethodName Change -Arguments @{ PathName = $binPath; StartMode = 'Automatic'; StartName = 'LocalSystem' }
    if ($changed.ReturnValue -ne 0) { throw "Service update failed: $($changed.ReturnValue)" }
} else {
    New-Service -Name NexusAgent -BinaryPathName $binPath -StartupType Automatic -DisplayName 'Nexus Remote Agent' -Description 'Authorized remote control from your paired devices, including UAC and login.' | Out-Null
}
Invoke-ServiceCommand @('failure', 'NexusAgent', 'reset=', '86400', 'actions=', 'restart/5000/restart/5000/restart/5000')
Invoke-ServiceCommand @('failureflag', 'NexusAgent', '1')
$policyPath = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Policies\System'
$policy = Get-ItemProperty -LiteralPath $policyPath -Name SoftwareSASGeneration -ErrorAction SilentlyContinue
if ($null -eq $policy) { New-ItemProperty -LiteralPath $policyPath -Name SoftwareSASGeneration -Value 1 -PropertyType DWord | Out-Null }
foreach ($rule in @('NexusRemoteHostLAN', 'NexusRemoteDiscoveryLAN', 'NexusRemoteSignalLAN')) {
    Get-NetFirewallRule -Name $rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
New-NetFirewallRule -Name NexusRemoteHostLAN -DisplayName 'Nexus Remote: encrypted LAN control' -Direction Inbound -Action Allow -Program $agent -Protocol UDP -RemoteAddress LocalSubnet -Profile Any | Out-Null
New-NetFirewallRule -Name NexusRemoteDiscoveryLAN -DisplayName 'Nexus Remote: LAN discovery' -Direction Inbound -Action Allow -Program $agent -Protocol UDP -LocalPort 5353 -RemoteAddress LocalSubnet -Profile Any | Out-Null
if ($localSignal) {
    $signal = Join-Path $install 'nexus-signal.exe'
    $signalState = Join-Path $state 'signal'
    New-Item -ItemType Directory -Force -Path $signalState | Out-Null
    $database = 'sqlite://' + (Join-Path $signalState 'pairings.db').Replace('\', '/')
    $args = '--listen {0}:{1} --audience {0}:{1} --database-url "{2}" --log-dir "{3}"' -f $LanAddress, $SignalPort, $database, (Join-Path $signalState 'logs')
    $action = New-ScheduledTaskAction -Execute $signal -Argument $args -WorkingDirectory $install
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $owner = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName NexusSignal -Action $action -Trigger $trigger -Principal $owner -Settings $settings -Force | Out-Null
    New-NetFirewallRule -Name NexusRemoteSignalLAN -DisplayName 'Nexus Remote: home-network pairing' -Direction Inbound -Action Allow -Program $signal -Protocol TCP -LocalPort $SignalPort -RemoteAddress LocalSubnet -Profile Any | Out-Null
    Start-ScheduledTask -TaskName NexusSignal
}
Start-Service NexusAgent
if ($KeepAwakeOnAc) {
    $powerBackup = Join-Path $state 'power-settings-before.txt'
    if (-not (Test-Path -LiteralPath $powerBackup)) {
        & powercfg.exe /query SCHEME_CURRENT SUB_SLEEP | Set-Content -LiteralPath $powerBackup -Encoding UTF8
        if ($LASTEXITCODE -ne 0) { throw 'Could not save the current sleep settings.' }
    }
    & powercfg.exe /change standby-timeout-ac 0
    if ($LASTEXITCODE -ne 0) { throw 'Could not keep the host awake on mains power.' }
    & powercfg.exe /change hibernate-timeout-ac 0
    if ($LASTEXITCODE -ne 0) { throw 'Could not disable timed hibernation on mains power.' }
}
$cli = Join-Path $install 'nexus.exe'
$ready = $false
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $status = & $cli --lang en --system status 2>$null
        if ($LASTEXITCODE -eq 0 -and ($status -join "`n") -match 'connected \(') { $ready = $true; break }
    } catch {
        # The session child may not have published its IPC token yet.
    }
    Start-Sleep -Seconds 1
}
if (-not $ready) { throw "Host has not connected to signaling. Inspect $state\logs and $state\signal\logs." }
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if (($machinePath -split ';') -notcontains $install) { [Environment]::SetEnvironmentVariable('Path', ($machinePath.TrimEnd(';') + ';' + $install), 'Machine') }
Write-Host "Windows host ready. Mac signaling URL: $($SignalUrl.AbsoluteUri)"
& $cli --system status
if ($LASTEXITCODE -ne 0) { throw 'Installed host status check failed.' }
Write-Host 'Next, in an Administrator terminal: nexus --system pair offer'
Write-Host 'After pairing, enroll a passkey/TOTP and pre-authorise only your MacBook for lock-screen access.'

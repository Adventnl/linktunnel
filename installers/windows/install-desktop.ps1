<# One-time elevated installation. Everyday shortcuts launch the GUI directly as the desktop owner. #>
[CmdletBinding()]
param(
    [string]$BundleDirectory = $PSScriptRoot,
    [string]$InstallDirectory = (Join-Path $env:ProgramFiles 'linktunnel'),
    [uri]$SignalUrl,
    [switch]$AllowInsecureSignal,
    [switch]$InPlace,
    [switch]$NoShortcuts
)
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]::new($identity)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run setup once as Administrator. The installed app runs without elevation.'
}
$ownerSid = $identity.User.Value
if ($ownerSid -in @('S-1-5-18','S-1-5-19','S-1-5-20')) { throw 'Run setup from the desktop account, not a system account.' }
$state = Join-Path $env:ProgramData 'Nexus'
$config = Join-Path $state 'config\config.toml'
foreach ($name in @('nexus-agent.exe','nexus.exe','nexus-app.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $BundleDirectory $name))) { throw "Missing $name in the setup bundle." }
}
if (-not (Test-Path -LiteralPath $config)) {
    if (-not $SignalUrl -or $SignalUrl.Scheme -notin @('ws','wss') -or $SignalUrl.UserInfo -or
        $SignalUrl.AbsolutePath -ne '/v1/ws' -or $SignalUrl.Query -or $SignalUrl.Fragment) {
        throw 'A new host needs -SignalUrl ws[s]://host[:port]/v1/ws.'
    }
    if ($SignalUrl.Scheme -eq 'ws' -and -not $AllowInsecureSignal) { throw 'Plain WS needs -AllowInsecureSignal for your trusted LAN/VPN.' }
}
foreach ($path in @($InstallDirectory,$state)) {
    if ((Test-Path -LiteralPath $path) -and ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Installation directory cannot be a reparse point: $path"
    }
}
$service = Get-Service NexusAgent -ErrorAction SilentlyContinue
if ($service) {
    Stop-Service NexusAgent
    $service.WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30))
}
try {
    New-Item -ItemType Directory -Force -Path $InstallDirectory, (Join-Path $state 'config'), (Join-Path $state 'run') | Out-Null
    if (-not $InPlace) {
        $stamp = Get-Date -Format yyyyMMdd-HHmmss
        foreach ($name in @('nexus-agent.exe','nexus.exe','nexus-app.exe')) {
            $destination = Join-Path $InstallDirectory $name
            if (Test-Path -LiteralPath $destination) { Copy-Item -LiteralPath $destination -Destination "$destination.before-$stamp" }
            Copy-Item -LiteralPath (Join-Path $BundleDirectory $name) -Destination $destination -Force
        }
    }
    if (-not (Test-Path -LiteralPath $config)) {
        $insecure = if ($AllowInsecureSignal) {'true'} else {'false'}
        $logs = (Join-Path $state 'logs').Replace('\','/')
        @"
[net]
signal_url = "$($SignalUrl.AbsoluteUri)"
allow_insecure_signal = $insecure
lan_discovery = true
[security]
consent = "unattended"
unattended_requires_stepup = false
[terminal]
enabled = false
[log]
ansi = false
file_dir = "$logs"
"@ | Set-Content -LiteralPath $config -Encoding ASCII
    }
    # The service resets the state ACL, then grants this SID read-only runtime access.
    # Identity keys, configuration and trust remain SYSTEM/Administrators only.
    [IO.File]::WriteAllText((Join-Path $state 'run\desktop-user.sid'), $ownerSid, [Text.UTF8Encoding]::new($false))
    $agent = Join-Path $InstallDirectory 'nexus-agent.exe'
    $binPath = '"{0}" --windows-service' -f $agent
    if ($service) {
        $registered = Get-CimInstance Win32_Service -Filter "Name='NexusAgent'"
        $changed = Invoke-CimMethod -InputObject $registered -MethodName Change -Arguments @{PathName=$binPath;StartMode='Automatic'}
        if ($changed.ReturnValue -ne 0) { throw "Cannot update the agent service ($($changed.ReturnValue))." }
    } else {
        New-Service -Name NexusAgent -BinaryPathName $binPath -DisplayName 'Link Tunnel Agent' -StartupType Automatic | Out-Null
    }
    # Keep existing LAN/VPN firewall scopes when an older install used another directory.
    Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object Name -Like 'NexusRemoteHost*' |
        Get-NetFirewallApplicationFilter | Set-NetFirewallApplicationFilter -Program $agent | Out-Null
    if (-not (Get-NetFirewallRule -Name NexusRemoteHostLAN -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name NexusRemoteHostLAN -DisplayName 'Link Tunnel: encrypted LAN control' -Direction Inbound -Action Allow -Program $agent -Protocol UDP -RemoteAddress LocalSubnet -Profile Any | Out-Null
    }
    Start-Service NexusAgent
    if (-not $NoShortcuts) {
        $shell = New-Object -ComObject WScript.Shell
        $menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Link Tunnel'
        New-Item -ItemType Directory -Force -Path $menu | Out-Null
        foreach ($path in @((Join-Path ([Environment]::GetFolderPath('Desktop')) 'linktunnel.lnk'), (Join-Path $menu 'Link Tunnel.lnk'))) {
            # Recreate old shortcuts so a legacy "Run as administrator" flag is cleared too.
            if (Test-Path -LiteralPath $path) {
                Copy-Item -LiteralPath $path -Destination ($path + '.before-desktop-setup') -Force
                Remove-Item -LiteralPath $path
            }
            $shortcut = $shell.CreateShortcut($path)
            $shortcut.TargetPath = Join-Path $InstallDirectory 'nexus-app.exe'
            $shortcut.Arguments = '--system'
            $shortcut.WorkingDirectory = $InstallDirectory
            $shortcut.IconLocation = (Join-Path $InstallDirectory 'nexus-app.exe') + ',0'
            $shortcut.Description = 'Link Tunnel'
            $shortcut.Save()
        }
    }
    Write-Host "Link Tunnel installed at $InstallDirectory. Open the app from its Desktop or Start menu shortcut."
} catch {
    if ($service -and (Get-Service NexusAgent).Status -ne 'Running') { Start-Service NexusAgent -ErrorAction SilentlyContinue }
    throw
}

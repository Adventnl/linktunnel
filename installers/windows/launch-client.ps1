<# Reuse this PC's installed host identity, or start an ordinary per-user controller. #>
param([uri]$SignalUrl, [switch]$AllowInsecureSignal)
$ErrorActionPreference = 'Stop'
$app = Join-Path $PSScriptRoot 'nexus-app.exe'
$cli = Join-Path $PSScriptRoot 'nexus.exe'
$agent = Join-Path $PSScriptRoot 'nexus-agent.exe'
foreach ($file in @($app, $cli, $agent)) {
    if (-not (Test-Path -LiteralPath $file)) { throw "Extract the complete Windows ZIP first; missing $file" }
}
function Test-LinkTunnelAgent {
    # Windows PowerShell 5.1 turns native stderr into terminating errors under
    # Stop, even when redirected. An absent agent is expected on first launch.
    $ErrorActionPreference = 'Continue'
    & $cli status *> $null
    return $LASTEXITCODE -eq 0
}
$service = Get-Service NexusAgent -ErrorAction SilentlyContinue
if ($service -and $service.Status -ne 'Running') {
    throw 'The installed Link Tunnel host is stopped. Run Start-Service NexusAgent in Administrator PowerShell, then open LinkTunnel again.'
}
if ($service -and $service.Status -eq 'Running') {
    if ($SignalUrl) { throw 'This PC already has a running host. Its signaling configuration is managed by that host.' }
    # The service state is restricted to Administrators. Use its identity and pairings,
    # not an empty per-user dashboard which would make existing peers disappear.
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    $enrolled = Test-Path -LiteralPath (Join-Path $env:ProgramData 'Nexus\run\desktop-user.sid')
    if ($enrolled -or $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Start-Process -FilePath $app -ArgumentList '--system'
    } else {
        Start-Process -FilePath $app -ArgumentList '--system' -Verb RunAs
    }
    return
}
if (-not (Test-LinkTunnelAgent)) {
    $settings = Join-Path $env:LOCALAPPDATA 'linktunnel\client-settings.json'
    if (-not $SignalUrl -and (Test-Path -LiteralPath $settings)) {
        $saved = Get-Content -LiteralPath $settings -Raw | ConvertFrom-Json
        $SignalUrl = [uri]$saved.signalUrl
        $AllowInsecureSignal = [bool]$saved.allowInsecureSignal
    }
    if (-not $SignalUrl) {
        $SignalUrl = [uri](Read-Host 'Signaling URL used by your Mac/Linux (ws[s]://host:port/v1/ws)')
        if ($SignalUrl.Scheme -eq 'ws') {
            $answer = Read-Host 'Plain ws is for a trusted LAN or VPN. Allow it? [y/N]'
            $AllowInsecureSignal = $answer -eq 'y'
        }
    }
    if (-not $SignalUrl.IsAbsoluteUri -or $SignalUrl.Scheme -notin @('ws', 'wss') -or
        $SignalUrl.UserInfo -or $SignalUrl.AbsolutePath -ne '/v1/ws' -or $SignalUrl.Query -or $SignalUrl.Fragment) {
        throw 'Use ws[s]://host[:port]/v1/ws without credentials.'
    }
    if ($SignalUrl.Scheme -eq 'ws' -and -not $AllowInsecureSignal) { throw 'Plain ws requires -AllowInsecureSignal on a trusted LAN/VPN.' }
    $agentArgs = @('--no-screen', '--signal-url', $SignalUrl.AbsoluteUri)
    if ($AllowInsecureSignal) { $agentArgs += '--allow-insecure-signal' }
    # A URI has no shell quoting characters after validation/URI escaping. Start-Process
    # runs the executable directly; its window stays hidden while the tray app is open.
    $child = Start-Process -FilePath $agent -ArgumentList $agentArgs -WindowStyle Hidden -PassThru
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        Start-Sleep -Milliseconds 500
        if (Test-LinkTunnelAgent) { $ready = $true; break }
        if ($child.HasExited) { throw 'The controller agent exited. Run nexus-agent.exe in PowerShell to see the error.' }
    }
    if (-not $ready) { throw 'Timed out starting the controller agent. Run nexus.exe doctor for details.' }
    New-Item -ItemType Directory -Force -Path (Split-Path $settings) | Out-Null
    @{ signalUrl = $SignalUrl.AbsoluteUri; allowInsecureSignal = [bool]$AllowInsecureSignal } |
        ConvertTo-Json | Set-Content -LiteralPath $settings -Encoding UTF8
}
Start-Process -FilePath $app

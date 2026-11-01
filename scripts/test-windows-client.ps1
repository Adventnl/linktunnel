<# Verify the packaged launcher against a real isolated agent, without opening app/UAC windows. #>
param([string]$BundleDirectory = "$PSScriptRoot\..\dist\windows-client")
$ErrorActionPreference = 'Stop'
$bundle = (Resolve-Path $BundleDirectory).Path
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('linktunnel-client-test-' + [guid]::NewGuid().ToString('N'))
$oldHome = $env:NEXUS_HOME
$oldLocal = $env:LOCALAPPDATA
$clientTestState = [pscustomobject]@{ Launches = [System.Collections.Generic.List[object]]::new(); HostStatus = $null; AgentProcess = $null }
function Read-Host { param($Prompt) throw "Unexpected interactive prompt: $Prompt" }
function Stop-TestAgent { $ErrorActionPreference = 'Continue'; & "$bundle\nexus.exe" stop *> $null }
function Get-Service { param($Name, $ErrorAction) if ($clientTestState.HostStatus) { [pscustomobject]@{ Status = $clientTestState.HostStatus } } }
function Start-Process {
    param($FilePath, $ArgumentList, $WindowStyle, [switch]$PassThru, $Verb)
    $clientTestState.Launches.Add([pscustomobject]@{ File = $FilePath; Arguments = $ArgumentList; Verb = $Verb })
    if ((Split-Path $FilePath -Leaf) -eq 'nexus-agent.exe') {
        $clientTestState.AgentProcess = Microsoft.PowerShell.Management\Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -WindowStyle Hidden -PassThru
        return $clientTestState.AgentProcess
    }
}
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
try {
    $env:NEXUS_HOME = Join-Path $testRoot 'identity'
    $env:LOCALAPPDATA = Join-Path $testRoot 'local'
    $rejected = $false
    try { & "$bundle\launch-client.ps1" -SignalUrl https://signal.example/v1/ws } catch { $rejected = $true }
    Assert $rejected 'Invalid signaling URLs must be rejected before starting an agent.'
    Assert ($clientTestState.Launches.Count -eq 0) 'Invalid URLs must not launch processes.'
    & "$bundle\launch-client.ps1" -SignalUrl ws://127.0.0.1:9/v1/ws -AllowInsecureSignal
    Assert ($clientTestState.Launches.Count -eq 2) 'First launch must start one agent and one dashboard.'
    Assert ($clientTestState.Launches[0].Arguments -contains '--no-screen') 'New controller must not share its screen.'
    Assert (Test-Path -LiteralPath "$env:LOCALAPPDATA\linktunnel\client-settings.json") 'First-launch settings not saved.'
    & "$bundle\launch-client.ps1"
    Assert ($clientTestState.Launches.Count -eq 3) 'Relaunch must reuse the running user agent.'
    Stop-TestAgent
    Assert ($clientTestState.AgentProcess.WaitForExit(10000)) 'Test agent did not stop.'
    & "$bundle\launch-client.ps1"
    Assert ($clientTestState.Launches.Count -eq 5) 'Saved settings must restart the controller without prompting.'
    $clientTestState.HostStatus = 'Running'
    & "$bundle\launch-client.ps1"
    Assert ($clientTestState.Launches.Count -eq 6) 'Host launch should open the dashboard only.'
    Assert ($clientTestState.Launches[-1].Arguments -eq '--system') 'Installed host must retain its system identity.'
    $rejected = $false
    try { & "$bundle\launch-client.ps1" -SignalUrl wss://other.example/v1/ws } catch { $rejected = $true }
    Assert $rejected 'A launcher must not change an installed host signaling server.'
    $clientTestState.HostStatus = 'Stopped'
    $rejected = $false
    try { & "$bundle\launch-client.ps1" } catch { $rejected = $true }
    Assert $rejected 'A stopped system host must not silently switch to an empty user identity.'
    Assert ($clientTestState.Launches.Count -eq 6) 'Stopped host must not launch another agent.'
    Write-Host 'PASS: real user-agent startup/reuse/settings and system-host selection.'
} finally {
    Stop-TestAgent
    if ($clientTestState.AgentProcess -and -not $clientTestState.AgentProcess.HasExited) {
        $clientTestState.AgentProcess.WaitForExit(10000) | Out-Null
        if (-not $clientTestState.AgentProcess.HasExited) { $clientTestState.AgentProcess.Kill() }
    }
    $env:NEXUS_HOME = $oldHome
    $env:LOCALAPPDATA = $oldLocal
    # Delete only the uniquely named directory whose resolved location we checked.
    if ((Test-Path -LiteralPath $testRoot) -and
        (Split-Path (Resolve-Path $testRoot).Path -Parent) -eq ([IO.Path]::GetTempPath()).TrimEnd('\')) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}

<#
.SYNOPSIS
Tests two real Windows agents, pairing, encrypted QUIC, and physical-desktop H.264 capture.
.DESCRIPTION
Uses isolated test state under target, never the installed host identity/configuration.
Saves a screenshot of the current desktop locally under target/windows-smoke-*.
Does not inject keyboard/mouse events or change the installed service.
#>
param([string]$BinDirectory = "$PSScriptRoot\..\target\release")
$ErrorActionPreference = 'Stop'
$bin = (Resolve-Path -LiteralPath $BinDirectory).Path
$root = (Resolve-Path "$PSScriptRoot\..").Path
$work = Join-Path $root ('target\windows-smoke-' + [guid]::NewGuid().ToString('N'))
$pc = Join-Path $work 'pc'
$mac = Join-Path $work 'controller'
$processes = [Collections.Generic.List[Diagnostics.Process]]::new()
New-Item -ItemType Directory -Force -Path (Join-Path $pc 'config'), (Join-Path $mac 'config') | Out-Null
# Explicitly relaxed only in disposable test homes, as in the existing Linux demo.
$testConfig = @'
[security]
unattended_requires_stepup = false
[audio]
enabled = false
[clipboard]
send = false
receive = false
[terminal]
enabled = false
[net]
lan_discovery = false
'@
$testConfig | Set-Content -LiteralPath (Join-Path $pc 'config\config.toml') -Encoding ASCII
$testConfig | Set-Content -LiteralPath (Join-Path $mac 'config\config.toml') -Encoding ASCII
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = $listener.LocalEndpoint.Port
$listener.Stop()
$signalUrl = "ws://127.0.0.1:$port/v1/ws"
$cli = Join-Path $bin 'nexus.exe'
function Start-TestProcess([string]$Name, [string]$Executable, [string]$Arguments) {
    $process = Start-Process -FilePath (Join-Path $bin $Executable) -ArgumentList $Arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $work "$Name.out") -RedirectStandardError (Join-Path $work "$Name.err")
    # Retain the native handle so Windows PowerShell can read ExitCode after a fast exit.
    $null = $process.Handle
    $processes.Add($process)
    return $process
}
function Invoke-TestCli([string]$AgentHome, [string[]]$Arguments) {
    & $cli --lang en --home $AgentHome @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Test CLI failed: $($Arguments -join ' ')" }
}
function Wait-TestReady([string]$AgentHome) {
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        try {
            $status = & $cli --lang en --home $AgentHome status 2>$null
            if ($LASTEXITCODE -eq 0 -and ($status -join "`n") -match 'connected \(') { return }
        } catch { }
        Start-Sleep -Milliseconds 250
    }
    throw "Test agent did not reach signaling; inspect $work"
}
try {
    $null = Start-TestProcess signal nexus-signal.exe "--listen 127.0.0.1:$port --audience 127.0.0.1:$port"
    $null = Start-TestProcess pc nexus-agent.exe ('--home "{0}" --name Smoke-PC --file-store --signal-url {1} --no-terminal' -f $pc, $signalUrl)
    $null = Start-TestProcess controller nexus-agent.exe ('--home "{0}" --name Smoke-Controller --file-store --signal-url {1} --no-screen --no-terminal' -f $mac, $signalUrl)
    Wait-TestReady $pc
    Wait-TestReady $mac
    $offer = Start-TestProcess offer nexus.exe ('--lang en --home "{0}" pair offer --assume-words-match' -f $pc)
    $code = $null
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        $offerText = Get-Content -LiteralPath (Join-Path $work 'offer.out') -Raw -ErrorAction SilentlyContinue
        if ($offerText -match 'nexus pair join "([^"]+)"') { $code = $Matches[1]; break }
        Start-Sleep -Milliseconds 250
    }
    if (-not $code) { throw "No pairing offer; inspect $work" }
    Invoke-TestCli $mac @('pair', 'join', $code, '--assume-words-match')
    if (-not $offer.WaitForExit(10000) -or $offer.ExitCode -ne 0) { throw 'The target did not complete pairing.' }
    Invoke-TestCli $pc @('trust', 'unattended', 'Smoke-Controller')
    Invoke-TestCli $mac @('remote', 'monitors', 'Smoke-PC')
    $screenshot = Join-Path $work 'desktop.png'
    Invoke-TestCli $mac @('remote', 'screenshot', 'Smoke-PC', '-o', $screenshot, '--cursor')
    if ((Get-Item -LiteralPath $screenshot).Length -lt 1024) { throw 'Desktop screenshot is unexpectedly small.' }
    Add-Type -AssemblyName System.Drawing
    $picture = [Drawing.Bitmap]::FromFile($screenshot)
    try {
        $visibleSamples = 0
        for ($row = 0; $row -lt 24; $row++) {
            for ($column = 0; $column -lt 32; $column++) {
                $pixel = $picture.GetPixel([int](($column + 0.5) * $picture.Width / 32), [int](($row + 0.5) * $picture.Height / 24))
                if ($pixel.R + $pixel.G + $pixel.B -gt 24) { $visibleSamples++ }
            }
        }
        if ($visibleSamples -lt 16) { throw 'The remote picture is blank; test against an unlocked desktop with a visible app.' }
    } finally { $picture.Dispose() }
    Invoke-TestCli $mac @('ping', 'Smoke-PC', '--json')
    Invoke-TestCli $pc @('audit', 'verify')
    Write-Host "Physical-desktop transport smoke test passed. Evidence: $work"
} finally {
    foreach ($process in $processes) {
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -ErrorAction SilentlyContinue }
    }
}

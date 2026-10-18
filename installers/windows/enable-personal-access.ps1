<#
.SYNOPSIS
One-time migration for an already installed personal PC: paired devices connect without prompts.
.EXAMPLE
./enable-personal-access.ps1 -Controller MacBook
#>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Controller)
$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this migration from an Administrator PowerShell window.'
}
$config = Join-Path $env:ProgramData 'Nexus\config\config.toml'
$cli = Join-Path $env:ProgramFiles 'Nexus Remote\nexus.exe'
if (-not (Test-Path -LiteralPath $config) -or -not (Test-Path -LiteralPath $cli)) {
    throw 'Install the Nexus Remote Windows host first.'
}
# Validate and pre-authorise the selected paired controller through the running host.
& $cli --system trust unattended $Controller
if ($LASTEXITCODE -ne 0) { throw 'Could not authorise the paired controller; configuration was not changed.' }
$text = Get-Content -LiteralPath $config -Raw
$section = [regex]::Match($text, '(?ms)^\[security\][^\r\n]*\r?\n(?<body>.*?)(?=^\[|\z)')
$body = if ($section.Success) { $section.Groups['body'].Value } else { '' }
foreach ($entry in @(@('consent', '"unattended"'), @('unattended_requires_stepup', 'false'))) {
    $pattern = '(?m)^\s*' + $entry[0] + '\s*=[^\r\n]*'
    $line = $entry[0] + ' = ' + $entry[1]
    if ($body -match $pattern) { $body = [regex]::Replace($body, $pattern, $line) }
    else { $body = $body.TrimEnd() + "`r`n" + $line + "`r`n" }
}
$newSection = "[security]`r`n" + $body.Trim() + "`r`n`r`n"
if ($section.Success) { $text = $text.Remove($section.Index, $section.Length).Insert($section.Index, $newSection) }
else { $text = $text.TrimEnd() + "`r`n`r`n" + $newSection }
$backup = $config + '.before-personal-access-' + (Get-Date -Format 'yyyyMMdd-HHmmss')
Copy-Item -LiteralPath $config -Destination $backup
[IO.File]::WriteAllText($config, $text, [Text.UTF8Encoding]::new($false))
Restart-Service NexusAgent
(Get-Service NexusAgent).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
Write-Host "Ready: open Nexus Remote on $Controller and press Connect. Configuration backup: $backup"

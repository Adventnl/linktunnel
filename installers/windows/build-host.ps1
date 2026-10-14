<# Builds a self-contained Windows host folder; no WiX or .NET installation required. #>
param([switch]$SkipBuild, [switch]$HardwareFfmpeg)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
if (-not $SkipBuild) {
    . "$root\scripts\windows-build-env.ps1"
    Push-Location $root
    try {
        $buildArgs = @('build', '--locked', '--release', '-p', 'nexus-agent', '-p', 'nexus-cli', '-p', 'nexus-signal')
        if ($HardwareFfmpeg) { $buildArgs += @('--features', 'nexus-agent/hardware-ffmpeg') }
        & cargo @buildArgs
        if ($LASTEXITCODE -ne 0) { throw 'Native Rust build failed.' }
    } finally { Pop-Location }
}
$out = Join-Path $root 'target\windows-host'
New-Item -ItemType Directory -Force -Path $out | Out-Null
foreach ($name in @('nexus-agent.exe', 'nexus.exe', 'nexus-signal.exe')) {
    Copy-Item -LiteralPath (Join-Path $root "target\release\$name") -Destination $out -Force
}
$app = Join-Path $root 'apps\nexus-app\src-tauri\target\release\nexus-app.exe'
if (Test-Path -LiteralPath $app) { Copy-Item -LiteralPath $app -Destination $out -Force }
Copy-Item -LiteralPath "$PSScriptRoot\install-host.ps1" -Destination $out -Force
Copy-Item -LiteralPath "$root\docs\MAC_TO_WINDOWS.md" -Destination $out -Force
Get-ChildItem -LiteralPath $out -Filter '*.exe' | Get-FileHash -Algorithm SHA256 | ForEach-Object {
    '{0}  {1}' -f $_.Hash.ToLowerInvariant(), (Split-Path $_.Path -Leaf)
} | Set-Content -LiteralPath (Join-Path $out 'SHA256SUMS.txt') -Encoding ASCII
Write-Host "Host bundle ready: $out"

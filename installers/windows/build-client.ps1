<# Builds a portable Windows controller with the native desktop viewer and agent. #>
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
if (-not $SkipBuild) {
    . "$root\scripts\windows-build-env.ps1"
    Push-Location $root
    try {
        & cargo build --locked --release -p nexus-agent -p nexus-cli
        if ($LASTEXITCODE -ne 0) { throw 'Agent/CLI build failed.' }
        & npm.cmd ci --no-audit --no-fund --prefix apps/nexus-app
        if ($LASTEXITCODE -ne 0) { throw 'Frontend dependency installation failed.' }
        & npm.cmd run build --prefix apps/nexus-app
        if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
        & cargo build --locked --release --manifest-path apps/nexus-app/src-tauri/Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed.' }
    } finally { Pop-Location }
}
$out = Join-Path $root 'dist\windows-client'
New-Item -ItemType Directory -Force -Path $out | Out-Null
foreach ($name in @('nexus.exe', 'nexus-agent.exe')) {
    Copy-Item -LiteralPath (Join-Path $root "target\release\$name") -Destination $out -Force
}
Copy-Item -LiteralPath "$root\apps\nexus-app\src-tauri\target\release\nexus-app.exe" -Destination $out -Force
foreach ($name in @('launch-client.ps1', 'LinkTunnel.cmd')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $out -Force
}
Copy-Item -LiteralPath "$root\docs\WINDOWS_CLIENT.md" -Destination "$out\README.md" -Force
Get-ChildItem -LiteralPath $out -File | Where-Object Name -NE 'SHA256SUMS.txt' | Get-FileHash -Algorithm SHA256 | ForEach-Object {
    '{0}  {1}' -f $_.Hash.ToLowerInvariant(), (Split-Path $_.Path -Leaf)
} | Set-Content -LiteralPath "$out\SHA256SUMS.txt" -Encoding ASCII
$zip = Join-Path $root 'dist\linktunnel-windows-x64.zip'
# List the distribution files explicitly; never ship locally saved client settings.
$files = @('nexus.exe', 'nexus-agent.exe', 'nexus-app.exe', 'launch-client.ps1', 'LinkTunnel.cmd', 'README.md', 'SHA256SUMS.txt')
Compress-Archive -LiteralPath ($files | ForEach-Object { Join-Path $out $_ }) -DestinationPath $zip -Force
Write-Host "Windows controller ready: $zip"

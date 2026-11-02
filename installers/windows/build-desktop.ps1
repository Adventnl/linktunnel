<# Build a real Windows setup executable, including the background host and native viewer. #>
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
. "$root\scripts\windows-build-env.ps1"
Push-Location $root
try {
    if (-not $SkipBuild) {
        & cargo build --locked --release -p nexus-agent -p nexus-cli
        if ($LASTEXITCODE -ne 0) { throw 'Host/viewer build failed.' }
        & npm.cmd run build --prefix apps/nexus-app
        if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
    }
    $binaries = Join-Path $root 'apps\nexus-app\src-tauri\binaries'
    New-Item -ItemType Directory -Force -Path $binaries | Out-Null
    foreach ($name in @('nexus','nexus-agent')) {
        Copy-Item -LiteralPath "$root\target\release\$name.exe" -Destination "$binaries\$name-x86_64-pc-windows-msvc.exe" -Force
    }
    Push-Location "$root\apps\nexus-app"
    try {
        # Viewer preparation above uses the same build as the agent, avoiding a second feature graph.
        & npm.cmd exec -- tauri build --bundles nsis --config src-tauri/tauri.setup.conf.json
        if ($LASTEXITCODE -ne 0) { throw 'Windows setup build failed.' }
    } finally { Pop-Location }
    $out = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    Get-ChildItem "$root\apps\nexus-app\src-tauri\target\release\bundle\nsis" -Filter '*setup.exe' |
        Copy-Item -Destination $out -Force
} finally { Pop-Location }

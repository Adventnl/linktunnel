<# Build Link Tunnel itself with the Windows identity/capabilities needed by native HFP calls.
   Does not install a package, trust a certificate, or change any Windows settings.
   Signing uses an existing CurrentUser\My certificate; no private keys enter the repo. #>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$AppExecutable,
    [string]$SigningCertificateThumbprint,
    [string]$Publisher = 'CN=LinkTunnel',
    [string]$Version = '0.1.0.0'
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$app = Join-Path $root 'apps\nexus-app'
$sdk = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Directory |
    Where-Object { $_.Name -match '^10\.' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'x64\makeappx.exe')) } |
    Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
if (-not $sdk) { throw 'Install a Windows 10/11 SDK with MakeAppx and SignTool.' }
if ($SigningCertificateThumbprint) {
    if ($SigningCertificateThumbprint -notmatch '^[0-9a-fA-F]{40}$') { throw 'Invalid signing certificate thumbprint.' }
    $certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$SigningCertificateThumbprint"
    if (-not $certificate.HasPrivateKey -or $certificate.Subject -ne $Publisher -or $certificate.NotAfter -le (Get-Date)) {
        throw 'The signing certificate must have a private key, match Publisher, and be unexpired.'
    }
}
if (-not $SkipBuild) {
    . "$root\scripts\windows-build-env.ps1"
    $AppExecutable = Join-Path $root 'target\calls-msix-app.exe'
    Push-Location $app
    try {
        & npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw 'Call-enabled frontend build failed.' }
        # A distinct output keeps the currently running desktop app intact.
        & cargo rustc --manifest-path src-tauri/Cargo.toml --release --bin nexus-app -- -o $AppExecutable
        if ($LASTEXITCODE -ne 0) { throw 'Call-enabled desktop build failed.' }
    } finally { Pop-Location }
}
$release = Join-Path $app 'src-tauri\target\release'
$stage = Join-Path $root ('target\calls-msix\' + [guid]::NewGuid().ToString('N'))
$assets = Join-Path $stage 'Assets'
New-Item -ItemType Directory -Force -Path $assets | Out-Null
foreach ($name in @('nexus-app.exe','nexus.exe','nexus-agent.exe')) {
    $source = Join-Path $release $name
    if ($name -eq 'nexus-app.exe' -and $AppExecutable) { $source = (Resolve-Path -LiteralPath $AppExecutable).Path }
    if (-not (Test-Path -LiteralPath $source)) { throw "Missing $source. Build the Windows desktop bundle first." }
    Copy-Item -LiteralPath $source -Destination (Join-Path $stage $name)
}
$phoneTools = Join-Path $app 'src-tauri\resources\phone-tools'
if (-not (Test-Path -LiteralPath $phoneTools)) { throw 'Bundled Android tools are missing. Run npm run prepare:phone.' }
Copy-Item -LiteralPath $phoneTools -Destination (Join-Path $stage 'phone-tools') -Recurse

# Reuse the app's existing icon. Only size conversion, no new visual asset.
Add-Type -AssemblyName System.Drawing
$image = [Drawing.Image]::FromFile((Join-Path $app 'src-tauri\icons\128x128@2x.png'))
try {
    foreach ($asset in @(@('StoreLogo.png',50),@('Square44x44Logo.png',44),@('Square150x150Logo.png',150))) {
        $bitmap = [Drawing.Bitmap]::new([int]$asset[1],[int]$asset[1])
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.DrawImage($image,0,0,[int]$asset[1],[int]$asset[1])
            $bitmap.Save((Join-Path $assets $asset[0]),[Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
} finally { $image.Dispose() }
[xml]$manifest = Get-Content -LiteralPath "$PSScriptRoot\calls\AppxManifest.xml" -Raw
$manifest.Package.Identity.Publisher = $Publisher
$manifest.Package.Identity.Version = ([version]$Version).ToString(4)
$manifest.Save((Join-Path $stage 'AppxManifest.xml'))
$out = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$suffix = if ($SigningCertificateThumbprint) { '' } else { '-unsigned' }
$package = Join-Path $out "LinkTunnel-Calls-$Version-x64$suffix.msix"
$packLog = "$stage.makeappx.log"
& (Join-Path $sdk.FullName 'x64\makeappx.exe') pack /o /d $stage /p $package *> $packLog
if ($LASTEXITCODE -ne 0) { throw "MSIX schema or payload validation failed. See $packLog" }
if ($SigningCertificateThumbprint) {
    & (Join-Path $sdk.FullName 'x64\signtool.exe') sign /fd SHA256 /sha1 $SigningCertificateThumbprint /s My $package
    if ($LASTEXITCODE -ne 0) { throw 'MSIX signing failed.' }
    & (Join-Path $sdk.FullName 'x64\signtool.exe') verify /pa $package
    if ($LASTEXITCODE -ne 0) { throw 'MSIX signature verification failed. The target must trust its signing certificate.' }
    Write-Host "Signed call-enabled package: $package"
} else {
    Write-Host "Unsigned review/build artifact: $package"
    Write-Host 'This artifact cannot be installed until signed with a certificate trusted by the target Windows account.'
}
Write-Host "Development directory (requires existing Developer Mode): $stage"
Set-Content -LiteralPath (Join-Path $out 'LinkTunnel-Calls-development-directory.txt') -Value $stage -Encoding UTF8

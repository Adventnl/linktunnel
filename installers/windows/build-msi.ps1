<#
.SYNOPSIS  Builds (and optionally signs) linktunnel.msi.
.DESCRIPTION
  Needs: Rust, WiX Toolset v4 (`dotnet tool install --global wix`; `wix extension add -g WixToolset.Firewall.wixext`, `wix extension add -g WixToolset.Util.wixext`),
  and for signing the Windows SDK's signtool plus a code-signing certificate.
  Signing is optional for your own machines (SmartScreen will warn); it is required to distribute.
  Binaries do NOT need to be signed for the SYSTEM service to reach the UAC/lock-screen desktop
  (that is a property of running as SYSTEM, not of the signature); only UIAccess would, and we do not use it.
.PARAMETER CertThumbprint  SHA-1 thumbprint of the code-signing certificate in the store (omit to skip signing).
#>
param(
  [string]$CertThumbprint = "",
  [string]$TimestampUrl = "http://timestamp.digicert.com",
  [switch]$HardwareFfmpeg
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"(.*)"' | Select-Object -First 1).Matches.Groups[1].Value
Push-Location $root
try {
  $buildArgs = @("build", "--locked", "--profile", "dist", "-p", "nexus-agent", "-p", "nexus-cli")
  if ($HardwareFfmpeg) { $buildArgs += @("--features", "nexus-agent/hardware-ffmpeg") }
  & cargo @buildArgs
  if ($LASTEXITCODE -ne 0) { throw "Rust build failed" }
} finally { Pop-Location }
$bin = "$root\target\dist"
function Sign($file) {
  if ($CertThumbprint) {
    signtool sign /sha1 $CertThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 /d "linktunnel" $file
    if ($LASTEXITCODE -ne 0) { throw "signing $file failed" }
  }
}
Sign "$bin\nexus-agent.exe"; Sign "$bin\nexus.exe"
$out = "$root\target\linktunnel-$version.msi"
wix build -arch x64 -ext WixToolset.Firewall.wixext -ext WixToolset.Util.wixext -d Version=$version -d BinDir=$bin "$PSScriptRoot\nexus.wxs" -o $out
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Sign $out
Write-Host "built $out"

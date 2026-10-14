# Dot-source this script before native Cargo commands. Environment changes stay in this process.
$nexusRoot = (Resolve-Path "$PSScriptRoot\..").Path
$nexusVsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $nexusVsWhere)) { throw 'Install Visual Studio C++ Build Tools first.' }
$nexusVs = (& $nexusVsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
if (-not $nexusVs) { throw 'Visual Studio needs the Desktop development with C++ workload.' }
$env:PATH = (Split-Path $nexusVsWhere) + ';' + $env:PATH
Import-Module (Join-Path $nexusVs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
Enter-VsDevShell -VsInstallPath $nexusVs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'
$nexusPerl = Join-Path $nexusRoot 'target\tools\perl\perl\bin'
if (Test-Path -LiteralPath $nexusPerl) { $env:PATH = $nexusPerl + ';' + $env:PATH }
$nexusNasm = Join-Path $nexusRoot 'target\tools\nasm\nasm-2.16.03'
if (Test-Path -LiteralPath $nexusNasm) { $env:PATH = $nexusNasm + ';' + $env:PATH }
$env:LC_ALL = 'C'
$env:LC_CTYPE = 'C'
if (-not (Get-Command perl -ErrorAction SilentlyContinue)) { throw 'Install Strawberry Perl for vendored OpenSSL.' }
if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) { throw 'Install NASM to enable optimized H.264 encoding/decoding.' }

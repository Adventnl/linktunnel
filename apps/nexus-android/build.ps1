param(
    [string]$Sdk = "$PSScriptRoot\..\..\target\mobile-tools\sdk",
    [string]$Jdk = '',
    [string]$Output = "$PSScriptRoot\..\..\dist\linktunnel-debug.apk"
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
if (-not $Jdk) {
    $candidate = Get-ChildItem "$repo\target\mobile-tools\jdk" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($candidate) { $Jdk = $candidate.FullName } else { $Jdk = $env:JAVA_HOME }
}
if (-not $Jdk -or -not (Test-Path "$Jdk\bin\javac.exe")) { throw 'Pass -Jdk with a JDK 21 directory.' }
$env:JAVA_HOME = $Jdk
$env:PATH = "$Jdk\bin;$env:PATH"
$Sdk = (Resolve-Path $Sdk).Path
$tools = Join-Path $Sdk 'build-tools\35.0.0'
$platform = Join-Path $Sdk 'platforms\android-35\android.jar'
$work = Join-Path $repo 'target\nexus-android'
$Output = [System.IO.Path]::GetFullPath($Output)
foreach ($buildDirectory in @('classes', 'generated', 'dex')) {
    $cleanTarget = [System.IO.Path]::GetFullPath((Join-Path $work $buildDirectory))
    if (-not $cleanTarget.StartsWith($work + '\', [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Android build cleanup escaped its workspace.' }
    if (Test-Path -LiteralPath $cleanTarget) { Remove-Item -LiteralPath $cleanTarget -Recurse -Force }
}
New-Item -ItemType Directory -Force "$work\classes", "$work\generated", "$work\dex", (Split-Path $Output) | Out-Null
function Invoke-BuildTool([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Android build failed: $Program" }
}
Invoke-BuildTool "$tools\aapt2.exe" @('compile', '--dir', "$PSScriptRoot\res", '-o', "$work\resources.zip")
Invoke-BuildTool "$tools\aapt2.exe" @('link', '-o', "$work\unsigned.apk", '-I', $platform, '--manifest', "$PSScriptRoot\AndroidManifest.xml", '--java', "$work\generated", "$work\resources.zip")
$sources = @(Get-ChildItem "$PSScriptRoot\src", "$work\generated" -Filter '*.java' -Recurse | ForEach-Object FullName)
Invoke-BuildTool "$Jdk\bin\javac.exe" (@('--release', '8', '-encoding', 'UTF-8', '-classpath', $platform, '-d', "$work\classes") + $sources)
$classes = @(Get-ChildItem "$work\classes" -Filter '*.class' -Recurse | ForEach-Object FullName)
Invoke-BuildTool "$tools\d8.bat" (@('--lib', $platform, '--min-api', '26', '--output', "$work\dex") + $classes)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::Open("$work\unsigned.apk", [System.IO.Compression.ZipArchiveMode]::Update)
try { [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, "$work\dex\classes.dex", 'classes.dex') | Out-Null } finally { $archive.Dispose() }
Invoke-BuildTool "$tools\zipalign.exe" @('-f', '4', "$work\unsigned.apk", "$work\aligned.apk")
$keystore = Join-Path $work 'development.p12'
if (-not (Test-Path $keystore)) {
    Invoke-BuildTool "$Jdk\bin\keytool.exe" @('-genkeypair', '-keystore', $keystore, '-storepass', 'android', '-keypass', 'android', '-alias', 'nexus-debug', '-keyalg', 'RSA', '-keysize', '3072', '-validity', '3650', '-dname', 'CN=Nexus Development')
}
Invoke-BuildTool "$tools\apksigner.bat" @('sign', '--ks', $keystore, '--ks-pass', 'pass:android', '--ks-key-alias', 'nexus-debug', '--out', $Output, "$work\aligned.apk")
Invoke-BuildTool "$tools\apksigner.bat" @('verify', '--verbose', $Output)
Get-FileHash -LiteralPath $Output -Algorithm SHA256

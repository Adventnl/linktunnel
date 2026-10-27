param(
    [string]$RepositoryRoot = "$env:USERPROFILE\Documents\Github",
    [string[]]$Repositories = @(),
    [string]$Bind = '',
    [int]$Port = 9443,
    [string]$DataDirectory = "$env:LOCALAPPDATA\Nexus\mobile",
    [switch]$RotatePairing,
    [switch]$Start,
    [switch]$InstallStartup
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
$node = (Get-Command node.exe -ErrorAction Stop).Source
if (-not $Bind) {
    $tailscale = Get-Command tailscale.exe -ErrorAction SilentlyContinue
    $tailscalePath = if ($tailscale) { $tailscale.Source } else { 'C:\Program Files\Tailscale\tailscale.exe' }
    if (Test-Path $tailscalePath) { $Bind = (& $tailscalePath ip -4 2>$null | Select-Object -First 1) }
}
if ($Bind -notmatch '^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3}$') { throw 'Connect Tailscale first or pass -Bind with this PC Tailscale IPv4 address.' }
if ($Port -lt 1024 -or $Port -gt 65535) { throw 'Port must be between 1024 and 65535.' }
if (-not (Get-NetIPAddress -IPAddress $Bind -ErrorAction SilentlyContinue)) { throw 'Bind must be an IPv4 address assigned to this PC.' }
New-Item -ItemType Directory -Force $DataDirectory | Out-Null
# Credentials belong to the interactive user, never the Nexus SYSTEM desktop service.
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& icacls.exe $DataDirectory /inheritance:r /grant:r "*${identity}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Could not protect mobile host credentials.' }
$configPath = Join-Path $DataDirectory 'mobile-host.json'
if ($RotatePairing -and (Test-Path $configPath)) {
    $oldConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    if (Get-NetTCPConnection -LocalAddress $oldConfig.bind -LocalPort $oldConfig.port -State Listen -ErrorAction SilentlyContinue) {
        throw 'Stop the running mobile host before rotating pairing.'
    }
}
if ((Test-Path $configPath) -and -not $RotatePairing) {
    Write-Output 'Preserving existing configuration and pairing. Use -RotatePairing to replace them.'
} else {
    $openssl = Get-Command openssl.exe -ErrorAction SilentlyContinue
    $opensslPath = if ($openssl) { $openssl.Source } else { 'C:\Program Files\Git\usr\bin\openssl.exe' }
    if (-not (Test-Path $opensslPath)) { throw 'OpenSSL is required (included with Git for Windows).' }
    $codexLauncher = (Get-Command codex.cmd -ErrorAction Stop).Source
    $codexJs = Join-Path (Split-Path $codexLauncher) 'node_modules\@openai\codex\bin\codex.js'
    if (-not (Test-Path $codexJs)) { throw 'Install the Codex CLI using npm, then sign in with codex login.' }
    $root = (Resolve-Path $RepositoryRoot).Path
    if ($Repositories.Count -eq 0) {
        $Repositories = @(Get-ChildItem $root -Directory | Where-Object { Test-Path (Join-Path $_.FullName '.git') } | ForEach-Object Name)
    }
    $allowed = @($Repositories | ForEach-Object {
        if ($_ -notmatch '^[a-zA-Z0-9_-]{1,64}$') { throw 'Pass repository folder names containing only letters, numbers, underscores and hyphens.' }
        if (-not (Test-Path (Join-Path (Join-Path $root $_) '.git'))) { throw "Repository not found: $_" }
        @{ id = $_; name = $_; directory = $_ }
    })
    if ($allowed.Count -eq 0) { throw 'No repositories found. Pass -RepositoryRoot and -Repositories.' }
    $random = New-Object byte[] 32
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($random) } finally { $rng.Dispose() }
    $token = [Convert]::ToBase64String($random).TrimEnd('=').Replace('+', '-').Replace('/', '_')
    $key = Join-Path $DataDirectory 'host.key'
    $cert = Join-Path $DataDirectory 'host.pem'
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $opensslPath req -x509 -newkey rsa:3072 -sha256 -nodes -keyout $key -out $cert -days 365 -subj '/CN=Nexus Mobile Host' -addext "subjectAltName=IP:$Bind" 2>$null
    $ErrorActionPreference = $previousPreference
    if ($LASTEXITCODE -ne 0) { throw 'Could not generate the PC TLS certificate.' }
    @{ bind = $Bind; port = $Port; token = $token; repositoryRoot = $root; repositories = $allowed; codexCommand = $node; codexArgs = @($codexJs); tlsKey = 'host.key'; tlsCert = 'host.pem' } |
        ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $configPath -Encoding UTF8
    $certificateBytes = [Convert]::FromBase64String(((Get-Content -LiteralPath $cert -Raw) -replace '-----[^-]+-----', '' -replace '\s', ''))
    $certificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(,$certificateBytes)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { $pin = ([BitConverter]::ToString($sha.ComputeHash($certificate.RawData))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose(); $certificate.Dispose() }
    @{ endpoint = "https://${Bind}:$Port"; token = $token; certificateSha256 = $pin } | ConvertTo-Json |
        Set-Content -LiteralPath (Join-Path $DataDirectory 'pairing.json') -Encoding UTF8
}
$entry = Join-Path $repo 'apps\nexus-codex-host\src\main.mjs'
$argsText = '"' + $entry + '" "' + $configPath + '"'
if ($InstallStartup) {
    # Current-user logon startup runs with ordinary user rights and the user's Codex profile.
    $startup = [Environment]::GetFolderPath('Startup')
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $startup 'Nexus Codex Host.lnk'))
    $shortcut.TargetPath = $node; $shortcut.Arguments = $argsText; $shortcut.WorkingDirectory = $repo; $shortcut.WindowStyle = 7; $shortcut.Save()
}
if ($Start) {
    $activeConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    if (Get-NetTCPConnection -LocalAddress $activeConfig.bind -LocalPort $activeConfig.port -State Listen -ErrorAction SilentlyContinue) {
        Write-Output 'The mobile port already has a listener; no duplicate host was started.'
    } else {
        Start-Process -FilePath $node -ArgumentList $argsText -WindowStyle Hidden -RedirectStandardOutput (Join-Path $DataDirectory 'host.log') -RedirectStandardError (Join-Path $DataDirectory 'host-error.log')
    }
}
Write-Output "Mobile host configuration: $configPath"
Write-Output "Private phone pairing code: $(Join-Path $DataDirectory 'pairing.json')"
Write-Output 'Use Tailscale on Android. If Windows Firewall blocks the connection, add an inbound TCP rule scoped to this port and the Tailscale interface.'

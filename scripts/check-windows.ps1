param([ValidateSet('All', 'Clippy', 'Test')][string]$Mode = 'All')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\windows-build-env.ps1"
Push-Location $nexusRoot
try {
    if ($Mode -in @('All', 'Clippy')) {
        cargo clippy --release --locked --workspace --all-targets -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'Windows strict lint checks failed.' }
    }
    if ($Mode -in @('All', 'Test')) {
        cargo test --release --locked --workspace --no-fail-fast
        if ($LASTEXITCODE -ne 0) { throw 'Windows workspace tests failed.' }
    }
} finally { Pop-Location }

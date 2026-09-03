# Runs one cargo-deny check on Windows.
#
# cargo-deny's own GitHub action is a container action and therefore Linux-only, and running the gate on a
# Linux runner would quietly check the wrong thing: this workspace's dependency graph is Windows-only
# (windows, minhook), so Linux resolves a smaller tree and the gate would pass without seeing what ships.
# `cargo install cargo-deny` would work but rebuilds it from source on every run, so the published binary
# is fetched instead - pinned to a version AND to its checksum, so a swapped asset fails loudly rather than
# running as us.
#
# With -InstallOnly it only puts cargo-deny on PATH, so a later `cargo deny ...` works - which is what
# packaging/build-dist.ps1 runs as its own pre-package gate.
[CmdletBinding()]
param(
    [ValidateSet('licenses', 'advisories', 'bans', 'sources')]
    [string]$Command,

    [switch]$InstallOnly
)

if (-not $Command -and -not $InstallOnly) {
    throw 'give -Command <check> or -InstallOnly'
}

$ErrorActionPreference = 'Stop'

$version = '0.20.2'
$sha256 = '975A22143262FD27476D19EE00C7AF67978426E40E1DEE94EED6BBADE1CF87DC'
$asset = "cargo-deny-$version-x86_64-pc-windows-msvc.tar.gz"
$url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/$asset"

$root = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$work = Join-Path $root 'cargo-deny'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$archive = Join-Path $work $asset

Write-Host "Fetching cargo-deny $version"
Invoke-WebRequest -Uri $url -OutFile $archive

$actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash
if ($actual -ne $sha256) {
    throw "cargo-deny checksum mismatch: expected $sha256, got $actual"
}

tar -xzf $archive -C $work
$exe = Get-ChildItem -Path $work -Recurse -Filter 'cargo-deny.exe' | Select-Object -First 1
if (-not $exe) { throw "cargo-deny.exe not found in $asset" }

if ($InstallOnly) {
    # Put it where `cargo deny` will find it. On a runner GITHUB_PATH makes it stick for later steps; off
    # a runner the process PATH is enough for whatever this shell runs next.
    $dir = $exe.Directory.FullName
    if ($env:GITHUB_PATH) { Add-Content -Path $env:GITHUB_PATH -Value $dir }
    $env:PATH = "$dir;$env:PATH"
    Write-Host "cargo-deny $version on PATH at $dir"
    return
}

& $exe.FullName check $Command
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

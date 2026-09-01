#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Assemble the portable Chrono Mock distribution (Stage 5).

.DESCRIPTION
    Produces a self-contained folder that runs on any Windows 10 1809+ / Server 2019+ x64
    box with no .NET install and no repo checkout. Layout:

        dist/ChronoMock/
            ChronoMock.exe            the WPF launcher (self-contained win-x64)
            <.NET runtime + Wpf.Ui.dll + Localization/>
            core/x64/  chrono.exe  chrono_hook.dll
            core/x86/  chrono.exe  chrono_hook.dll
            calendars/*.json   presets/*.json
            LICENSE   THIRD-PARTY-NOTICES.md   README.md
        dist/ChronoMock-win-x64.zip   the same folder, zipped

    It also assembles a lean CLI-only package (no .NET runtime, just the self-contained Rust
    binaries and data) for scripting and CI:

        dist/chrono-cli/
            chrono.exe  chrono_hook.dll        the 64-bit tool (run + calc)
            x86/  chrono.exe  chrono_hook.dll  the 32-bit tool (for 32-bit targets)
            calendars/*.json   presets/*.json
            LICENSE   THIRD-PARTY-NOTICES.md   README.md
        dist/chrono-cli-win.zip       the same folder, zipped

    The launcher resolves the GUI layout through AppPaths (the x64 core beside it under core/
    is the portable marker). The native cores are self-contained already (static CRT, ADR-1).

    Run with PowerShell 7 (pwsh). Every missing piece is a hard stop - a half-assembled
    distribution that looks complete is worse than a clear failure (untouchable rule 6).

.PARAMETER SkipCoreBuild
    Reuse the cargo release outputs already on disk instead of rebuilding both targets.

.PARAMETER SkipPublish
    Reuse the last GUI publish under target/publish-gui instead of publishing again.
#>
[CmdletBinding()]
param(
    [switch] $SkipCoreBuild,
    [switch] $SkipPublish
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist 'ChronoMock'
$publish = Join-Path $root 'target/publish-gui'
$zip = Join-Path $dist 'ChronoMock-win-x64.zip'
$cliStage = Join-Path $dist 'chrono-cli'
$cliZip = Join-Path $dist 'chrono-cli-win.zip'

function Assert-Exists([string] $path, [string] $why) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "missing '$path' - $why"
    }
}

# Where the cargo build lands each core and its matching-bitness hook DLL. x64 is the default
# host build (target/release), x86 is the cross build (target/i686-pc-windows-msvc/release) -
# the same two locations the GUI's dev fallback and the tests already use.
$x64core = Join-Path $root 'target/release/chrono.exe'
$x64hook = Join-Path $root 'target/release/chrono_hook.dll'
$x86core = Join-Path $root 'target/i686-pc-windows-msvc/release/chrono.exe'
$x86hook = Join-Path $root 'target/i686-pc-windows-msvc/release/chrono_hook.dll'

# --- 1. Native cores, both bitnesses. The support matrix promises x64 AND x86, so a missing
#        i686 build is a hard stop, never a silent single-bitness ship (rule 6). ------------------
if (-not $SkipCoreBuild) {
    Write-Host '== cargo build --release (x64 + x86) =='
    Push-Location $root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build (x64) failed with exit $LASTEXITCODE" }
        cargo build --release --target i686-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw "cargo build (x86) failed with exit $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }
}

Assert-Exists $x64core 'build the x64 core: cargo build --release'
Assert-Exists $x64hook 'build the x64 core: cargo build --release'
Assert-Exists $x86core 'build the x86 core: cargo build --release --target i686-pc-windows-msvc'
Assert-Exists $x86hook 'build the x86 core: cargo build --release --target i686-pc-windows-msvc'

# --- 2. GUI: Release, self-contained win-x64, folder (NOT single-file - WPF single-file with
#        wpfui is unverified, so the safe folder form ships first). --------------------------------
if (-not $SkipPublish) {
    Write-Host '== dotnet publish GUI (Release, self-contained win-x64) =='
    if (Test-Path -LiteralPath $publish) { Remove-Item -LiteralPath $publish -Recurse -Force }
    dotnet publish (Join-Path $root 'gui/ChronoMock.App/ChronoMock.App.csproj') `
        -c Release -r win-x64 --self-contained true -p:PublishSingleFile=false -o $publish --nologo
    if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit $LASTEXITCODE" }
}

Assert-Exists (Join-Path $publish 'ChronoMock.exe') 'run a GUI publish first (do not pass -SkipPublish on a clean tree)'

# --- 3. Assemble dist/ChronoMock fresh. --------------------------------------------------------
Write-Host '== assemble dist/ChronoMock =='
if (Test-Path -LiteralPath $dist) { Remove-Item -LiteralPath $dist -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

# 3a. The published launcher, its runtime, wpfui, and the loose Localization files.
Copy-Item -Path (Join-Path $publish '*') -Destination $stage -Recurse

# 3b. Both cores, each beside its matching-bitness hook DLL - the layout AppPaths resolves.
$coreX64 = Join-Path $stage 'core/x64'
$coreX86 = Join-Path $stage 'core/x86'
New-Item -ItemType Directory -Path $coreX64 | Out-Null
New-Item -ItemType Directory -Path $coreX86 | Out-Null
Copy-Item -LiteralPath $x64core -Destination $coreX64
Copy-Item -LiteralPath $x64hook -Destination $coreX64
Copy-Item -LiteralPath $x86core -Destination $coreX86
Copy-Item -LiteralPath $x86hook -Destination $coreX86

# 3c. Shared data once at the root - the launcher runs each core with this folder as the working
#     directory, so the core's ./calendars and ./presets lookup resolves here.
Copy-Item -Path (Join-Path $root 'calendars') -Destination $stage -Recurse
Copy-Item -Path (Join-Path $root 'presets') -Destination $stage -Recurse

# 3d. Licence and third-party notices - their licences require the notice to travel with every copy.
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'THIRD-PARTY-NOTICES.md') -Destination $stage
Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $stage

# --- 4. Zip the portable folder (a teammate unzips one ChronoMock/ folder). ---------------------
Write-Host '== zip =='
Compress-Archive -Path $stage -DestinationPath $zip -Force

# --- 4b. Assemble the lean CLI-only package (chrono, both bitnesses, no .NET runtime). The Rust
#         binaries are self-contained (static CRT, ADR-1), so this needs no framework. The CLI is
#         a first-class surface (equal to the GUI), not just the GUI's internal core. ------------
Write-Host '== assemble dist/chrono-cli =='
$cliX86 = Join-Path $cliStage 'x86'
New-Item -ItemType Directory -Path $cliStage | Out-Null
New-Item -ItemType Directory -Path $cliX86 | Out-Null
# x64 at the root (the default tool), x86 in a subfolder (for 32-bit targets).
Copy-Item -LiteralPath $x64core -Destination $cliStage
Copy-Item -LiteralPath $x64hook -Destination $cliStage
Copy-Item -LiteralPath $x86core -Destination $cliX86
Copy-Item -LiteralPath $x86hook -Destination $cliX86
# Data beside the x64 exe, so chrono's next-to-exe lookup resolves with no working-directory trick.
Copy-Item -Path (Join-Path $root 'calendars') -Destination $cliStage -Recurse
Copy-Item -Path (Join-Path $root 'presets') -Destination $cliStage -Recurse
# A second copy beside the x86 exe (a few KB of JSON), so the 32-bit tool resolves calendars and
# presets next-to-exe as well, regardless of the working directory.
Copy-Item -Path (Join-Path $root 'calendars') -Destination $cliX86 -Recurse
Copy-Item -Path (Join-Path $root 'presets') -Destination $cliX86 -Recurse
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $cliStage
Copy-Item -LiteralPath (Join-Path $root 'THIRD-PARTY-NOTICES.md') -Destination $cliStage
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cli-readme.md') -Destination (Join-Path $cliStage 'README.md')
Write-Host '== zip cli =='
Compress-Archive -Path $cliStage -DestinationPath $cliZip -Force

# --- 5. Summary. --------------------------------------------------------------------------------
$folderBytes = (Get-ChildItem -LiteralPath $stage -Recurse -File | Measure-Object -Property Length -Sum).Sum
$zipBytes = (Get-Item -LiteralPath $zip).Length
$fileCount = (Get-ChildItem -LiteralPath $stage -Recurse -File | Measure-Object).Count
Write-Host ''
Write-Host 'distribution assembled:'
Write-Host ("  folder   : {0}  ({1:N0} files, {2:N1} MB)" -f $stage, $fileCount, ($folderBytes / 1MB))
Write-Host ("  launcher : {0}" -f (Join-Path $stage 'ChronoMock.exe'))
Write-Host ("  zip      : {0}  ({1:N1} MB)" -f $zip, ($zipBytes / 1MB))
$cliBytes = (Get-ChildItem -LiteralPath $cliStage -Recurse -File | Measure-Object -Property Length -Sum).Sum
$cliZipBytes = (Get-Item -LiteralPath $cliZip).Length
$cliFileCount = (Get-ChildItem -LiteralPath $cliStage -Recurse -File | Measure-Object).Count
Write-Host ("  cli      : {0}  ({1:N0} files, {2:N1} MB)" -f $cliStage, $cliFileCount, ($cliBytes / 1MB))
Write-Host ("  cli zip  : {0}  ({1:N1} MB)" -f $cliZip, ($cliZipBytes / 1MB))

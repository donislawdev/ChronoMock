#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Render the WinGet manifests and the Chocolatey packages of one published release.

.DESCRIPTION
    Four packages, two per feed (the owner's decision of 2026-11-02):

                    the window                     the command line
      WinGet        DonislawDev.ChronoMock         DonislawDev.ChronoMock.CLI
      Chocolatey    chrono-mock                    chrono-mock-cli
      installs      the Windows installer, and     ChronoMock-cli-win.zip
                    in WinGet the window zip
                    second, for --scope user

    The window's packages install the Windows installer because only an installer puts a program in the
    Start menu: WinGet makes no shortcut for a portable package and its manifest has no field for one. The
    order and the reasons stand in the templates, beside the lines they are about.

    Every value comes from one place. The release folder is the one a person downloads from the release
    page. Its SHA256SUMS is the one file taken as it is: every other file is hashed here and has to match
    its line in it, and what ties those files to this project is their signature, not the sums. The
    installer, and every binary sign-release.ps1 signs in the two archives, has to be signed by the
    certificate packaging/codesign.json pins, with a timestamp, the way verify-release.yml checks a
    published release. The installer has to carry the tag's version and the UpgradeCode build-msi.ps1 builds
    with, and each archive has to hold the programs the manifests start, at the version of the tag. Our file
    names carry no version, so these checks are what tell a folder of another release apart.
    The names and addresses come from packaging/components.json, the installer's name, UpgradeCode and
    shortcut sentence from build-msi.ps1 (its -SourceOnly output, the source the installer was built
    from), the release date from CHANGELOG.md, the templates from packaging/winget and packaging/chocolatey.

    Written to -OutDir, which must not exist yet, under a parent that does, outside the repository:
      winget/manifests/d/DonislawDev/ChronoMock/<version>/       the three files of one WinGet pull request
      winget/manifests/d/DonislawDev/ChronoMock/CLI/<version>/   and the three of the other
      chocolatey/chrono-mock/ and chocolatey/chrono-mock-cli/   one folder per `choco pack`
    Rendered into a working folder beside it and renamed at the end, so a refused run leaves nothing, and a
    second run given the same -OutDir is refused rather than mixed into the first.

    Nothing here submits anything. A pull request to microsoft/winget-pkgs and a `choco push` publish under
    the project's name to feeds somebody else moderates, so they stay a person's step.

    Exit codes: 0 rendered, 1 refused - one line on stderr naming the input and why.

.PARAMETER Tag
    The published release, e.g. v0.5.0. A release candidate has no installer, so it gets no packages.

.PARAMETER Release
    The folder holding that release's SHA256SUMS, ChronoMock-app-win-x64.msi, ChronoMock-app-win-x64.zip
    and ChronoMock-cli-win.zip, as downloaded from the release page.

.PARAMETER OutDir
    Where the packages go. Must not exist, and must be outside the repository.
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)] [string] $Tag,
    [Parameter(Mandatory)] [string] $Release,
    [Parameter(Mandatory)] [string] $OutDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$wingetTemplates = Join-Path $PSScriptRoot 'winget'
$chocoTemplates = Join-Path $PSScriptRoot 'chocolatey'

# The manifest schema. Take it from manifests merged into microsoft/winget-pkgs, not from that repository's
# schema folder: a version can be documented there before the validation service accepts it (refused within
# minutes on 2026-08-25 in a sibling project). On 2026-11-02, 3 of the 8 installer manifests of the latest
# commits carried 1.28.0 and the other 5 carried 1.12.0, and 1.28.0 defines every field used here.
$WingetSchema = '1.28.0'

# A refusal is one plain line on stderr and exit 1, the way build-msi.ps1 refuses. `exit` still runs the
# `finally` that removes the working folders.
function Stop-Render([string] $message) {
    [Console]::Error.WriteLine("build-feeds: $message")
    exit 1
}

# What went wrong in a .NET or COM call, without the line PowerShell wraps it in (in the language of the
# machine, which is not always English).
function Get-Reason([System.Management.Automation.ErrorRecord] $record) {
    $exception = $record.Exception
    if ($exception.InnerException) { $exception = $exception.InnerException }
    return $exception.Message
}

# --- what the release has to be ------------------------------------------------------------------------

function Get-ReleaseVersion([string] $releaseTag) {
    if ($releaseTag.Contains('-')) {
        Stop-Render "$releaseTag is a release candidate. Candidates get no installer, so they get no packages"
    }
    if ($releaseTag -notmatch '^v(\d+)\.(\d+)\.(\d+)$') {
        Stop-Render "'$releaseTag' is not a release tag. Pass it the way the release is tagged, for example v0.5.0"
    }
    return "$([int]$Matches[1]).$([int]$Matches[2]).$([int]$Matches[3])"
}

# SHA256SUMS of the release as name -> lower-case hash. A release's file is under a kilobyte, so anything over
# a megabyte is not one, and a name listed twice is a file that cannot be trusted either way.
function Read-Sums([string] $path) {
    $item = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
    if (-not $item -or $item.PSIsContainer) { Stop-Render "$path is not there. Download SHA256SUMS of the release into -Release" }
    if ($item.Length -gt 1MB) { Stop-Render "$path holds $($item.Length) bytes, and a release's SHA256SUMS is under a kilobyte" }
    try {
        $text = [System.IO.File]::ReadAllText($path, [System.Text.UTF8Encoding]::new($false, $true))
    }
    catch {
        Stop-Render "$path is not UTF-8 text"
    }
    $sums = @{}
    foreach ($line in ($text -split "`r?`n")) {
        if (-not $line) { continue }
        if ($line -notmatch '^([0-9a-fA-F]{64}) [ *]?(\S+)$') { Stop-Render "$path has a line that is not '<sha256>  <file>': $line" }
        $name = $Matches[2]
        if ($sums.ContainsKey($name)) { Stop-Render "$path lists $name twice" }
        $sums[$name] = $Matches[1].ToLowerInvariant()
    }
    return $sums
}

# The file's hash, computed here, and the one SHA256SUMS gives for it, which must agree.
function Assert-Hashed([hashtable] $sums, [string] $folder, [string] $name) {
    if (-not $sums.ContainsKey($name)) { Stop-Render "SHA256SUMS in $folder has no line for $name" }
    $path = Join-Path $folder $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Stop-Render "$name is not in $folder. Download it from the release page" }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $sums[$name]) { Stop-Render "$name in $folder hashes to $hash, and SHA256SUMS says $($sums[$name])" }
    return $hash
}

# Property rows of an installer database. The same reader as Read-MsiRows in build-msi.ps1, which says why
# every COM object is released on every path: a held database keeps the file open.
function Read-MsiProperties([string] $msi) {
    $installer = $null
    $database = $null
    $view = $null
    $properties = @{}
    try {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $database = $installer.OpenDatabase($msi, 0)
        $view = $database.OpenView('SELECT `Property`, `Value` FROM `Property`')
        [void]$view.Execute()
        while ($true) {
            $record = $view.Fetch()
            if ($null -eq $record) { break }
            try { $properties[$record.StringData(1)] = $record.StringData(2) }
            finally { [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($record) }
        }
        [void]$view.Close()
    }
    catch {
        Stop-Render "$msi could not be read as an installer database: $(Get-Reason $_)"
    }
    finally {
        foreach ($held in @($view, $database, $installer)) {
            if ($null -ne $held) { [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($held) }
        }
        [System.GC]::Collect()
        [System.GC]::WaitForPendingFinalizers()
    }
    return $properties
}

# The binaries the release signs, per archive, as name in the archive. Read out of sign-release.ps1, the
# script that signs them, by the PowerShell parser: the script is parsed and never run, and SafeGetValue
# gives the value of a constant and refuses anything else.
function Get-SignedBinaries {
    $script = Join-Path $PSScriptRoot 'sign-release.ps1'
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$tokens, [ref]$errors)
    if ($errors) { Stop-Render "sign-release.ps1 does not parse, so what it signs is not known: $($errors[0].Message)" }
    $assignments = @($ast.FindAll({
                param($node)
                $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$OURS'
            }, $true))
    if ($assignments.Count -ne 1) { Stop-Render "sign-release.ps1 assigns `$OURS $($assignments.Count) times, and what it signs is read from one" }
    try {
        $ours = $assignments[0].Right.Expression.SafeGetValue()
    }
    catch {
        Stop-Render "the `$OURS of sign-release.ps1 is not a constant, so what it signs is not known: $(Get-Reason $_)"
    }
    if ($ours -isnot [hashtable]) { Stop-Render 'the $OURS of sign-release.ps1 is not a table of archive -> binaries' }
    return $ours
}

# Every program a manifest starts has to be in the archive at that path and at the tag's version, and every
# binary the release signs in it has to be there too. Each is copied out into the scratch folder so that its
# signature can be asked, and returned as its name in the archive -> the copy. An archive of another release
# is refused here and not by the person who installs it.
function Expand-Checked([string] $zip, [string[]] $programs, [string[]] $signed, [string] $version, [string] $scratch) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipName = Split-Path -Leaf $zip
    $copies = [ordered]@{}
    $archive = $null
    try {
        try {
            $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
        }
        catch {
            Stop-Render "$zipName could not be read as an archive: $(Get-Reason $_)"
        }
        foreach ($name in @($programs) + @($signed)) {
            if ($copies.Contains($name)) { continue }
            $entry = $archive.GetEntry($name)
            if ($null -eq $entry) {
                $why = if ($name -in $programs) { 'which a package starts' } else { 'which the release signs' }
                Stop-Render "$zipName holds no $name, $why"
            }
            $copy = Join-Path $scratch ([guid]::NewGuid().ToString('N') + [System.IO.Path]::GetExtension($name))
            try {
                [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $copy)
            }
            catch {
                Stop-Render "$name could not be read out of $zipName`: $(Get-Reason $_)"
            }
            $copies[$name] = $copy
        }
    }
    finally {
        if ($null -ne $archive) { $archive.Dispose() }
    }
    foreach ($program in $programs) {
        $found = (Get-Item -LiteralPath $copies[$program]).VersionInfo.ProductVersion
        if ($found -ne $version) { Stop-Render "$program in $zipName is version '$found', and the tag is $version" }
    }
    return $copies
}

# Why a file is not signed the way a published release is, or nothing when it is. The questions
# verify-release.yml asks: Valid says only that somebody's certificate signed it, so the certificate has to
# be the one packaging/codesign.json pins, and without a timestamp a signature dies with the certificate.
function Get-SignatureFault([string] $path, [string] $pin) {
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne 'Valid') { return "signature status $($signature.Status)" }
    $signer = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($signature.SignerCertificate.RawData)).ToLowerInvariant()
    if ($signer -ne $pin) { return "signed by a certificate with SHA-256 $signer" }
    if (-not $signature.TimeStamperCertificate) { return 'signed with no timestamp' }
    return ''
}

# What build-msi.ps1 builds the installer from, asked of the script itself rather than copied out of it.
function Get-InstallerIdentity([string] $releaseTag) {
    # A process of its own: the script writes the source to the console rather than to the pipeline, and its
    # refusals end the process they run in.
    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    $source = (& $pwsh -NoProfile -NonInteractive -File (Join-Path $PSScriptRoot 'build-msi.ps1') -Tag $releaseTag -SourceOnly) -join "`n"
    if ($LASTEXITCODE -ne 0) { Stop-Render "build-msi.ps1 -SourceOnly refused $releaseTag (its line above says why)" }
    $identity = @{}
    $patterns = [ordered]@{
        Name        = '<Package\b[^>]*?\sName="([^"]+)"'
        UpgradeCode = '<Package\b[^>]*?\sUpgradeCode="([0-9A-F-]{36})"'
        Shortcut    = '<Shortcut\b[^>]*?\sDescription="([^"]+)"'
    }
    foreach ($key in $patterns.Keys) {
        $found = [regex]::Matches($source, $patterns[$key])
        if ($found.Count -ne 1) { Stop-Render "build-msi.ps1 -SourceOnly gave $($found.Count) values for the installer's $key, and one is needed" }
        $identity[$key] = $found[0].Groups[1].Value
    }
    return $identity
}

function Get-ReleaseDate([string] $version) {
    $changelog = Join-Path $root 'CHANGELOG.md'
    $dates = [regex]::Matches([System.IO.File]::ReadAllText($changelog), "(?m)^## \[$([regex]::Escape($version))\] - (\d{4}-\d{2}-\d{2})\s*$")
    if ($dates.Count -ne 1) { Stop-Render "CHANGELOG.md dates $version $($dates.Count) times, and a release is dated once" }
    return $dates[0].Groups[1].Value
}

# --- where the packages go -----------------------------------------------------------------------------

# A path with every link and junction on the way to it followed, so a link does not get round the refusal
# to write into the repository. Segments that do not exist yet are kept as written.
function Resolve-RealPath([string] $path) {
    $full = [System.IO.Path]::GetFullPath($path)
    $current = [System.IO.Path]::GetPathRoot($full)
    foreach ($segment in $full.Substring($current.Length).Split([System.IO.Path]::DirectorySeparatorChar, [StringSplitOptions]::RemoveEmptyEntries)) {
        $next = Join-Path $current $segment
        $item = Get-Item -LiteralPath $next -Force -ErrorAction SilentlyContinue
        if ($item -and $item.LinkType) { $next = $item.ResolveLinkTarget($true).FullName }
        $current = $next
    }
    return $current.TrimEnd([System.IO.Path]::DirectorySeparatorChar)
}

function Assert-Destination([string] $path) {
    $real = Resolve-RealPath $path
    $repo = Resolve-RealPath $root
    if ($real -eq $repo -or $real.StartsWith($repo + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        Stop-Render "$path is inside the repository. Render outside it, so nothing rendered can be committed by mistake"
    }
    if (Test-Path -LiteralPath $path) { Stop-Render "$path exists already. Give a folder that does not, so nothing old is mixed in" }
    $parent = Split-Path -Parent ([System.IO.Path]::GetFullPath($path))
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) { Stop-Render "$parent, the folder $path would go in, does not exist" }
}

# --- rendering -----------------------------------------------------------------------------------------

# A value goes into YAML, XML and PowerShell alike, so it holds nothing any of them reads as syntax: no quote
# of either kind, no backtick or dollar for PowerShell, no angle bracket or ampersand for XML, no doubled
# brace that would read as a placeholder. Prose is written without apostrophes for that reason.
$unsafe = [regex]'[''"`$<>&]|\{\{|\}\}|\r'

# Fills every placeholder. A value of several lines placed on a line of its own gets that line's indentation
# on each of its lines, which is what a YAML block needs, and blank lines stay blank. An unknown placeholder
# is refused before anything is replaced, because a refusal from inside the replacement would be raised in a
# .NET callback rather than end the run.
function Expand-Template([string] $templatePath, [System.Collections.IDictionary] $values, [System.Collections.Generic.HashSet[string]] $used) {
    $text = [System.IO.File]::ReadAllText($templatePath).Replace("`r`n", "`n")
    $placeholder = '(?m)(^[ ]*)?\{\{([A-Z0-9_]+)\}\}'
    foreach ($asked in [regex]::Matches($text, $placeholder)) {
        $name = $asked.Groups[2].Value
        if (-not $values.Contains($name)) { Stop-Render "$templatePath asks for {{$name}}, which build-feeds.ps1 does not give" }
        [void]$used.Add($name)
    }
    $evaluator = [System.Text.RegularExpressions.MatchEvaluator] {
        param($m)
        $indent = $m.Groups[1].Value
        $lines = ([string]$values[$m.Groups[2].Value]).Split("`n")
        return (($lines | ForEach-Object { if ($_) { $indent + $_ } else { '' } }) -join "`n")
    }.GetNewClosure()
    $out = [regex]::Replace($text, $placeholder, $evaluator)
    if ($out.Contains('{{') -or $out.Contains('}}')) { Stop-Render "$templatePath still holds a doubled brace after rendering" }
    if ($out -match '[^\x09\x0A\x20-\x7E]') {
        # Windows PowerShell 5.1 reads a script without a byte order mark as ANSI, and every file here is
        # read by somebody else's parser, so ASCII is the one encoding nothing can get wrong.
        Stop-Render "$templatePath renders to text that is not ASCII"
    }
    return $out
}

function Write-Rendered([string] $path, [string] $text) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    [System.IO.File]::WriteAllText($path, $text, [System.Text.UTF8Encoding]::new($false))
}

# --- the run -------------------------------------------------------------------------------------------

$version = Get-ReleaseVersion $Tag
# Against the location of this session, as PowerShell resolves a path. .NET resolves a relative path against
# the process folder, which Set-Location does not move, so `out` given after a cd would be another folder.
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$Release = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Release)
Assert-Destination $OutDir
$releaseDir = [System.IO.Path]::GetFullPath($Release)
if (-not (Test-Path -LiteralPath $releaseDir -PathType Container)) { Stop-Render "$Release is not a folder. Download the release files into one" }

$register = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'components.json') | ConvertFrom-Json
$product = $register.product
$publisher = ($product.supplier -split ':\s*', 2)[-1]
foreach ($field in @($product.name, $publisher, $product.homepage, $product.source, $product.license, $product.copyright)) {
    if (-not $field) { Stop-Render 'packaging/components.json is missing a product field the packages name' }
}
$msiName = $register.packages.msi.zip
$appZipName = $register.packages.gui.zip
$cliZipName = $register.packages.cli.zip

# The programs each package starts, as the archives hold them (packaging/build-dist.ps1 stages the window in
# ChronoMock/ and the command line in chrono-cli/).
$appZipExe = 'ChronoMock\ChronoMock.exe'
$cliZipExe = 'chrono-cli\chrono.exe'
$cliX86Exe = 'chrono-cli\x86\chrono.exe'

$identity = Get-InstallerIdentity $Tag
$releaseDate = Get-ReleaseDate $version
$sums = Read-Sums (Join-Path $releaseDir 'SHA256SUMS')
$msiHash = Assert-Hashed $sums $releaseDir $msiName
$appZipHash = Assert-Hashed $sums $releaseDir $appZipName
$cliZipHash = Assert-Hashed $sums $releaseDir $cliZipName
$signedBinaries = Get-SignedBinaries
foreach ($zipName in @($appZipName, $cliZipName)) {
    if (-not $signedBinaries.ContainsKey($zipName) -or -not @($signedBinaries[$zipName])) {
        Stop-Render "sign-release.ps1 signs nothing in $zipName, and each archive holds binaries this project builds"
    }
}
$pin = (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'codesign.json') | ConvertFrom-Json).certificate_sha256
if ($pin -notmatch '^[0-9a-f]{64}$') { Stop-Render 'packaging/codesign.json carries no usable certificate_sha256' }
$msiPath = Join-Path $releaseDir $msiName

$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ('chrono-feeds-' + [guid]::NewGuid().ToString('N'))
$work = Join-Path (Split-Path -Parent ([System.IO.Path]::GetFullPath($OutDir))) ('.' + (Split-Path -Leaf $OutDir) + '.partial-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
try {
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    # The archives first, so that one of another release is refused by what it holds before its signatures
    # are asked. The names in an archive are written with forward slashes.
    $appCopies = Expand-Checked (Join-Path $releaseDir $appZipName) @($appZipExe.Replace('\', '/')) @($signedBinaries[$appZipName]) $version $scratch
    $cliCopies = Expand-Checked (Join-Path $releaseDir $cliZipName) @($cliZipExe.Replace('\', '/'), $cliX86Exe.Replace('\', '/')) @($signedBinaries[$cliZipName]) $version $scratch

    # Every signature is asked before any is refused, so one refusal names every file that is not ours.
    $faults = [System.Collections.Generic.List[string]]::new()
    $fault = Get-SignatureFault $msiPath $pin
    if ($fault) { $faults.Add("$msiName ($fault)") }
    $extracted = [ordered]@{ $appZipName = $appCopies; $cliZipName = $cliCopies }
    foreach ($zipName in $extracted.Keys) {
        foreach ($name in $extracted[$zipName].Keys) {
            $fault = Get-SignatureFault $extracted[$zipName][$name] $pin
            if ($fault) { $faults.Add("$name in $zipName ($fault)") }
        }
    }
    if ($faults.Count) {
        Stop-Render "not signed by the certificate packaging/codesign.json pins ($pin), with a timestamp: $($faults -join ', ')"
    }

    $msiProperties = Read-MsiProperties $msiPath
    if ($msiProperties['ProductVersion'] -ne $version) {
        Stop-Render "$msiName is version '$($msiProperties['ProductVersion'])', and the tag is $version"
    }
    $upgradeCode = "{$($identity.UpgradeCode)}"
    if ($msiProperties['UpgradeCode'] -ne $upgradeCode) {
        Stop-Render "$msiName carries UpgradeCode $($msiProperties['UpgradeCode']), and build-msi.ps1 builds with $upgradeCode"
    }

    # The Chocolatey icon. Moderation takes a CDN address and refuses raw.githubusercontent.com and
    # github.com/.../raw alike, and an address on a branch would change the icon of an approved package with
    # every commit, so it is jsDelivr pinned to the tag. The file has to be in the tag for that to answer.
    $iconPath = 'site/assets/icon.png'
    & git -C $root cat-file -e "$($Tag):$iconPath" 2>$null
    if ($LASTEXITCODE -ne 0) { Stop-Render "$iconPath is not in $Tag in this clone, so the icon address would not answer. Fetch the tag" }
    $repoPath = ([uri]$product.source).AbsolutePath.Trim('/')

    $download = "$($product.source)/releases/download/$Tag"
    $shared = [ordered]@{
        VERSION            = $version
        RELEASE_DATE       = $releaseDate
        WINGET_SCHEMA      = $WingetSchema
        PUBLISHER          = $publisher
        REPO_URL           = $product.source
        ISSUES_URL         = "$($product.source)/issues"
        PROJECT_URL        = $product.homepage
        LICENSE            = $product.license
        LICENSE_URL        = "$($product.source)/blob/$Tag/LICENSE"
        COPYRIGHT          = $product.copyright
        RELEASE_NOTES_URL  = "$($product.source)/releases/tag/$Tag"
        PACKAGE_SOURCE_URL = "$($product.source)/tree/main/packaging/chocolatey"
        ICON_URL           = "https://cdn.jsdelivr.net/gh/$repoPath@$Tag/$iconPath"
    }

    $made = 'It is made for testing your own software. A native application gets a small library injected into ' +
        'its process, which antivirus software may flag. A Chromium or Electron application is driven over its ' +
        'DevTools protocol instead, with nothing injected.'
    $moment = 'as if it were another moment in time - a date in the past or the future, another time zone, a ' +
        'clock that stands still or runs faster - without touching the system clock'
    $tags = @('qa', 'testing', 'time', 'date', 'clock', 'time-zone', 'mock', 'calendar', 'holidays', 'test-dates')
    $x86Note = 'The 32-bit chrono, for 32-bit applications, is in the x86 folder beside it.'

    $packages = @(
        [ordered]@{
            Kind       = 'window'
            WingetId   = 'DonislawDev.ChronoMock'
            ChocoId    = 'chrono-mock'
            Title      = $identity.Name
            Moniker    = 'chronomock'
            Short      = $identity.Shortcut
            Tags       = $tags + @('gui')
            Body       = ("$($identity.Name) starts the application you are testing $moment. Its verdict says whether the " +
                'substitution took effect, and its report lists the time channels it covered and the ones it did not.' +
                "`n`nA second tab calculates test dates - month and quarter ends, business days, the holidays of the " +
                "United States and Poland - and sends a date straight into a session.`n`n$made")
            WingetHow  = ("The Windows installer puts $($identity.Name) in the Start menu of every account and the chrono " +
                'command on the machine PATH, and asks for administrator rights. With --scope user you get the ' +
                'portable archive instead: no administrator rights, and no Start menu entry either - start the window ' +
                'by typing ChronoMock in a terminal. The command line alone is the package DonislawDev.ChronoMock.CLI.')
            ChocoHow   = ("This package runs the Windows installer of the release: $($identity.Name) in the Start menu of " +
                'every account and the chrono command on the machine PATH. The command line alone is the package ' +
                'chrono-mock-cli.')
            Own        = [ordered]@{
                MSI_URL          = "$download/$msiName"
                MSI_SHA256       = $msiHash.ToUpperInvariant()
                MSI_UPGRADE_CODE = $upgradeCode
                APP_ZIP_URL      = "$download/$appZipName"
                APP_ZIP_SHA256   = $appZipHash.ToUpperInvariant()
                APP_ZIP_EXE      = $appZipExe
            }
            Chocolatey = @('chocolateyinstall', 'chocolateyuninstall')
        },
        [ordered]@{
            Kind       = 'cli'
            WingetId   = 'DonislawDev.ChronoMock.CLI'
            ChocoId    = 'chrono-mock-cli'
            Title      = "$($identity.Name) CLI"
            Moniker    = 'chronomock-cli'
            Short      = 'Run any Windows application at a different date from the command line, without touching the system clock'
            Tags       = $tags + @('cli', 'command-line', 'ci')
            Body       = ("chrono is the command line of $($identity.Name). It starts the application you are testing $moment, " +
                'and prints a verdict that says whether the substitution took effect, with the time channels it ' +
                'covered and the ones it did not. Its exit code and its --json output are made for a test pipeline.' +
                "`n`nchrono calc calculates test dates - month and quarter ends, business days, the holidays of the " +
                "United States and Poland.`n`n$made")
            WingetHow  = "Type chrono --help to see the commands. $x86Note The window, with a Start menu entry and the same command line, is the package DonislawDev.ChronoMock."
            ChocoHow   = "Type chrono --help to see the commands. $x86Note The window, with a Start menu entry and the same command line, is the package chrono-mock."
            Own        = [ordered]@{
                CLI_ZIP_URL    = "$download/$cliZipName"
                CLI_ZIP_SHA256 = $cliZipHash.ToUpperInvariant()
                CLI_ZIP_EXE    = $cliZipExe
                CLI_X86_EXE    = $cliX86Exe
            }
            Chocolatey = @('chocolateyinstall')
        }
    )

    foreach ($package in $packages) {
        $values = [ordered]@{}
        foreach ($key in $shared.Keys) { $values[$key] = $shared[$key] }
        foreach ($key in $package.Own.Keys) { $values[$key] = $package.Own[$key] }
        $values['WINGET_ID'] = $package.WingetId
        $values['CHOCO_ID'] = $package.ChocoId
        $values['TITLE'] = $package.Title
        $values['MONIKER'] = $package.Moniker
        $values['SHORT_DESCRIPTION'] = $package.Short
        $values['DESCRIPTION'] = $package.Body
        $values['WINGET_HOW_TO_START'] = $package.WingetHow
        $values['CHOCO_HOW_TO_START'] = $package.ChocoHow
        $values['WINGET_TAGS'] = ($package.Tags | ForEach-Object { "- $_" }) -join "`n"
        $values['CHOCO_TAGS'] = $package.Tags -join ' '
        foreach ($key in $values.Keys) {
            $multiLine = $key -in @('DESCRIPTION', 'WINGET_TAGS')
            $value = [string]$values[$key]
            if (-not $value) { Stop-Render "the $($package.Kind) package has no value for $key" }
            if ($unsafe.IsMatch($value) -or (-not $multiLine -and $value.Contains("`n"))) {
                Stop-Render "the value of $key for the $($package.Kind) package would break a file it lands in: $value"
            }
        }

        # The WinGet folder is the path the files take in microsoft/winget-pkgs: the identifier's first letter,
        # then one folder per part of the identifier, then the version.
        $idParts = $package.WingetId.Split('.')
        $wingetDir = Join-Path $work (Join-Path 'winget/manifests' (Join-Path $idParts[0].Substring(0, 1).ToLowerInvariant() (($idParts -join '/') + "/$version")))
        $files = [ordered]@{
            (Join-Path $wingetDir "$($package.WingetId).yaml")                = Join-Path $wingetTemplates 'version.yaml.in'
            (Join-Path $wingetDir "$($package.WingetId).locale.en-US.yaml")   = Join-Path $wingetTemplates 'locale.en-US.yaml.in'
            (Join-Path $wingetDir "$($package.WingetId).installer.yaml")      = Join-Path $wingetTemplates "installer.$($package.Kind).yaml.in"
            (Join-Path $work "chocolatey/$($package.ChocoId)/$($package.ChocoId).nuspec") = Join-Path $chocoTemplates 'package.nuspec.in'
        }
        foreach ($script in $package.Chocolatey) {
            $files[(Join-Path $work "chocolatey/$($package.ChocoId)/tools/$script.ps1")] = Join-Path $chocoTemplates "tools/$script.$($package.Kind).ps1.in"
        }

        $used = [System.Collections.Generic.HashSet[string]]::new()
        foreach ($target in $files.Keys) {
            $template = $files[$target]
            if (-not (Test-Path -LiteralPath $template -PathType Leaf)) { Stop-Render "there is no template at $template" }
            Write-Rendered $target (Expand-Template $template $values $used)
        }
        # A value nobody asks for is a template that lost a line, which rendering would otherwise not notice.
        $unused = @($values.Keys | Where-Object { -not $used.Contains($_) })
        if ($unused) { Stop-Render "no template of the $($package.Kind) package asks for $($unused -join ', ')" }
    }

    # A rename, which fails when the destination is there. Move-Item would put the folder INSIDE an existing
    # one, so a second run given the same -OutDir, past the check at the start while this one rendered,
    # would nest its packages in this run's and still say rendered.
    try {
        [System.IO.Directory]::Move($work, $OutDir)
    }
    catch {
        Stop-Render "$OutDir could not be made from the rendered folder, so nothing was put there: $(Get-Reason $_)"
    }
    $work = $null
}
finally {
    if ($work -and (Test-Path -LiteralPath $work)) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Output "build-feeds: $Tag rendered into $OutDir"
Get-ChildItem -LiteralPath $OutDir -Recurse -File | ForEach-Object { Write-Output ('  ' + $_.FullName.Substring(([System.IO.Path]::GetFullPath($OutDir)).Length + 1)) }

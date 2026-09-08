#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Phase B: sign a release build with the card, then hand it back to the workflow.

.DESCRIPTION
    The signing key lives on a cryptographic card in a USB reader and cannot be exported - that
    is the whole value of it, so no GitHub-hosted runner will ever reach it. A self-hosted
    runner could, and this is a PUBLIC repository, where a self-hosted runner is a machine
    strangers can aim a pull request at. So the build happens in a workflow and the signature
    happens here, and this script is the seam between them.

    In order, and what it refuses at each step:

      1. downloads the unsigned build phase A produced for this tag;
      2. VERIFIES that build's provenance attestation before touching it - signing something you
         did not check is how a supply chain gets a signature on it;
      3. signs OUR binaries, and only ours, with an RFC 3161 timestamp. Without a timestamp the
         signature dies when the certificate expires, and this one is valid for a year;
      4. reads the certificate back OUT of each signed file and refuses to go on unless it hashes
         to the pin in packaging/codesign.json. A second code-signing certificate on the same
         machine - a renewal, a test one, one from another project - is exactly this accident;
      5. repacks both archives, regenerates their bills of materials over the SIGNED bytes, and
         writes SHA256SUMS over what will actually ship;
      6. uploads all four to the DRAFT release and asks phase C to attest the signed bytes;
      7. waits for that and confirms the draft is COMPLETE - a draft missing one file looks almost
         exactly like a finished one.

    Nothing here publishes. The release stays a draft until a person reads it and presses the
    button, and pressing it runs phase D, which re-checks the published page the way a user would.

    🔴 BE AT THE MACHINE. signtool reaches the card and then waits for its PIN, so this script
    cannot run unattended - measured, by watching it block on exactly that. Whether the card asks
    once or once per file depends on the card middleware's own PIN caching, and there are eleven
    files, so watch the first run before assuming. Each signature prints its file, so a run that
    has stopped is easy to tell from one that is working.

.PARAMETER Tag
    The release tag, e.g. v0.2.0.

.PARAMETER ListCertificates
    Print every code-signing certificate in the store with its subject, fingerprints and expiry,
    and do nothing else. This is how the pin in packaging/codesign.json is set, and how the
    holder sees exactly which personal details a signature would make public.

.PARAMETER DryRun
    Everything except signing, uploading and dispatching.

.PARAMETER Wait
    How long to wait for phase C to attach its bundles, in seconds.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)] [string] $Tag,
    [switch] $ListCertificates,
    [switch] $DryRun,
    [int] $Wait = 300,
    [string] $Work
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$repo = 'donislawdev/ChronoMock'
$attestWorkflow = 'attest-signed.yml'
if (-not $Work) { $Work = Join-Path $root 'build/signing' }

# The Enhanced Key Usage OID for code signing. Matched by OID and never by the friendly name,
# because the friendly name is LOCALISED - on a Polish Windows the same certificate reads
# "Podpisywanie kodu", and a filter written against "Code Signing" reports an empty store. That
# false negative is not hypothetical: it happened while writing this script.
$CODE_SIGNING_OID = '1.3.6.1.5.5.7.3.3'

# 🔴 Exactly the binaries WE build, per archive, by path inside the zip. Not a glob.
#
# The window package ships around 240 assemblies that Microsoft already signed, and re-signing
# somebody else's binary with our certificate would both destroy their signature and assert that we
# produced it. Two more files in there are third-party and unsigned by their own publisher
# (Wpf.Ui.dll and Wpf.Ui.Abstractions.dll) - they stay unsigned for the same reason. The bill of
# materials declares them, so a reader who notices can see what they are.
$OURS = @{
    'ChronoMock-win-x64.zip' = @(
        'ChronoMock/ChronoMock.exe',
        'ChronoMock/ChronoMock.dll',
        'ChronoMock/ChronoMock.Protocol.dll',
        'ChronoMock/core/x64/chrono.exe',
        'ChronoMock/core/x64/chrono_hook.dll',
        'ChronoMock/core/x86/chrono.exe',
        'ChronoMock/core/x86/chrono_hook.dll'
    )
    'chrono-cli-win.zip'     = @(
        'chrono-cli/chrono.exe',
        'chrono-cli/chrono_hook.dll',
        'chrono-cli/x86/chrono.exe',
        'chrono-cli/x86/chrono_hook.dll'
    )
}

# Which package id in packaging/components.json each archive is, for regenerating the SBOM.
$PACKAGE_ID = @{
    'ChronoMock-win-x64.zip' = 'gui'
    'chrono-cli-win.zip'     = 'cli'
}

# What a complete draft carries. A missing one of these is a phase that did not finish.
$EXPECTED_ASSETS = @('ChronoMock-win-x64.zip', 'chrono-cli-win.zip',
    'ChronoMock-win-x64.spdx.json', 'chrono-cli-win.spdx.json', 'SHA256SUMS')

$WARN_DAYS = 90

function Invoke-Step([string[]] $Command) {
    Write-Host "  `$ $($Command -join ' ')"
    $output = & $Command[0] @($Command[1..($Command.Length - 1)]) 2>&1
    if ($LASTEXITCODE -ne 0) {
        $output | ForEach-Object { Write-Host $_ }
        throw "sign-release: '$($Command[0])' failed with exit $LASTEXITCODE"
    }
    return $output
}

function Get-CodeSigningCertificates {
    # Wrapped in @() at both levels on purpose. Under Set-StrictMode a certificate carrying no
    # enhanced key usage at all makes a bare property walk throw rather than return nothing, and the
    # store here holds several of those - so the first version of this line failed on the store
    # itself rather than on any certificate in it.
    Get-ChildItem Cert:\CurrentUser\My, Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
        Where-Object { @($_.EnhancedKeyUsageList | ForEach-Object { $_.ObjectId }) -contains $CODE_SIGNING_OID }
}

function Get-CertificateSha256($certificate) {
    (([System.Security.Cryptography.SHA256]::Create().ComputeHash($certificate.RawData) |
                ForEach-Object { $_.ToString('x2') }) -join '')
}

function Find-SignTool {
    $kits = 'C:\Program Files (x86)\Windows Kits\10\bin'
    $found = @()
    if (Test-Path -LiteralPath $kits) {
        foreach ($version in (Get-ChildItem -LiteralPath $kits -Directory | Sort-Object Name)) {
            $candidate = Join-Path $version.FullName 'x64\signtool.exe'
            if (Test-Path -LiteralPath $candidate) { $found += $candidate }
        }
    }
    if (-not $found) {
        throw ("sign-release: no signtool.exe under $kits - install the Windows SDK " +
            "('Windows SDK Signing Tools' is enough)")
    }
    return $found[-1]
}

# 🔴 The certificate expires on a known date, and a script that merely PRINTS that date draws no
# conclusion from it. The first release after expiry would then fail in the middle of the ritual, at
# the signing step, with the card already in the reader - the worst moment to learn of a certificate
# problem. Pure, so `now` is passed in and the logic can be checked without a card.
function Get-ExpiryNotice($notAfter, [datetime] $now) {
    if (-not $notAfter) { return @('  🔴 the store reported no expiry date - check the card by hand') }
    $when = [datetime]$notAfter
    $days = [int]($when - $now).TotalDays
    if ($days -lt 0) {
        throw ("sign-release: the pinned certificate EXPIRED $(-$days) days ago ($($when.ToString('yyyy-MM-dd'))).`n" +
            "Signing with it now produces a signature Windows will reject. Renew the certificate, then move`n" +
            "certificate_sha256 in packaging/codesign.json to the NEW one - a renewal is a different`n" +
            "certificate, not the same one with a later date.")
    }
    if ($days -le $WARN_DAYS) {
        return @("  🔴 WARNING: $days days left on this certificate ($($when.ToString('yyyy-MM-dd'))).",
            "     Renewing issues a NEW certificate, so certificate_sha256 in packaging/codesign.json",
            "     has to move with it or the next release refuses to sign at all.")
    }
    return @("  $days days left on the certificate")
}

function Get-Pin {
    $path = Join-Path $PSScriptRoot 'codesign.json'
    if (-not (Test-Path -LiteralPath $path)) { throw "sign-release: missing the pin at $path" }
    $pin = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
    if (-not $pin.certificate_sha256 -or $pin.certificate_sha256 -notmatch '^[0-9a-f]{64}$') {
        throw ("sign-release: packaging/codesign.json has no usable certificate_sha256. Run this script " +
            "with -ListCertificates, find the card's certificate and paste its sha256 there.")
    }
    if (-not $pin.timestamp_url) { throw 'sign-release: packaging/codesign.json has no timestamp_url' }
    return $pin
}

function Assert-Path([string] $path, [string] $why) {
    if (-not (Test-Path -LiteralPath $path)) { throw "sign-release: missing '$path' - $why" }
}

function Get-Sha256([string] $path) {
    (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# Every binary in a directory tree with the state of its Authenticode signature. Used before and
# after signing: the difference has to be exactly the files we meant to sign, which is what catches
# a path list that reached further than it should.
function Get-SignatureStates([string] $directory) {
    $states = @{}
    foreach ($file in Get-ChildItem -LiteralPath $directory -Recurse -Include *.exe, *.dll -File) {
        $relative = $file.FullName.Substring($directory.Length).TrimStart('\', '/') -replace '\\', '/'
        $states[$relative] = (Get-AuthenticodeSignature -LiteralPath $file.FullName).Status.ToString()
    }
    return $states
}

# ---------------------------------------------------------------------------------------------
# -ListCertificates: the only mode that touches nothing
# ---------------------------------------------------------------------------------------------

if ($ListCertificates) {
    $pinned = $null
    $path = Join-Path $PSScriptRoot 'codesign.json'
    if (Test-Path -LiteralPath $path) {
        $pinned = (Get-Content -Raw -LiteralPath $path | ConvertFrom-Json).certificate_sha256
    }
    $certificates = @(Get-CodeSigningCertificates)
    if (-not $certificates) {
        Write-Host 'No code-signing certificate in the Windows store.'
        Write-Host 'Plug in the card reader and check the card middleware can see the card.'
        Write-Host "Matched by the code-signing OID $CODE_SIGNING_OID rather than by name, because the"
        Write-Host 'friendly name is localised and a name filter reports an empty store on a localised Windows.'
        return
    }
    Write-Host ''
    Write-Host '🔴 A certificate issued to an individual carries the holder name, town and province in its'
    Write-Host '   subject, and every signed file carries that with it. The first signed release makes it'
    Write-Host '   public and nothing takes it back. Read the subject below before signing anything.'
    Write-Host ''
    foreach ($certificate in $certificates) {
        $sha256 = Get-CertificateSha256 $certificate
        Write-Host "subject     : $($certificate.Subject)"
        Write-Host "issuer      : $($certificate.Issuer)"
        Write-Host "sha1 thumb  : $($certificate.Thumbprint)   (what signtool selects by)"
        Write-Host "sha256      : $sha256   (what packaging/codesign.json pins)"
        Write-Host "valid       : $($certificate.NotBefore.ToString('yyyy-MM-dd')) .. $($certificate.NotAfter.ToString('yyyy-MM-dd'))"
        Get-ExpiryNotice $certificate.NotAfter ([datetime]::Now) | ForEach-Object { Write-Host $_ }
        Write-Host "private key : $($certificate.HasPrivateKey)"
        if ($pinned -and $pinned -eq $sha256) { Write-Host '   THIS IS THE PINNED ONE' }
        elseif ($pinned) { Write-Host '   not the pinned certificate' }
        else { Write-Host '   nothing is pinned yet - paste the sha256 above into packaging/codesign.json' }
        Write-Host ''
    }
    return
}

# ---------------------------------------------------------------------------------------------
# The ritual
# ---------------------------------------------------------------------------------------------

if (-not $Tag) { throw 'sign-release: give the tag, e.g. ./packaging/sign-release.ps1 v0.2.0' }
if (-not $IsWindows) { throw 'sign-release: the card lives on Windows, run this there' }

$work = Join-Path $Work $Tag
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Path $work -Force | Out-Null
Write-Host "working in $work"

Write-Host "`n[1/8] fetching the build this tag produced"
Invoke-Step @('gh', 'run', 'download', '--repo', $repo, '--name', "unsigned-build-$Tag", '--dir', $work) | Out-Null
$archives = @(Get-ChildItem -LiteralPath $work -Filter *.zip -File)
if ($archives.Count -ne $OURS.Count) {
    throw "sign-release: expected $($OURS.Count) archives in the artefact, got $($archives.Name -join ', ')"
}

Write-Host "`n[2/8] verifying what the workflow says it built"
foreach ($archive in $archives) {
    Invoke-Step @('gh', 'attestation', 'verify', $archive.FullName, '--repo', $repo) | Out-Null
}

Write-Host "`n[3/8] unpacking"
$unpacked = @{}
$before = @{}
foreach ($archive in $archives) {
    if (-not $OURS.ContainsKey($archive.Name)) {
        throw "sign-release: the artefact carries '$($archive.Name)', which this script has no signing list for"
    }
    $target = Join-Path $work ("unpacked/" + [System.IO.Path]::GetFileNameWithoutExtension($archive.Name))
    Expand-Archive -LiteralPath $archive.FullName -DestinationPath $target -Force
    $unpacked[$archive.Name] = $target
    $before[$archive.Name] = Get-SignatureStates $target
    foreach ($relative in $OURS[$archive.Name]) {
        Assert-Path (Join-Path $target $relative) 'the signing list names a file this archive does not carry'
    }
    Write-Host ("  {0}: {1} binaries, {2} of them ours" -f $archive.Name,
        $before[$archive.Name].Count, $OURS[$archive.Name].Count)
}

Write-Host "`n[4/8] signing with the card"
$pin = Get-Pin
$certificate = Get-CodeSigningCertificates | Where-Object { (Get-CertificateSha256 $_) -eq $pin.certificate_sha256 } | Select-Object -First 1
if (-not $certificate) {
    throw ("sign-release: the pinned certificate ($($pin.certificate_sha256.Substring(0, 16))...) is not in the " +
        "Windows store. Plug in the card reader and check the middleware sees the card. If the certificate " +
        "was renewed, certificate_sha256 in packaging/codesign.json has to move with it - run this script " +
        "with -ListCertificates to see what is there.")
}
Write-Host "  certificate: $(($certificate.Subject -split ',')[0])"
Get-ExpiryNotice $certificate.NotAfter ([datetime]::Now) | ForEach-Object { Write-Host $_ }
$signtool = Find-SignTool
Write-Host "  signtool: $signtool"

foreach ($archive in $archives) {
    foreach ($relative in $OURS[$archive.Name]) {
        $file = Join-Path $unpacked[$archive.Name] $relative
        if ($DryRun) {
            Write-Host "  DRY RUN, would sign $relative"
            continue
        }
        Invoke-Step @($signtool, 'sign', '/sha1', $certificate.Thumbprint, '/fd', 'sha256',
            '/tr', $pin.timestamp_url, '/td', 'sha256', '/q', $file) | Out-Null
        # 🔴 Read the certificate back OUT of the signed file. A second code-signing certificate on
        # this machine would sign just as willingly and the release page would look identical.
        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne 'Valid') {
            throw "sign-release: $relative came back with signature status $($signature.Status). Nothing has been uploaded."
        }
        $actual = Get-CertificateSha256 $signature.SignerCertificate
        if ($actual -ne $pin.certificate_sha256) {
            throw ("sign-release: $relative was signed by a DIFFERENT certificate.`n" +
                "  expected $($pin.certificate_sha256)`n  got      $actual`nNothing has been uploaded.")
        }
        if (-not $signature.TimeStamperCertificate) {
            throw ("sign-release: $relative carries no timestamp. Without one the signature dies with the " +
                "certificate. Nothing has been uploaded.")
        }
    }
    if (-not $DryRun) {
        # Exactly the files we meant to sign changed state, and nobody else's signature broke.
        $after = Get-SignatureStates $unpacked[$archive.Name]
        $changed = @($after.Keys | Where-Object { $after[$_] -ne $before[$archive.Name][$_] })
        $expected = @($OURS[$archive.Name] | ForEach-Object { $_ -replace '^[^/]+/', '' })
        $unexpected = @($changed | Where-Object { $expected -notcontains $_ })
        if ($unexpected) {
            throw ("sign-release: signing changed files it should not have touched in $($archive.Name): " +
                ($unexpected -join ', '))
        }
        $broken = @($after.Keys | Where-Object { $before[$archive.Name][$_] -eq 'Valid' -and $after[$_] -ne 'Valid' })
        if ($broken) {
            throw "sign-release: signing broke somebody else's signature in $($archive.Name): $($broken -join ', ')"
        }
        Write-Host ("  {0}: {1} files signed by the pinned certificate, timestamped, nothing else touched" -f
            $archive.Name, $OURS[$archive.Name].Count)
    }
}

if ($DryRun) {
    Write-Host "`ndry run finished - nothing was signed, uploaded or published"
    return
}

Write-Host "`n[5/8] repacking"
$shipped = @()
foreach ($archive in $archives) {
    Remove-Item -LiteralPath $archive.FullName -Force
    $source = Join-Path $unpacked[$archive.Name] '*'
    Compress-Archive -Path $source -DestinationPath $archive.FullName -Force
    $shipped += $archive.FullName
    Write-Host ("  {0}  {1}" -f $archive.Name, (Get-Sha256 $archive.FullName))
}

Write-Host "`n[6/8] bills of materials over the signed bytes, and the checksums"
# 🔴 Regenerated here rather than in phase A, and this is a deliberate difference from the ritual as
# written elsewhere. Each SBOM carries the sha256 of the archive it describes, and repacking after
# signing changes that hash - a document generated before the signature would describe an archive
# nobody ships. Phase C then attests these against the signed bytes.
foreach ($archive in $archives) {
    $sbom = Join-Path $work ([System.IO.Path]::GetFileNameWithoutExtension($archive.Name) + '.spdx.json')
    & (Join-Path $PSScriptRoot 'sbom.ps1') -PackageId $PACKAGE_ID[$archive.Name] -ZipPath $archive.FullName -OutPath $sbom
    $shipped += $sbom
}
$sums = Join-Path $work 'SHA256SUMS'
$lines = $shipped | ForEach-Object { "{0}  {1}" -f (Get-Sha256 $_), (Split-Path -Leaf $_) }
[System.IO.File]::WriteAllText($sums, ($lines -join "`n") + "`n", (New-Object System.Text.UTF8Encoding($false)))
$shipped += $sums
Write-Host "  SHA256SUMS over $($shipped.Count - 1) files"

Write-Host "`n[7/8] handing it back to the workflow"
Invoke-Step (@('gh', 'release', 'upload', $Tag) + $shipped + @('--repo', $repo, '--clobber')) | Out-Null
$digests = $archives | ForEach-Object { "$($_.Name)=$(Get-Sha256 $_.FullName)" }
Invoke-Step @('gh', 'workflow', 'run', $attestWorkflow, '--repo', $repo,
    '-f', "tag=$Tag", '-f', "digests=$($digests -join ',')") | Out-Null

Write-Host "`n[8/8] confirming the draft is complete"
# 🔴 This step exists because the script would otherwise end at "dispatched, go look". The upload and
# the dispatch are two calls, phase C is a third thing, and a half-finished draft looks almost exactly
# like a finished one. Waits for phase C rather than assuming it: attesting takes well under a minute,
# and "well under a minute" is not "already done" at the moment the dispatch returns.
$deadline = (Get-Date).AddSeconds([Math]::Max(0, $Wait))
$signedDigests = @{}
foreach ($archive in $archives) { $signedDigests[$archive.Name] = Get-Sha256 $archive.FullName }
while ($true) {
    $view = gh release view $Tag --repo $repo --json assets, isDraft 2>$null | ConvertFrom-Json
    $names = @()
    if ($view) { $names = @($view.assets | ForEach-Object { $_.name }) }
    $missing = @($EXPECTED_ASSETS | Where-Object { $names -notcontains $_ })
    $bundles = @($names | Where-Object { $_.EndsWith('.sigstore.json') })
    if (-not $missing -and $bundles.Count -ge $archives.Count) { break }
    if ((Get-Date) -gt $deadline) {
        Write-Host "  waited $Wait s and the draft is still incomplete."
        if ($missing) { Write-Host "  missing: $($missing -join ', ')" }
        if ($bundles.Count -lt $archives.Count) { Write-Host "  attestation bundles present: $($bundles.Count) of $($archives.Count)" }
        Write-Host "  assets present: $(($names | Sort-Object) -join ', ')"
        throw ("sign-release: the draft is NOT complete. Nothing was published, so nothing is broken - but do " +
            "not press publish until the missing piece is there. Check the run log of $attestWorkflow. " +
            "Re-running this script is safe, the upload uses --clobber.")
    }
    Start-Sleep -Seconds 5
}

# The digests are checked against what the RELEASE carries, not against the local files we made -
# those are the same bytes only if the upload really landed.
$confirm = Join-Path $work 'confirm'
New-Item -ItemType Directory -Path $confirm -Force | Out-Null
Invoke-Step @('gh', 'release', 'download', $Tag, '--repo', $repo, '--pattern', 'SHA256SUMS', '--dir', $confirm) | Out-Null
$published = Get-Content -Raw -LiteralPath (Join-Path $confirm 'SHA256SUMS')
foreach ($name in $signedDigests.Keys) {
    if ($published -notmatch [regex]::Escape($signedDigests[$name])) {
        throw ("sign-release: SHA256SUMS on the release does NOT name the digest we signed for $name.`n" +
            "  signed:    $($signedDigests[$name])`n  published: $published")
    }
}
$view = gh release view $Tag --repo $repo --json isDraft | ConvertFrom-Json
if (-not $view.isDraft) { throw "sign-release: $Tag is NOT a draft any more - it is already public" }

Write-Host ''
foreach ($name in ($names | Sort-Object)) { Write-Host "  asset  $name" }
Write-Host '  PASS: every expected asset is there, the published checksums name the digests we signed, still a draft'
Write-Host ''
Write-Host 'Done. The release is still a DRAFT.'
Write-Host 'Read it, then publish. Publishing runs phase D, which re-checks the published bytes the way a user would.'

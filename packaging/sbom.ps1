#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Write an SPDX 2.3 bill of materials for a packaged zip, from the curated register.

.DESCRIPTION
    Reads packaging/components.json - the register of everything shipped that somebody else
    wrote - and emits one SPDX document per package. The register is the SOURCE. This script
    renders it, checks nothing, and invents nothing: what a build knows and a register cannot
    (the sha256 of the zip that was just written) comes from the file, and everything else
    comes from the register, which build-dist.ps1 has already checked against Cargo.lock and
    against the deps.json the publish produced.

    One document per zip rather than one for both. The two packages have different contents,
    and a single document describing both would misstate each of them.

    The document carries the zip's own sha256, so it is tied to exact bytes. That is also why
    the SBOM sits BESIDE the zip in the release rather than inside it - a file cannot contain
    its own hash.

.PARAMETER PackageId
    Which package to describe: a key of the register's `packages` object (cli or gui).

.PARAMETER ZipPath
    The zip that was built for it. Its sha256 goes into the document.

.PARAMETER OutPath
    Where to write the SPDX JSON.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $PackageId,
    [Parameter(Mandatory)] [string] $ZipPath,
    [Parameter(Mandatory)] [string] $OutPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$registerPath = Join-Path $PSScriptRoot 'components.json'

if (-not (Test-Path -LiteralPath $registerPath)) { throw "missing the register at $registerPath" }
if (-not (Test-Path -LiteralPath $ZipPath)) { throw "missing the package at $ZipPath - build it first" }

$register = Get-Content -Raw -LiteralPath $registerPath | ConvertFrom-Json
if (-not $register.packages.PSObject.Properties.Name.Contains($PackageId)) {
    throw "the register has no package '$PackageId' - it knows: $($register.packages.PSObject.Properties.Name -join ', ')"
}
$package = $register.packages.$PackageId

# The product version, read from the workspace manifest rather than retyped. The GUI half states the
# same number in gui/Directory.Build.props, and bumping only one of the two is a known landmine.
$cargoToml = Get-Content -Raw -LiteralPath (Join-Path $root 'Cargo.toml')
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'cannot read the product version from Cargo.toml' }
$productVersion = $Matches[1]

$zipItem = Get-Item -LiteralPath $ZipPath
$zipSha = (Get-FileHash -LiteralPath $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()

# SPDX identifiers accept letters, digits, '.' and '-' and nothing else, so `serde_json` and
# `Microsoft.NETCore.App.Runtime.win-x64` cannot be used as they stand. Collisions after the rewrite
# are checked below rather than assumed away - two components sharing one identifier would produce a
# document that says different things about the same element.
function ConvertTo-SpdxId([string] $name, [string] $kind) {
    $clean = ($name -replace '[^A-Za-z0-9.\-]', '-')
    # A vendored library is marked as one. SPDX identifiers are case-sensitive, so `minhook` the crate
    # and `MinHook` the C library inside it would already be two distinct elements - and a document
    # whose two entries differ by a single capital letter is a trap for whoever reads it.
    if ($kind -eq 'vendored-c') { return "SPDXRef-Package-$clean-vendored" }
    return "SPDXRef-Package-$clean"
}

function Get-Purl($component) {
    switch ($component.kind) {
        'rust-crate' { "pkg:cargo/$($component.name)@$($component.version)" }
        'nuget' { "pkg:nuget/$($component.name)@$($component.version)" }
        'dotnet-runtime-pack' { "pkg:nuget/$($component.name)@$($component.version)" }
        # A vendored C library is on no registry and has no package URL. Saying nothing is the
        # honest answer - a made-up purl would resolve to somebody else's package.
        default { $null }
    }
}

$rootId = ConvertTo-SpdxId $package.zip 'package'
$packages = [System.Collections.Generic.List[object]]::new()
$relationships = [System.Collections.Generic.List[object]]::new()

$packages.Add([ordered]@{
        SPDXID           = $rootId
        name             = [System.IO.Path]::GetFileNameWithoutExtension($package.zip)
        versionInfo      = $productVersion
        downloadLocation = "$($register.product.source)/releases/download/v$productVersion/$($package.zip)"
        homepage         = $register.product.homepage
        filesAnalyzed    = $false
        licenseConcluded = $register.product.license
        licenseDeclared  = $register.product.license
        copyrightText    = $register.product.copyright
        supplier         = $register.product.supplier
        checksums        = @([ordered]@{ algorithm = 'SHA256'; checksumValue = $zipSha })
        comment          = $package.description
    })
$relationships.Add([ordered]@{
        spdxElementId      = 'SPDXRef-DOCUMENT'
        relationshipType   = 'DESCRIBES'
        relatedSpdxElement = $rootId
    })

# Ordinal, because SPDX identifiers are case-sensitive and a case-insensitive map would report a
# collision that the format does not have.
$seen = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
$count = 0
foreach ($component in $register.components) {
    if ($component.in -notcontains $PackageId) { continue }

    $id = ConvertTo-SpdxId $component.name $component.kind
    if ($seen.ContainsKey($id)) {
        throw "'$($component.name)' and '$($seen[$id])' both become $id once the name is made an SPDX identifier"
    }
    $seen[$id] = $component.name

    $entry = [ordered]@{
        SPDXID           = $id
        name             = $component.name
        downloadLocation = $component.source
        filesAnalyzed    = $false
        licenseConcluded = $component.license_concluded
        licenseDeclared  = $component.license_declared
        # The register records who holds the copyright, not the full notice text. The notices
        # themselves are reproduced in THIRD-PARTY-NOTICES.md, which ships inside the package -
        # the document comment below points a reader there.
        copyrightText    = $component.supplier
    }
    # A component with no version of its own is left without the field rather than given the string
    # NOASSERTION, which SPDX reserves for licences and download locations.
    if ($component.version -ne 'NOASSERTION') { $entry.versionInfo = $component.version }

    $purl = Get-Purl $component
    if ($purl) {
        $entry.externalRefs = @([ordered]@{
                referenceCategory = 'PACKAGE-MANAGER'
                referenceType     = 'purl'
                referenceLocator  = $purl
            })
    }
    $packages.Add($entry)
    $relationships.Add([ordered]@{
            spdxElementId      = $rootId
            relationshipType   = 'CONTAINS'
            relatedSpdxElement = $id
        })
    $count++
}

if ($count -eq 0) { throw "the register lists no component in package '$PackageId', which cannot be right" }

$created = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
$document = [ordered]@{
    spdxVersion       = 'SPDX-2.3'
    dataLicense       = 'CC0-1.0'
    SPDXID            = 'SPDXRef-DOCUMENT'
    name              = "$([System.IO.Path]::GetFileNameWithoutExtension($package.zip))-$productVersion"
    # Unique per build, because it carries the hash of the exact zip this document describes.
    documentNamespace = "$($register.product.homepage)/spdx/$([System.IO.Path]::GetFileNameWithoutExtension($package.zip))/$productVersion/$($zipSha.Substring(0, 16))"
    creationInfo      = [ordered]@{
        created  = $created
        creators = @("Person: $(($register.product.supplier -split ':\s*', 2)[-1])", 'Tool: chronomock-sbom-1')
        comment  = 'Written from packaging/components.json, a register maintained by hand and checked against Cargo.lock and against the deps.json the publish produced. Not the output of a scanner over the built package.'
    }
    comment           = "Full licence texts for every component listed here are in THIRD-PARTY-NOTICES.md, which ships inside the package. The .NET runtime packs additionally carry Microsoft's own notices under dotnet/."
    packages          = $packages
    relationships     = $relationships
}

# A self-check before anything is written, because this document goes into a release and gets an
# attestation of its own. A malformed SBOM is worse than no SBOM: it looks like an answer.
$ids = $packages | ForEach-Object { $_.SPDXID }
$unique = [System.Collections.Generic.HashSet[string]]::new([string[]]$ids, [System.StringComparer]::Ordinal)
if ($unique.Count -ne $ids.Count) { throw 'two packages share an SPDX identifier' }
foreach ($id in $ids) {
    $bare = $id -replace '^SPDXRef-', ''
    if ($bare -notmatch '^[A-Za-z0-9.\-]+$') { throw "SPDX identifier '$id' has a character the format does not allow" }
}
foreach ($relationship in $relationships) {
    foreach ($end in @($relationship.spdxElementId, $relationship.relatedSpdxElement)) {
        if ($end -ne 'SPDXRef-DOCUMENT' -and -not $unique.Contains($end)) {
            throw "relationship points at '$end', which is in no package of this document"
        }
    }
}
foreach ($entry in $packages) {
    foreach ($field in @('SPDXID', 'name', 'downloadLocation', 'licenseConcluded', 'licenseDeclared', 'copyrightText')) {
        if (-not $entry.$field) { throw "package '$($entry.name)' is missing $field, which SPDX requires" }
    }
}

$json = $document | ConvertTo-Json -Depth 12
# Written without a byte order mark: a BOM makes the file fail some SPDX validators, and it is the
# same trap that turns `dotnet format` red elsewhere in this repository.
[System.IO.File]::WriteAllText($OutPath, $json, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ("== sbom == {0}: {1} components, zip {2:N1} MB, sha256 {3}..." -f `
        $package.zip, $count, ($zipItem.Length / 1MB), $zipSha.Substring(0, 12))

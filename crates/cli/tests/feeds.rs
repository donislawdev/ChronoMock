//! The WinGet and Chocolatey packages, and the script that renders them (packaging/build-feeds.ps1).
//!
//! What a package does on a machine is measured by installing it from the feed's own client, which no test
//! here does (tools/feeds has the probes). These guards hold what such a measurement cannot see coming: the
//! shape the measurement rests on, the names that are public for good, the one source of the UpgradeCode,
//! the paths several files must agree on, and the refusals of the script. Rendering a real release needs its
//! files signed by our certificate, so every refusal asked here comes before the signatures or is theirs, on
//! release folders built here: this build's chrono.exe with the version of the newest release in two archives
//! and a copy of pwsh.exe as the installer.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// A file without its whole-line comments, so a word in the explanation of a line never counts as the line.
fn active(text: &str, marker: &str) -> String {
    text.lines().filter(|l| !l.trim_start().starts_with(marker)).collect::<Vec<_>>().join("\n")
}

/// A PowerShell script without its comments: `#` lines and `<# ... #>` blocks, every form the packaging
/// scripts use.
fn active_powershell(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if in_block {
            in_block = !trimmed.contains("#>");
            continue;
        }
        if trimmed.starts_with("<#") {
            in_block = !trimmed.contains("#>");
            continue;
        }
        if !trimmed.starts_with('#') {
            out.push(line);
        }
    }
    out.join("\n")
}

/// The value a PowerShell script assigns to a variable in one quoted literal, e.g. `$Name = 'value'`.
fn powershell_literal(script: &str, variable: &str) -> String {
    let prefix = format!("${variable} = '");
    let line = active_powershell(script)
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(&prefix))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("the script assigns no literal to ${variable}"));
    line[prefix.len()..].split('\'').next().expect("a closing quote").to_string()
}

/// The entries of the top-level `Installers:` list of a WinGet installer manifest, each as its own lines,
/// and the lines outside the list. Enough YAML for files this project writes, not a parser.
fn installers(manifest: &str) -> (Vec<String>, String) {
    let mut entries: Vec<Vec<&str>> = Vec::new();
    let mut outside = Vec::new();
    let mut in_list = false;
    for line in manifest.lines() {
        if line == "Installers:" {
            in_list = true;
            continue;
        }
        let top_level_key = !line.starts_with(' ') && !line.starts_with('-') && !line.trim().is_empty();
        if in_list && top_level_key {
            in_list = false;
        }
        if !in_list {
            outside.push(line);
        } else if line.starts_with("- ") {
            entries.push(vec![line]);
        } else if let Some(entry) = entries.last_mut() {
            entry.push(line);
        }
    }
    (entries.into_iter().map(|e| e.join("\n")).collect(), outside.join("\n"))
}

fn has_line(text: &str, wanted: &str) -> bool {
    text.lines().any(|l| l.trim_start().trim_start_matches("- ") == wanted)
}

/// A fresh install of the window takes the Windows installer, the only kind that puts it in the Start menu
/// (WinGet makes no shortcut for a portable package and has no manifest field for one). Recent clients
/// prefer it over portable and older ones take the first entry that applies, so it is FIRST. The archive is
/// SECOND for --scope user, with no scope of its own, and without ArchiveBinariesDependOnPath, because the
/// window started through WinGet's link runs from the real folder while that field would put the whole .NET
/// runtime on PATH. Measured on Windows Server 2025 with winget v1.29.380 on 2026-11-02 (tools/feeds).
#[test]
fn the_window_installs_the_installer_first_and_the_archive_second() {
    let manifest = active(&read("packaging/winget/installer.window.yaml.in"), "#");
    let (entries, outside) = installers(&manifest);
    assert_eq!(entries.len(), 2, "the window manifest lists two installers, got:\n{entries:#?}");
    let (installer, archive) = (&entries[0], &entries[1]);
    for line in [
        "Architecture: x64",
        "InstallerType: wix",
        "Scope: machine",
        "ElevationRequirement: elevatesSelf",
        "InstallerUrl: {{MSI_URL}}",
        "InstallerSha256: {{MSI_SHA256}}",
        "UpgradeCode: '{{MSI_UPGRADE_CODE}}'",
    ] {
        assert!(has_line(installer, line), "the FIRST installer has `{line}`:\n{installer}");
    }
    for line in [
        "Architecture: x64",
        "InstallerType: zip",
        "NestedInstallerType: portable",
        "RelativeFilePath: {{APP_ZIP_EXE}}",
        "InstallerUrl: {{APP_ZIP_URL}}",
        "InstallerSha256: {{APP_ZIP_SHA256}}",
    ] {
        assert!(has_line(archive, line), "the SECOND installer has `{line}`:\n{archive}");
    }
    assert!(!archive.contains("Scope:"), "the archive has no scope of its own, so --scope user can take it:\n{archive}");
    assert!(!manifest.contains("ArchiveBinariesDependOnPath"), "the window manifest leaves ArchiveBinariesDependOnPath out:\n{manifest}");
    for field in ["InstallerType", "Scope", "NestedInstallerType"] {
        assert!(!outside.contains(field), "{field} stands in each installer's entry, not at the top, because the two are of two kinds");
    }
}

/// chrono started through a link looks for chrono_hook.dll, the calendars and the presets beside the link
/// and finds none (core.hook_dll_missing, tools/feeds/symlink-probe.ps1), so the command line's WinGet
/// package needs the field that makes WinGet put chrono's own folder on PATH instead of making a link.
#[test]
fn the_command_line_is_reached_through_its_own_folder_on_path() {
    let manifest = active(&read("packaging/winget/installer.cli.yaml.in"), "#");
    for line in [
        "InstallerType: zip",
        "NestedInstallerType: portable",
        "RelativeFilePath: {{CLI_ZIP_EXE}}",
        "ArchiveBinariesDependOnPath: true",
        "InstallerUrl: {{CLI_ZIP_URL}}",
        "InstallerSha256: {{CLI_ZIP_SHA256}}",
        "chrono",
    ] {
        assert!(has_line(&manifest, line), "the command line manifest has `{line}`:\n{manifest}");
    }
    let (entries, _) = installers(&manifest);
    assert_eq!(entries.len(), 1, "one installer for the command line, x64");
}

/// The Chocolatey window package runs the same signed installer WinGet installs first, so the two feeds put
/// down one thing in one shape and the Start menu entry comes from the installer, and finds it again by its
/// UpgradeCode to take it away, whichever build of it is installed.
#[test]
fn the_chocolatey_window_runs_the_installer_and_finds_it_by_its_upgrade_code() {
    let install = active_powershell(&read("packaging/chocolatey/tools/chocolateyinstall.window.ps1.in"));
    for line in ["fileType       = 'msi'", "url64bit       = '{{MSI_URL}}'", "checksum64     = '{{MSI_SHA256}}'", "checksumType64 = 'sha256'"] {
        assert!(install.contains(line), "the install script says `{line}`:\n{install}");
    }
    assert!(install.contains("Install-ChocolateyPackage @packageArgs"), "the install script runs the installer");
    let codes = install.lines().find(|l| l.contains("validExitCodes")).expect("the install script names its exit codes");
    assert!(codes.contains("3010"), "3010 is a success: a file in use is set aside and nothing is closed");
    for absent in ["Install-ChocolateyZipPackage", "Install-ChocolateyShortcut"] {
        assert!(!install.contains(absent), "the window package makes nothing of its own ({absent}): the installer does");
    }
    let uninstall = active_powershell(&read("packaging/chocolatey/tools/chocolateyuninstall.window.ps1.in"));
    assert_eq!(powershell_literal(&uninstall, "upgradeCode"), "{{MSI_UPGRADE_CODE}}", "the uninstall finds the installer by the rendered UpgradeCode");
    assert!(uninstall.contains(".RelatedProducts($upgradeCode)"), "the uninstall asks Windows Installer, not Installed apps by name");
    assert!(uninstall.contains("-FileType 'msi'"), "the uninstall runs msiexec");
}

/// Chocolatey makes a shim for every program in a package, and the archive holds two chrono.exe. Two shims
/// of one name would fight over the command, so the 32-bit one gets none.
#[test]
fn the_chocolatey_command_line_shims_only_the_64_bit_chrono() {
    let install = active_powershell(&read("packaging/chocolatey/tools/chocolateyinstall.cli.ps1.in"));
    assert!(install.contains("Install-ChocolateyZipPackage"), "the command line package unpacks the release archive");
    assert!(install.contains("'{{CLI_X86_EXE}}.ignore'"), "the 32-bit chrono is marked for no shim:\n{install}");
}

/// Every package's identifier is public for good: a person installs and upgrades by it, and a new one is a
/// new package beside the old. The owner chose these on 2026-11-02.
#[test]
fn the_package_names_are_the_published_ones() {
    let script = active_powershell(&read("packaging/build-feeds.ps1"));
    for name in ["'DonislawDev.ChronoMock'", "'DonislawDev.ChronoMock.CLI'", "'chrono-mock'", "'chrono-mock-cli'"] {
        assert!(script.contains(&format!("= {name}")), "build-feeds.ps1 names the package {name}");
    }
}

/// The UpgradeCode has one source, build-msi.ps1, and reaches the packages through the installer source it
/// prints: no template and not the renderer may carry a copy that could stay behind when it is read wrong.
#[test]
fn the_upgrade_code_is_written_nowhere_but_in_build_msi() {
    let code = powershell_literal(&read("packaging/build-msi.ps1"), "UpgradeCode");
    assert_eq!(code.len(), 36, "build-msi.ps1 assigns the UpgradeCode");
    let mut files = vec!["packaging/build-feeds.ps1".to_string()];
    for dir in ["packaging/winget", "packaging/chocolatey", "packaging/chocolatey/tools"] {
        for entry in std::fs::read_dir(repo_root().join(dir)).expect("the template folder").flatten() {
            if entry.path().is_file() {
                files.push(format!("{dir}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    assert!(files.len() >= 8, "read {} files, so this check is not reading the templates", files.len());
    for file in &files {
        assert!(!read(file).to_uppercase().contains(&code), "{file} carries the UpgradeCode itself");
    }
}

/// The programs the packages start are where packaging/build-dist.ps1 stages them: the window in
/// ChronoMock/, the command line in chrono-cli/ with the 32-bit one in x86/. A renamed staging folder would
/// otherwise reach a person as an install that finds no program.
#[test]
fn the_program_paths_are_the_staged_ones() {
    let feeds = read("packaging/build-feeds.ps1");
    let dist = read("packaging/build-dist.ps1");
    let staged = |variable: &str, folder: &str| {
        let line = format!("${variable} = Join-Path $dist '{folder}'");
        assert!(active_powershell(&dist).lines().any(|l| l.trim() == line), "build-dist.ps1 stages into {folder} (`{line}`)");
    };
    staged("stage", "ChronoMock");
    staged("cliStage", "chrono-cli");
    assert!(active_powershell(&dist).contains("$cliX86 = Join-Path $cliStage 'x86'"), "build-dist.ps1 puts the 32-bit chrono in x86");
    assert_eq!(powershell_literal(&feeds, "appZipExe"), r"ChronoMock\ChronoMock.exe");
    assert_eq!(powershell_literal(&feeds, "cliZipExe"), r"chrono-cli\chrono.exe");
    assert_eq!(powershell_literal(&feeds, "cliX86Exe"), r"chrono-cli\x86\chrono.exe");
}

/// pwsh, found on this process's PATH.
fn pwsh() -> PathBuf {
    let path = std::env::var_os("PATH").expect("a PATH");
    std::env::split_paths(&path)
        .map(|dir| dir.join("pwsh.exe"))
        .find(|candidate| candidate.is_file())
        .expect("PowerShell 7 (pwsh) must be on PATH - the packaging scripts run on it (untouchable rule 19)")
}

struct Run {
    code: i32,
    said: String,
}

fn build_feeds(tag: &str, release: &Path, out: &Path) -> Run {
    let output = Command::new(pwsh())
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(repo_root().join("packaging").join("build-feeds.ps1"))
        .args(["-Tag", tag, "-Release"])
        .arg(release)
        .arg("-OutDir")
        .arg(out)
        .output()
        .expect("pwsh could not be started");
    let said = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Run { code: output.status.code().unwrap_or(-1), said }
}

const MSI: &str = "ChronoMock-app-win-x64.msi";
const APP_ZIP: &str = "ChronoMock-app-win-x64.zip";
const CLI_ZIP: &str = "ChronoMock-cli-win.zip";

/// The tag of the newest release CHANGELOG.md dates, which the script reads for the release date. Not the
/// workspace's version: between a candidate and its release the workspace carries the candidate's, which the
/// script refuses before it reads a file, and the cases below would check nothing but that refusal.
fn release_tag() -> String {
    let log = read("CHANGELOG.md");
    let is_release = |v: &&str| v.split('.').count() == 3 && v.split('.').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let version = log
        .lines()
        .filter_map(|l| l.strip_prefix("## [")?.split_once("] - "))
        .map(|(version, _)| version)
        .find(is_release)
        .expect("CHANGELOG.md dates a release under `## [X.Y.Z] - date`");
    format!("v{version}")
}

/// This build's chrono.exe as a program of the release `tag`: a copy whose version strings (ProductVersion and
/// FileVersion, the workspace version in UTF-16) carry the release's, padded with NULs so that no length in
/// the version resource moves. Windows reads such a string up to its first NUL.
fn release_program(tag: &str, dir: &Path) -> PathBuf {
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    let built = utf16(env!("CARGO_PKG_VERSION"));
    let mut wanted = utf16(tag.trim_start_matches('v'));
    assert!(wanted.len() <= built.len(), "{tag} is longer than this build's {}, so it cannot be written over it", env!("CARGO_PKG_VERSION"));
    wanted.resize(built.len(), 0);
    let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_chrono")).expect("this build's chrono.exe");
    let mut replaced = 0;
    let mut at = 0;
    while let Some(found) = bytes[at..].windows(built.len()).position(|w| w == built.as_slice()) {
        bytes[at + found..at + found + built.len()].copy_from_slice(&wanted);
        at += found + built.len();
        replaced += 1;
    }
    assert!(replaced >= 1, "this build's chrono.exe carries no version string {} to write {tag} over", env!("CARGO_PKG_VERSION"));
    std::fs::create_dir_all(dir).expect("a folder for the program");
    let program = dir.join("chrono.exe");
    std::fs::write(&program, bytes).expect("the program of the release");
    program
}

/// The binaries sign-release.ps1 signs, per archive, out of its `$OURS` table: each archive name, then the
/// quoted names with a slash under it. Enough PowerShell for that table, not a parser.
fn signed_binaries() -> Vec<(String, Vec<String>)> {
    let script = active_powershell(&read("packaging/sign-release.ps1"));
    let start = script.find("$OURS = @{").expect("sign-release.ps1 assigns $OURS");
    let mut archives: Vec<(String, Vec<String>)> = Vec::new();
    for line in script[start..].lines().skip(1) {
        if line.trim() == "}" {
            break;
        }
        for name in line.split('\'').skip(1).step_by(2) {
            if name.ends_with(".zip") {
                archives.push((name.to_string(), Vec::new()));
            } else if name.contains('/') {
                archives.last_mut().expect("an archive named before its binaries").1.push(name.to_string());
            }
        }
    }
    assert!(archives.len() == 2 && archives.iter().all(|(_, b)| b.len() >= 2), "read {archives:?} out of the $OURS of sign-release.ps1");
    archives
}

/// The binaries verify-release.yml asks the signature of, per archive of a release from 0.3.0 on: the archive
/// name the workflow gives `$appArchive` or `$cliArchive` outside the legacy branch, then the quoted names in
/// the `@( ... )` list it assigns to that archive. Enough PowerShell for those lines, not a parser.
fn verified_binaries() -> Vec<(String, Vec<String>)> {
    let workflow = active(&read(".github/workflows/verify-release.yml"), "#");
    let mut archives = Vec::new();
    for variable in ["appArchive", "cliArchive"] {
        let named = workflow
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("${variable} = if ($legacy)")))
            .unwrap_or_else(|| panic!("verify-release.yml names no ${variable} for the legacy and the current release"));
        let current = named.rsplit("else {").next().and_then(|tail| tail.split('\'').nth(1)).expect("a quoted archive name after else");
        let open = format!("$ours[${variable}] = @(");
        let start = workflow.find(&open).unwrap_or_else(|| panic!("verify-release.yml assigns no {open}"));
        let list = &workflow[start + open.len()..];
        let list = &list[..list.find(')').expect("the list closes")];
        archives.push((current.to_string(), list.split('\'').skip(1).step_by(2).map(str::to_string).collect::<Vec<_>>()));
    }
    archives
}

/// The release is signed by one list and checked by another: sign-release.ps1 signs what its `$OURS` names,
/// and verify-release.yml, the workflow that answers whether a published release is ours, asks the
/// signature of what its `$ours` names (build-feeds.ps1 reads the first). A binary added to the signing
/// list alone is signed and never checked, one added to the check alone fails every release. Both lists
/// have to name the same archives with the same binaries.
#[test]
fn the_release_check_asks_the_signature_of_every_binary_the_release_signs() {
    let sorted = |list: Vec<(String, Vec<String>)>| {
        let mut list: Vec<(String, Vec<String>)> = list
            .into_iter()
            .map(|(archive, mut binaries)| {
                binaries.sort();
                (archive, binaries)
            })
            .collect();
        list.sort();
        list
    };
    let signed = sorted(signed_binaries());
    let verified = sorted(verified_binaries());
    assert!(
        verified.len() == 2 && verified.iter().all(|(_, binaries)| binaries.len() >= 2),
        "read {verified:?} out of the $ours of verify-release.yml"
    );
    assert_eq!(
        verified, signed,
        "verify-release.yml checks another set of binaries than sign-release.ps1 signs (left: checked, right: signed)"
    );
}

/// The programs the packages start, as names in their archives.
fn started_programs() -> Vec<String> {
    let feeds = read("packaging/build-feeds.ps1");
    ["appZipExe", "cliZipExe", "cliX86Exe"].iter().map(|v| powershell_literal(&feeds, v).replace('\\', "/")).collect()
}

/// Builds a release folder that gets as far as the signatures: the installer as a copy of the pwsh.exe that
/// runs this, validly signed by a certificate that is not ours, both archives holding every binary the
/// release signs - `program` (release_program) for each program a package starts and a line of text for the
/// rest - and a SHA256SUMS that lists the three as they are. -Change builds one thing otherwise. It prints how
/// many entries it packed, so a fixture that packed nothing cannot pass for one that worked.
const RELEASE_FIXTURE: &str = r#"
param([string] $Release, [string] $Program, [string] $Programs, [string] $Archives, [string] $Change)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression.FileSystem
New-Item -ItemType Directory -Force -Path $Release | Out-Null
$kind, $what = $Change.Split(':', 2)
$signed = Join-Path $PSHOME 'pwsh.exe'
if ((Get-AuthenticodeSignature -LiteralPath $signed).Status -ne 'Valid') {
    throw "$signed has no valid signature, and the installer stands in as a file somebody else signed"
}
Copy-Item -LiteralPath $signed -Destination (Join-Path $Release 'ChronoMock-app-win-x64.msi')
$started = $Programs.Split(',')
$packed = 0
foreach ($archive in $Archives.Split(';')) {
    $zipName, $list = $archive.Split('=', 2)
    $zipPath = Join-Path $Release $zipName
    if ($kind -eq 'not-an-archive' -and $what -eq $zipName) {
        [System.IO.File]::WriteAllText($zipPath, "stand-in for $zipName")
        continue
    }
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, 'Create')
    try {
        foreach ($entry in $list.Split(',')) {
            if ($kind -eq 'without' -and $what -eq $entry) { continue }
            if ($entry -in $started -and -not ($kind -eq 'as-text' -and $what -eq $entry)) {
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $Program, $entry, 'NoCompression')
            }
            else {
                $writer = [System.IO.StreamWriter]::new($zip.CreateEntry($entry).Open())
                try { $writer.Write("stand-in for $entry") } finally { $writer.Dispose() }
            }
            $packed++
        }
    }
    finally { $zip.Dispose() }
}
$sums = foreach ($name in 'ChronoMock-app-win-x64.msi', 'ChronoMock-app-win-x64.zip', 'ChronoMock-cli-win.zip') {
    "$((Get-FileHash -LiteralPath (Join-Path $Release $name) -Algorithm SHA256).Hash.ToLowerInvariant())  $name"
}
[System.IO.File]::WriteAllText((Join-Path $Release 'SHA256SUMS'), ($sums -join "`n") + "`n")
"packed $packed"
"#;

fn release_folder(dir: &Path, program: &Path, change: &str) {
    let fixture = std::env::temp_dir().join(format!("chrono-feeds-guard-{}-fixture.ps1", std::process::id()));
    std::fs::write(&fixture, RELEASE_FIXTURE).expect("the fixture script");
    let archives = signed_binaries();
    let packed: usize = archives
        .iter()
        .filter(|(zip, _)| change != format!("not-an-archive:{zip}"))
        .flat_map(|(_, binaries)| binaries)
        .filter(|binary| change != format!("without:{binary}"))
        .count();
    let listed = archives.iter().map(|(zip, binaries)| format!("{zip}={}", binaries.join(","))).collect::<Vec<_>>().join(";");
    let output = Command::new(pwsh())
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&fixture)
        .arg("-Release")
        .arg(dir)
        .arg("-Program")
        .arg(program)
        .args(["-Programs", &started_programs().join(","), "-Archives", &listed, "-Change", change])
        .output()
        .expect("pwsh could not be started");
    let said = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success() && said.contains(&format!("packed {packed}")), "the release fixture ({change}) should pack {packed} entries:\n{said}");
}

/// Which archive a binary is in, by the folder it unpacks to.
fn archive_of(binary: &str) -> &'static str {
    if binary.starts_with("ChronoMock/") { APP_ZIP } else { CLI_ZIP }
}

fn temp_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-feeds-guard-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a case folder");
    dir
}

/// One way a release folder or a destination differs from one that gets as far as the signatures: what it
/// is, how the fixture builds it otherwise, how the built folder is changed, and the words the refusal must
/// contain.
type FeedCase = (&'static str, &'static str, fn(&Path), &'static str);

/// The script renders from a published release or not at all, and a refusal leaves nothing behind. A folder
/// that gets past every check of the folder and of the archives is asked first. It stops at the signatures,
/// which nothing here has from our certificate, and that one refusal names every file that is not ours - the
/// installer for its signer and each binary the release signs for its status. So each refusal below is the
/// refusal of what the case changed, not of something the fixture got wrong.
#[test]
fn the_packages_are_rendered_from_a_published_release_or_not_at_all() {
    let tag = release_tag();
    let base = temp_case("base");
    let program = release_program(&tag, &base.join("program"));
    release_folder(&base.join("release"), &program, "none");
    let run = build_feeds(&tag, &base.join("release"), &base.join("out"));
    assert!(run.code == 1 && run.said.contains("not signed by the certificate packaging/codesign.json pins"), "the fixture should stop at the signatures, exited {}:\n{}", run.code, run.said);
    assert!(run.said.contains(&format!("{MSI} (signed by a certificate with SHA-256 ")), "the installer, validly signed by somebody else, is refused for its signer:\n{}", run.said);
    let mut named = 0;
    for (archive, binaries) in signed_binaries() {
        for binary in binaries {
            assert!(run.said.contains(&format!("{binary} in {archive} (signature status ")), "the refusal names {binary} in {archive}:\n{}", run.said);
            named += 1;
        }
    }
    for program in started_programs() {
        let unsigned = format!("{program} in {} (signature status NotSigned)", archive_of(&program));
        assert!(run.said.contains(&unsigned), "the program a package starts is asked its own signature (`{unsigned}`):\n{}", run.said);
    }
    assert!(named >= 11, "the refusal was checked for {named} binaries, so this is not reading what the release signs");
    assert!(!base.join("out").exists(), "a refused run leaves no destination");

    let cases: [FeedCase; 11] = [
        ("no sums", "none", |r| std::fs::remove_file(r.join("SHA256SUMS")).expect("remove"), "is not there"),
        ("a file the sums do not list", "none", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            std::fs::write(r.join("SHA256SUMS"), sums.lines().filter(|l| !l.ends_with(MSI)).collect::<Vec<_>>().join("\n")).expect("write");
        }, "has no line for ChronoMock-app-win-x64.msi"),
        ("a listed file that is missing", "none", |r| std::fs::remove_file(r.join(CLI_ZIP)).expect("remove"), "is not in"),
        ("a file of other bytes", "none", |r| std::fs::write(r.join(APP_ZIP), b"other bytes").expect("write"), "hashes to"),
        ("a line of no known form", "none", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            std::fs::write(r.join("SHA256SUMS"), format!("{sums}not a sum\n")).expect("write");
        }, "has a line that is not"),
        ("a file listed twice", "none", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            let first = sums.lines().next().expect("a line").to_string();
            std::fs::write(r.join("SHA256SUMS"), format!("{sums}{first}\n")).expect("write");
        }, "twice"),
        ("no release folder", "none", |r| std::fs::remove_dir_all(r).expect("remove"), "is not a folder"),
        ("an archive that is not one", "not-an-archive:ChronoMock-cli-win.zip", |_| {}, "ChronoMock-cli-win.zip could not be read as an archive"),
        ("an archive without a program a package starts", "without:chrono-cli/x86/chrono.exe", |_| {}, "ChronoMock-cli-win.zip holds no chrono-cli/x86/chrono.exe, which a package starts"),
        ("an archive without a binary the release signs", "without:ChronoMock/core/x86/chrono_hook.dll", |_| {}, "ChronoMock-app-win-x64.zip holds no ChronoMock/core/x86/chrono_hook.dll, which the release signs"),
        ("a program of no version", "as-text:ChronoMock/ChronoMock.exe", |_| {}, "ChronoMock/ChronoMock.exe in ChronoMock-app-win-x64.zip is version ''"),
    ];
    for (what, built, change, says) in cases {
        let dir = temp_case("case");
        release_folder(&dir.join("release"), &program, built);
        change(&dir.join("release"));
        let run = build_feeds(&tag, &dir.join("release"), &dir.join("out"));
        assert!(run.code == 1 && run.said.contains(says), "{what}: exited {} and said:\n{}\nwant exit 1 and {says:?}", run.code, run.said);
        let left: Vec<String> = std::fs::read_dir(&dir).expect("the case folder").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != "release").collect();
        assert!(left.is_empty(), "{what}: a refused run left {left:?} beside the release folder");
    }

    for (asked, says) in [("v0.5.0-rc.1", "is a release candidate"), ("0.5.0", "is not a release tag")] {
        let run = build_feeds(asked, &base.join("release"), &base.join("out"));
        assert!(run.code == 1 && run.said.contains(says), "-Tag {asked} exited {} and said:\n{}", run.code, run.said);
    }
    let inside = repo_root().join("target").join("feeds-guard-out");
    let run = build_feeds(&tag, &base.join("release"), &inside);
    assert!(run.code == 1 && run.said.contains("is inside the repository"), "a destination in the repository exited {}:\n{}", run.code, run.said);
    assert!(!inside.exists(), "nothing was written into the repository");
    std::fs::create_dir_all(base.join("taken")).expect("a folder");
    let run = build_feeds(&tag, &base.join("release"), &base.join("taken"));
    assert!(run.code == 1 && run.said.contains("exists already"), "a destination that exists exited {}:\n{}", run.code, run.said);

    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_file(std::env::temp_dir().join(format!("chrono-feeds-guard-{}-fixture.ps1", std::process::id())));
}

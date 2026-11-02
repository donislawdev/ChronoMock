//! The WinGet and Chocolatey packages, and the script that renders them (packaging/build-feeds.ps1).
//!
//! What a package does on a machine is measured by installing it from the feed's own client, which no test
//! here does (tools/feeds has the probes). These guards hold what such a measurement cannot see coming: the
//! shape the measurement rests on, the names that are public for good, the one source of the UpgradeCode,
//! the paths several files must agree on, and the refusals of the script. Rendering a real release needs its
//! signed installer, so every refusal asked here comes before the script looks for a signature.

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

/// SHA-256 of a file, lower case, from the same cmdlet the script hashes with.
fn sha256(path: &Path) -> String {
    let output = Command::new(pwsh())
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("(Get-FileHash -LiteralPath '{}' -Algorithm SHA256).Hash.ToLowerInvariant()", path.display()))
        .output()
        .expect("pwsh could not be started");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

const MSI: &str = "ChronoMock-app-win-x64.msi";
const APP_ZIP: &str = "ChronoMock-app-win-x64.zip";
const CLI_ZIP: &str = "ChronoMock-cli-win.zip";

/// A release folder of three stand-in files, and a SHA256SUMS that lists them as they are.
fn release_folder(dir: &Path) {
    std::fs::create_dir_all(dir).expect("a release folder");
    let mut sums = String::new();
    for name in [MSI, APP_ZIP, CLI_ZIP] {
        let path = dir.join(name);
        std::fs::write(&path, format!("stand-in for {name}")).expect("a stand-in file");
        sums.push_str(&format!("{}  {name}\n", sha256(&path)));
    }
    std::fs::write(dir.join("SHA256SUMS"), sums).expect("a SHA256SUMS");
}

fn temp_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-feeds-guard-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a case folder");
    dir
}

/// One way a release folder or a destination differs from one that gets through: what it is, how the folder
/// is changed, and the words the refusal must contain.
type FeedCase = (&'static str, fn(&Path), &'static str);

/// The script renders from a published release or not at all, and a refusal leaves nothing behind. A folder
/// of stand-ins that list their own hashes is asked first: it gets past every check of the folder and stops
/// at the installer's signature, which no stand-in has. So each refusal below is the refusal of what the case
/// changed, not of something the fixture got wrong.
#[test]
fn the_packages_are_rendered_from_a_published_release_or_not_at_all() {
    let base = temp_case("base");
    release_folder(&base.join("release"));
    let run = build_feeds("v0.5.0", &base.join("release"), &base.join("out"));
    assert!(run.code == 1 && run.said.contains("has signature status"), "the stand-ins should stop at the signature, exited {}:\n{}", run.code, run.said);
    assert!(!base.join("out").exists(), "a refused run leaves no destination");

    let cases: [FeedCase; 7] = [
        ("no sums", |r| std::fs::remove_file(r.join("SHA256SUMS")).expect("remove"), "is not there"),
        ("a file the sums do not list", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            std::fs::write(r.join("SHA256SUMS"), sums.lines().filter(|l| !l.ends_with(MSI)).collect::<Vec<_>>().join("\n")).expect("write");
        }, "has no line for ChronoMock-app-win-x64.msi"),
        ("a listed file that is missing", |r| std::fs::remove_file(r.join(CLI_ZIP)).expect("remove"), "is not in"),
        ("a file of other bytes", |r| std::fs::write(r.join(APP_ZIP), b"other bytes").expect("write"), "hashes to"),
        ("a line of no known form", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            std::fs::write(r.join("SHA256SUMS"), format!("{sums}not a sum\n")).expect("write");
        }, "has a line that is not"),
        ("a file listed twice", |r| {
            let sums = std::fs::read_to_string(r.join("SHA256SUMS")).expect("sums");
            let first = sums.lines().next().expect("a line").to_string();
            std::fs::write(r.join("SHA256SUMS"), format!("{sums}{first}\n")).expect("write");
        }, "twice"),
        ("no release folder", |r| std::fs::remove_dir_all(r).expect("remove"), "is not a folder"),
    ];
    for (what, change, says) in cases {
        let dir = temp_case("case");
        release_folder(&dir.join("release"));
        change(&dir.join("release"));
        let run = build_feeds("v0.5.0", &dir.join("release"), &dir.join("out"));
        assert!(run.code == 1 && run.said.contains(says), "{what}: exited {} and said:\n{}\nwant exit 1 and {says:?}", run.code, run.said);
        let left: Vec<String> = std::fs::read_dir(&dir).expect("the case folder").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != "release").collect();
        assert!(left.is_empty(), "{what}: a refused run left {left:?} beside the release folder");
    }

    for (tag, says) in [("v0.5.0-rc.1", "is a release candidate"), ("0.5.0", "is not a release tag")] {
        let run = build_feeds(tag, &base.join("release"), &base.join("out"));
        assert!(run.code == 1 && run.said.contains(says), "-Tag {tag} exited {} and said:\n{}", run.code, run.said);
    }
    let inside = repo_root().join("target").join("feeds-guard-out");
    let run = build_feeds("v0.5.0", &base.join("release"), &inside);
    assert!(run.code == 1 && run.said.contains("is inside the repository"), "a destination in the repository exited {}:\n{}", run.code, run.said);
    assert!(!inside.exists(), "nothing was written into the repository");
    std::fs::create_dir_all(base.join("taken")).expect("a folder");
    let run = build_feeds("v0.5.0", &base.join("release"), &base.join("taken"));
    assert!(run.code == 1 && run.said.contains("exists already"), "a destination that exists exited {}:\n{}", run.code, run.said);

    let _ = std::fs::remove_dir_all(&base);
}

//! The product promises it never reaches the network, and until this file nothing checked it.
//!
//! Three places SAY it - the README ("no telemetry, and it never talks to the internet"),
//! `SECURITY.md` ("it makes no connections off the machine"), and the product document. All three
//! are claims. This is the first thing that looks at the code, and here that matters more than
//! usual: this tool injects a library into a program somebody else wrote and then reports on what
//! that program did. "It sends nothing anywhere" is the one promise where being wrong is not a bug
//! but a betrayal.
//!
//! # Two questions, two registers
//!
//! **Forbidden** names have no legitimate place in this workspace at all. An HTTP client, a
//! download helper, a URL-opening shell verb: their presence is the finding, and there is no file
//! that may carry one.
//!
//! **Watched** names are legitimate somewhere and nowhere else. A TCP socket is how the Chromium
//! mode talks to a debug port on this machine, and spawning a process is how every session starts
//! its target. Each use is registered below with the reason, and a use outside that register fails.
//!
//! # What this canNOT prove, said plainly
//!
//! * **It reads source, not binaries.** A dependency that links WinHTTP would not appear here, and
//!   two other things answer that rather than this scan. The dependency register below holds
//!   because the tree is 51 packages of serde, toml, tracing, minhook and `windows`, with the
//!   `windows` features enumerated one by one in the root manifest. The second layer is
//!   `gui/ChronoMock.Protocol.Tests/BinaryImportsTests.cs`, which reads the import table of the
//!   built binaries - the list of DLLs the loader resolves before a binary runs, written by the
//!   linker from what the code actually calls - and pins every module each of the six links, with
//!   the reason. That is where a networking dependency shows up whether or not our source spells
//!   it. It lives on the C# side because only there are those binaries certainly the ones the same
//!   run just built: `cargo test` goes BEFORE the two release builds in CI and in
//!   `tools/gates.ps1`, and `dotnet test` after them.
//! * **It reads what is written, not what runs.** A name assembled at runtime defeats it. There is
//!   no equivalent of a Python audit hook here, so the static half stands alone.
//! * **Data can leave a machine without a socket** - a file written into a synced folder, a report
//!   pasted into an issue. Nothing here looks at that.
//! * **The hooked `connect` is somebody else's traffic, not ours.** `chrono-hook` resolves
//!   `ws2_32.dll` and intercepts `connect` so the audit can report that the target application
//!   asked the network for something, which is a suspected server time source. Counting a call is
//!   the opposite of making one, and the register says so where it grants that. The binary layer
//!   says the half this one cannot: `chrono_hook.dll` LINKS no networking DLL at all, `ws2_32`
//!   included, because it looks that module up only when the target has already loaded it.
//!
//! So this is not a proof of silence. It is a lock on the surface: nobody adds a way out by
//! accident, and adding one on purpose means editing a register here and writing down why.
//!
//! # The canary is not decoration
//!
//! Every check is pointed at code it must reject, and at clean code it must not. A guard nobody has
//! watched fail is indistinguishable from a guard that reads nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// What the scan reads
// ---------------------------------------------------------------------------------------------

/// Committed trees that ship or build the product. Written out rather than discovered, for the
/// same reason `hygiene.rs` writes its list: `tools/` and `docs/` exist on the maintainer's
/// machine and in no checkout, so a scan that walked the root would be green in CI and red here.
const SCANNED_TREES: &[&str] = &["crates", "gui"];

const SKIP_DIRS: &[&str] = &["target", "obj", "bin", "node_modules", ".vs", ".git"];

const SCANNED_EXTENSIONS: &[&str] = &["rs", "cs"];

// ---------------------------------------------------------------------------------------------
// Register 1: names with no legitimate place here
// ---------------------------------------------------------------------------------------------

/// Clients and helpers that exist to talk to a network. There is no file in this workspace that
/// may carry one, so the name itself is the finding.
///
/// The Windows entries matter more than the crate names: a Rust HTTP client would also show up as
/// a new dependency, but `WinHttpOpen` through the `windows` crate would not, because that crate
/// is already a dependency. It would only need a new feature in the root manifest, which is
/// exactly what the feature check below refuses.
const FORBIDDEN: &[(&str, &str)] = &[
    ("reqwest", "HTTP client"),
    ("ureq", "HTTP client"),
    ("isahc", "HTTP client"),
    ("attohttpc", "HTTP client"),
    ("hyper::", "HTTP library"),
    ("WinHttp", "WinHTTP, the Windows HTTP stack"),
    ("InternetOpen", "WinINet, the other Windows HTTP stack"),
    ("URLDownloadToFile", "a one-call downloader"),
    ("HttpClient", "the .NET HTTP client"),
    ("WebClient", "the older .NET HTTP client"),
    ("WebRequest", "the oldest .NET HTTP client"),
    ("System.Net.Http", "the .NET HTTP namespace"),
    ("System.Net.Sockets", "the .NET socket namespace"),
    ("Dns.", "a name lookup, which is a network round trip"),
    ("Win32_Networking", "a networking feature of the windows crate"),
];

// ---------------------------------------------------------------------------------------------
// Register 2: names that are legitimate in named places and nowhere else
// ---------------------------------------------------------------------------------------------

/// Reaching outside this process. Legitimate somewhere, and the register below says where.
const WATCHED: &[(&str, &str)] = &[
    ("TcpStream", "socket"),
    ("TcpListener", "socket"),
    ("UdpSocket", "socket"),
    ("std::net", "socket"),
    ("Command::new", "spawn"),
    ("Process.Start", "spawn"),
    ("ProcessStartInfo", "spawn"),
    ("UseShellExecute = true", "shell-open"),
    ("ws2_32", "winsock"),
];

/// Every place a watched name is allowed, and why. A use anywhere else fails, and an entry naming
/// code that is gone fails too - a register that keeps a dead permission is a register nobody has
/// read since the code moved.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "crates/cli/src/cdp/mod.rs",
        "socket",
        "the Chromium debug port on this machine: the tool checks the endpoint is the loopback \
         host and the port it was given before it speaks to it",
    ),
    (
        "crates/cli/src/cdp/ws.rs",
        "socket",
        "the WebSocket client for that same local debug port",
    ),
    (
        "crates/cli/src/cdp/launch.rs",
        "spawn",
        "launching the Chromium or Electron target under test",
    ),
    (
        "crates/cli/src/run/mod.rs",
        "spawn",
        "launching the core process the driver speaks the protocol to",
    ),
    (
        "crates/cli/tests/cdp_conformance.rs",
        "spawn",
        "the conformance test runs the built binary. Test files are scanned rather than excluded, \
         because a test can reach the network in CI as easily as the product can on a desktop",
    ),
    (
        "crates/hook/src/lib.rs",
        "winsock",
        "the injected library resolves ws2_32 to INTERCEPT the target's own connect and count it. \
         Counting somebody else's call is the opposite of making one, and the audit reports it as \
         a suspected server time source",
    ),
    (
        "gui/ChronoMock.Protocol.Tests/BinaryImportsTests.cs",
        "winsock",
        "the binary layer of this same guard, which names the module in order to REFUSE it. It reads \
         the import table of every release binary and asserts that ws2_32 is linked by chrono.exe \
         and by nothing else - least of all by chrono_hook.dll, which hooks connect without linking \
         it. Naming a module in a register is the opposite of opening one",
    ),
    (
        "gui/ChronoMock.Protocol/CoreClient.cs",
        "spawn",
        "the window launches the core process, exactly as the command line does",
    ),
    (
        "gui/ChronoMock.Protocol/CalcClient.cs",
        "spawn",
        "the same launch for a one-shot calculator query",
    ),
];

/// Crates that must never reach outside their own process at all, whatever the register says.
///
/// `chrono-hook` is loaded into a program somebody else wrote. `chrono-mech`, `chrono-ctl`,
/// `chrono-core` and `chrono-proto` are the layers beneath the interface. A socket or a spawn in
/// any of them would be a different product. The winsock grant above is deliberately narrow: it
/// permits naming the module, never opening one.
const SILENT_CRATES: &[&str] = &[
    "crates/hook",
    "crates/mech",
    "crates/ctl",
    "crates/core",
    "crates/proto",
    "crates/site",
];

// ---------------------------------------------------------------------------------------------
// The scan itself, as a pure function over one file's text
// ---------------------------------------------------------------------------------------------

/// One finding: what kind of rule was broken, the name that broke it, and the line.
type Finding = (&'static str, String, usize);

/// A source line with its line comment removed, so a rule can be WRITTEN ABOUT in prose without
/// being reported. `chrono-hook` explains ws2_32 at length in comments and must not fail for it.
fn code_only(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("*") {
        return "";
    }
    match line.find("//") {
        Some(cut) => &line[..cut],
        None => line,
    }
}

/// Every finding in one file. A pure function over text so the canary can point it at code that is
/// not in the repository at all.
fn findings(source: &str, rel: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (number, raw) in source.lines().enumerate() {
        let line = code_only(raw);
        if line.trim().is_empty() {
            continue;
        }
        for (name, what) in FORBIDDEN.iter().copied() {
            if line.contains(name) {
                out.push(("forbidden", format!("{name} ({what})"), number + 1));
            }
        }
        for (name, kind) in WATCHED.iter().copied() {
            if !line.contains(name) {
                continue;
            }
            let permitted = ALLOWED
                .iter()
                .any(|(file, allowed_kind, _)| *file == rel && *allowed_kind == kind);
            if !permitted {
                out.push(("unregistered", format!("{name} ({kind})"), number + 1));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Reading the workspace
// ---------------------------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn rel(path: &Path) -> String {
    let root = repo_root().canonicalize().expect("repo root exists");
    let full = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    full.strip_prefix(&root)
        .unwrap_or(&full)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_skipped(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    SKIP_DIRS.contains(&name.as_str())
}

fn has_scanned_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SCANNED_EXTENSIONS.contains(&e))
}

fn scanned_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    for tree in SCANNED_TREES.iter().copied() {
        let mut stack = vec![root.join(tree)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.expect("directory entry").path();
                // Flattened into two conditions on purpose. The nested form reads the same and
                // sits exactly on the nesting ceiling, which would freeze this walk at its
                // current shape for no reason worth paying.
                if path.is_dir() && !is_skipped(&path) {
                    stack.push(path);
                } else if has_scanned_extension(&path) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// This file names every rule it enforces, so it would report itself as the violation.
fn is_this_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("network.rs")
}

// ---------------------------------------------------------------------------------------------
// N1. Nothing reaches the network outside the register
// ---------------------------------------------------------------------------------------------

#[test]
fn nothing_reaches_the_network_outside_the_named_exceptions() {
    let files = scanned_files();
    let mut offenders = Vec::new();
    for path in &files {
        if is_this_file(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let name = rel(path);
        for (kind, detail, line) in findings(&text, &name) {
            offenders.push(format!("{name}:{line} {kind}: {detail}"));
        }
    }

    // The canary every scan here carries: one that reads nothing finds nothing and looks exactly
    // like one that works.
    assert!(
        files.len() >= 60,
        "the network scan read only {} files - it is looking in the wrong place",
        files.len()
    );
    assert!(
        offenders.is_empty(),
        "the product promises it never reaches the network. Either this is a mistake, or the \
         register at the top of this file needs an entry saying why it is not: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// N2. The injected library and the layers beneath the interface are silent
// ---------------------------------------------------------------------------------------------

#[test]
fn the_injected_library_and_the_core_layers_open_nothing() {
    let mut offenders = Vec::new();
    let mut read = 0usize;
    for path in scanned_files() {
        if is_this_file(&path) {
            continue;
        }
        let name = rel(&path);
        if !SILENT_CRATES.iter().any(|c| name.starts_with(c)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        read += 1;
        for (number, raw) in text.lines().enumerate() {
            let line = code_only(raw);
            // Naming ws2_32 is granted to the hook and the control section, because intercepting
            // the target's connect is what the audit is for. Opening one is not, and these are the
            // spellings that would do it.
            for name_to_ban in ["TcpStream", "TcpListener", "UdpSocket", "std::net", "Command::new"]
            {
                if line.contains(name_to_ban) {
                    offenders.push(format!("{name}:{} opens {name_to_ban}", number + 1));
                }
            }
        }
    }

    assert!(read >= 10, "the silent-crate scan read only {read} files");
    assert!(
        offenders.is_empty(),
        "chrono-hook runs inside a program somebody else wrote, and the layers beneath the \
         interface have no business outside their own process: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// N3. No dependency is a network client
// ---------------------------------------------------------------------------------------------

#[test]
fn no_dependency_is_a_network_client() {
    let lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).expect("Cargo.lock is read");
    let names: BTreeSet<&str> = lock
        .lines()
        .filter_map(|l| l.strip_prefix("name = "))
        .map(|n| n.trim_matches('"'))
        .collect();

    // Whole crates whose purpose is to speak to a network. A transitive one arrives the same way a
    // direct one does, and Cargo.lock is where both become visible.
    const NETWORK_CRATES: &[&str] = &[
        "reqwest", "hyper", "ureq", "isahc", "attohttpc", "surf", "awc", "curl", "curl-sys",
        "tokio", "async-std", "smol", "rustls", "native-tls", "openssl", "trust-dns-resolver",
        "hickory-resolver", "tungstenite", "tokio-tungstenite", "socket2", "mio",
    ];
    let found: Vec<&str> =
        NETWORK_CRATES.iter().copied().filter(|c| names.contains(c)).collect();

    assert!(
        names.len() >= 20,
        "the dependency scan read only {} package names from Cargo.lock",
        names.len()
    );
    assert!(
        found.is_empty(),
        "a dependency that can speak to a network arrived in the tree. The Chromium mode uses \
         std::net directly and deliberately, precisely so that no such crate is needed: {found:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// N4. The windows crate enables no networking feature
// ---------------------------------------------------------------------------------------------

#[test]
fn the_windows_crate_enables_no_networking_feature() {
    let manifest =
        std::fs::read_to_string(repo_root().join("Cargo.toml")).expect("root manifest is read");

    // The features are enumerated one by one in the root manifest, which is what makes this
    // cheap: reaching WinHTTP through the windows crate needs a new line there, and this is the
    // line that refuses it.
    let offenders: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("\"Win32_Networking") || l.starts_with("\"Win32_Web"))
        .collect();

    assert!(
        manifest.contains("Win32_System_Time"),
        "the manifest scan did not find the feature list it is supposed to be reading"
    );
    assert!(
        offenders.is_empty(),
        "a networking feature of the windows crate was enabled: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// N5. The register describes code that exists
// ---------------------------------------------------------------------------------------------

#[test]
fn every_registered_exception_still_names_live_code() {
    let root = repo_root();
    let mut stale = Vec::new();
    for (file, kind, _reason) in ALLOWED.iter().copied() {
        let path = root.join(file);
        if !path.is_file() {
            stale.push(format!("{file} is gone but still holds a {kind} permission"));
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("registered file is read");
        let still_uses = WATCHED
            .iter()
            .filter(|(_, k)| *k == kind)
            .any(|(name, _)| text.lines().any(|l| code_only(l).contains(name)));
        if !still_uses {
            stale.push(format!("{file} no longer uses anything of kind '{kind}'"));
        }
    }
    assert!(
        stale.is_empty(),
        "a permission outlived the code it was granted for. A register that keeps dead entries is \
         one nobody has read since the code moved: {stale:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// N6. The canary, in both directions
// ---------------------------------------------------------------------------------------------

/// Code this scanner exists to reject, one case per shape. A canary that only tries the spelling
/// the author had in mind proves nothing about the ones they did not.
const BAD_CODE: &[(&str, &str, &str)] = &[
    ("an HTTP client crate", "let c = reqwest::blocking::Client::new();", "forbidden"),
    ("the other one", "let r = ureq::get(\"x\").call();", "forbidden"),
    ("WinHTTP through the windows crate", "unsafe { WinHttpOpen(None) };", "forbidden"),
    ("WinINet, the other stack", "unsafe { InternetOpenW(None) };", "forbidden"),
    ("a one-call downloader", "URLDownloadToFileW(ptr, url, path);", "forbidden"),
    ("the .NET HTTP client", "var client = new HttpClient();", "forbidden"),
    ("its older form", "using var wc = new WebClient();", "forbidden"),
    ("a .NET name lookup", "var ip = Dns.GetHostAddresses(host);", "forbidden"),
    ("a socket in a file with no permission", "let s = TcpStream::connect(addr)?;", "unregistered"),
    ("a listener", "let l = TcpListener::bind(addr)?;", "unregistered"),
    ("a datagram socket", "let u = UdpSocket::bind(addr)?;", "unregistered"),
    ("the module path spelling", "use std::net::ToSocketAddrs;", "unregistered"),
    ("spawning a process", "Command::new(\"curl\").arg(url).spawn()?;", "unregistered"),
    ("spawning one from the window", "Process.Start(psi);", "unregistered"),
    ("handing a URL to the shell", "psi.UseShellExecute = true;", "unregistered"),
];

#[test]
fn the_scanner_catches_every_shape_it_exists_to_catch() {
    let mut missed = Vec::new();
    for (label, source, want) in BAD_CODE.iter().copied() {
        let kinds: BTreeSet<&str> =
            findings(source, "crates/cli/src/nowhere.rs").iter().map(|(k, _, _)| *k).collect();
        if !kinds.contains(want) {
            missed.push(format!("{label} -> saw {kinds:?}, wanted {want}"));
        }
    }
    assert!(
        missed.is_empty(),
        "the scanner missed a shape it exists to catch. A guard that has only ever seen clean code \
         has been shown to run, not to look: {missed:?}"
    );
}

#[test]
fn ordinary_code_and_prose_raise_no_finding() {
    // The negative half of the same claim. A scanner that flagged everything would satisfy the
    // canary above and be useless - and the prose case is not hypothetical, because chrono-hook
    // discusses ws2_32 and the connect hook at length in comments.
    let clean = "\
        // The audit counts the target's own connect (ws2_32) so a server time source is visible.\n\
        /// Spawning is not done here - see crates/cli/src/run/mod.rs for Command::new.\n\
        let moment = zone.to_utc(local)?;\n\
        let count = coverage.calls_for(Channel::GetSystemTime);\n";
    let found = findings(clean, "crates/core/src/calc.rs");
    assert!(found.is_empty(), "ordinary code and prose raised a finding: {found:?}");
}

#[test]
fn a_registered_file_may_use_what_it_was_granted_and_nothing_more() {
    // The register is per file AND per kind, which is the half most easily got wrong. The Chromium
    // socket file may open a socket, and may still not spawn a process.
    let socket_use = "let stream = TcpStream::connect((host, port))?;";
    assert!(
        findings(socket_use, "crates/cli/src/cdp/mod.rs").is_empty(),
        "the registered socket file was refused its own socket"
    );

    let spawn_use = "Command::new(browser).spawn()?;";
    let refused = findings(spawn_use, "crates/cli/src/cdp/mod.rs");
    assert!(
        refused.iter().any(|(kind, _, _)| *kind == "unregistered"),
        "a file granted a socket was also allowed to spawn, which the register does not say"
    );
}

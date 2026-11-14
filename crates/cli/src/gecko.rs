//! Applications on Mozilla's Gecko engine (ADR-28).
//!
//! Gecko runs its pages, their scripts and its network work in processes its sandbox broker starts under a
//! restricted token. The hook does not enter a process started that way (`h_cpiw` in chrono-hook): the
//! broker prepares the child before its first instruction runs, and loading the library earlier would run
//! the child's start outside the sandbox it is prepared for. Those processes read the real clock, and the
//! session counts them among the uncovered children. Gecko reads one variable per kind of process that
//! starts that kind without its sandbox, and a process started so is a child the hook enters like any
//! other. The session sets them only when the tester asks (`--no-gecko-sandbox`), because the application
//! then runs without that protection for as long as those processes live.

use std::path::Path;

/// The variables, one per kind of process Gecko sandboxes on Windows. Read out of the xul.dll of two Gecko
/// applications of version 160 on 2026-11-14: these seven, in both. With all seven set, every process of
/// either ran under the session, and without the socket one the network process stayed
/// uncovered in three runs out of three (tools/probes/gecko-start). A kind Gecko adds later stays uncovered
/// until its variable is added here, and the session names it among the uncovered children meanwhile.
pub(crate) const SANDBOX_VARIABLES: [&str; 7] = [
    "MOZ_DISABLE_CONTENT_SANDBOX",
    "MOZ_DISABLE_GMP_SANDBOX",
    "MOZ_DISABLE_GPU_SANDBOX",
    "MOZ_DISABLE_RDD_SANDBOX",
    "MOZ_DISABLE_SOCKET_PROCESS_SANDBOX",
    "MOZ_DISABLE_UTILITY_SANDBOX",
    "MOZ_DISABLE_VR_SANDBOX",
];

/// The variable of the processes the pages run in, the one a tester's own environment may already set.
const PAGES_VARIABLE: &str = SANDBOX_VARIABLES[0];

/// The library every Gecko application carries beside its program.
const GECKO_LIBRARY: &str = "xul.dll";

/// The session started its programs with the variables that switch the Gecko sandbox off.
pub(crate) const KEY_SANDBOX_OFF: &str = "session.gecko_sandbox_off";

/// The target is a Gecko application and its sandbox stays on, so its pages stay on the real clock.
pub(crate) const KEY_PAGES_SANDBOXED: &str = "session.gecko_pages_sandboxed";

/// Whether the program at `target` is a Gecko application: xul.dll beside it. A launcher in another folder
/// that starts one (a portable build) is not recognised, and the variables still reach the application
/// through it when the tester asks for them.
pub(crate) fn is_gecko_program(target: &Path) -> bool {
    target.parent().is_some_and(|dir| dir.join(GECKO_LIBRARY).is_file())
}

/// The changes `--no-gecko-sandbox` makes to the environment the target inherits, and the processes it
/// starts inherit from it.
pub(crate) fn sandbox_off() -> Vec<chrono_mech::EnvChange> {
    SANDBOX_VARIABLES.iter().map(|name| chrono_mech::EnvChange::Set((*name).to_string(), "1".to_string())).collect()
}

/// Whether the environment the session would hand on already starts the pages without their sandbox.
pub(crate) fn pages_unsandboxed_by_tester() -> bool {
    std::env::var_os(PAGES_VARIABLE).is_some()
}

/// What a native session says beforehand about the Gecko sandbox, the session and its dry run alike. With
/// the sandbox switched off, that it was, whatever the program: the session set the variables, and a
/// Gecko application it starts through a launcher gets them too. With it on, for a program known to be
/// Gecko, that its pages stay on the real clock and how to change that - unless the tester's own
/// environment already starts them unsandboxed.
pub(crate) fn warning(target: &Path, sandbox: bool, unsandboxed_by_tester: bool) -> Option<&'static str> {
    if !sandbox {
        Some(KEY_SANDBOX_OFF)
    } else if !unsandboxed_by_tester && is_gecko_program(target) {
        Some(KEY_PAGES_SANDBOXED)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chrono-gecko-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        dir
    }

    /// A program with xul.dll beside it is Gecko, one without it is not, and a folder named so is not the
    /// library.
    #[test]
    fn a_gecko_program_is_known_by_the_library_beside_it() {
        let gecko = folder("gecko", &["browser.exe", "xul.dll"]);
        let other = folder("other", &["app.exe", "xul.dll.bak"]);
        let named = folder("named", &["app.exe"]);
        std::fs::create_dir_all(named.join("xul.dll")).unwrap();
        assert!(is_gecko_program(&gecko.join("browser.exe")));
        assert!(!is_gecko_program(&other.join("app.exe")));
        assert!(!is_gecko_program(&named.join("app.exe")), "a folder called xul.dll is not the library");
        assert!(!is_gecko_program(Path::new("browser.exe")), "a bare name has no folder to look in");
        for dir in [gecko, other, named] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Switched off, the session says so for any program, and only then sets the variables. Left on, it
    /// says the pages stay on the real clock for a Gecko program alone, and not when the tester's own
    /// environment already starts them unsandboxed.
    #[test]
    fn the_session_says_what_it_did_and_what_the_pages_will_read() {
        let gecko = folder("warning", &["browser.exe", "xul.dll"]);
        let browser = gecko.join("browser.exe");
        let other = Path::new(r"C:\Windows\System32\notepad.exe");
        assert_eq!(warning(&browser, false, false), Some(KEY_SANDBOX_OFF));
        assert_eq!(warning(other, false, false), Some(KEY_SANDBOX_OFF), "the session set the variables, whatever the program");
        assert_eq!(warning(&browser, true, false), Some(KEY_PAGES_SANDBOXED));
        assert_eq!(warning(&browser, true, true), None, "the tester's environment already unsandboxes the pages");
        assert_eq!(warning(other, true, false), None);
        let _ = std::fs::remove_dir_all(gecko);
    }

    /// Every variable is set, to a value Gecko reads as set, and nothing else is.
    #[test]
    fn switching_the_sandbox_off_sets_every_variable_and_nothing_else() {
        let changes = sandbox_off();
        assert_eq!(changes.len(), SANDBOX_VARIABLES.len());
        for name in SANDBOX_VARIABLES {
            assert!(
                changes.iter().any(|c| matches!(c, chrono_mech::EnvChange::Set(n, v) if n == name && v == "1")),
                "{name} is set to 1"
            );
        }
    }
}

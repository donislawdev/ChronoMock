//! Detecting a Chromium/Electron target and launching it under our control (slice C2). Unlike a
//! native session - which injects a hook into whatever the user runs - the CDP mechanism OWNS the
//! instance it drives: it launches with an isolated profile and a debug port, and tears both down at
//! the end (chrono-mock 8.8 - leave nothing behind).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Whether the target exe looks like a Chromium/Electron app: its folder ships the Chromium runtime
/// (`icudtl.dat` plus a V8 snapshot). Electron additionally carries `resources/app.asar`, but the
/// runtime files alone are a strong signature that is stable across Chromium/Electron versions.
pub fn is_chromium_target(target: &str) -> bool {
    let Some(dir) = Path::new(target).parent() else {
        return false;
    };
    let has_icu = dir.join("icudtl.dat").exists();
    let has_snapshot =
        dir.join("v8_context_snapshot.bin").exists() || dir.join("snapshot_blob.bin").exists();
    has_icu && has_snapshot
}

/// A Chromium target we launched: an isolated profile and a debug port, both owned by us. The child
/// is terminated and the temp profile removed on [`shutdown`], since (unlike a native target) this is
/// our own instance, not the user's running app.
pub struct LaunchedChromium {
    child: Child,
    pub port: u16,
    user_data_dir: PathBuf,
}

impl LaunchedChromium {
    /// Whether the launched instance is still running. Reaps it if it has exited (so a later shutdown
    /// is a clean no-op). Lets the driver end the session when the user closes the app.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Terminate the launched instance and remove its temp profile. Best-effort: a QA tool must not
    /// leave a process or temp files behind, but a cleanup hiccup is not worth failing the session.
    pub fn shutdown(self) {
        let _ = self.shutdown_with_residue();
    }

    /// Like [`shutdown`] but reports cleanup residue: an empty vec means the process was terminated and
    /// the temp profile removed cleanly; a non-empty vec names what was left behind (e.g. the profile
    /// could not be removed because the OS had not released its file handles yet) so a session can
    /// report it honestly via `ended.residue_keys` instead of leaving a silent mess (untouchable rules
    /// 4 and 6).
    pub fn shutdown_with_residue(mut self) -> Vec<String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Chromium's own child processes (renderer, GPU) briefly outlive the main process we killed and
        // keep file locks on the profile, so an immediate remove races and fails. They self-terminate a
        // few hundred ms after the parent dies (broken IPC channel), so retry over ~half a second, each
        // attempt continuing to clear whatever the previous one could not. Only if the profile is STILL
        // on disk after the retries do we report it honestly, rather than pretend it was clean or
        // false-alarm on a dir that vanished just after the last attempt (rules 4, 6). The common case
        // succeeds on the first try with no delay.
        for attempt in 0..5 {
            if !self.user_data_dir.exists() {
                return Vec::new();
            }
            if std::fs::remove_dir_all(&self.user_data_dir).is_ok() {
                return Vec::new();
            }
            if attempt < 4 {
                std::thread::sleep(Duration::from_millis(120));
            }
        }
        if self.user_data_dir.exists() {
            vec!["cleanup.chromium_profile_left".to_string()]
        } else {
            Vec::new()
        }
    }
}

/// Launch a Chromium/Electron target with an isolated profile and an auto-assigned debug port, then
/// wait for its `DevToolsActivePort` file and return the resolved port. The isolated `--user-data-dir`
/// sidesteps single-instance apps (a fresh profile is a new instance) and never touches the user's
/// real profile; `--remote-debugging-port=0` lets Chromium choose a free port (no collision) and
/// record it in the file. Fails loudly if the port never appears (remote debugging disabled, or not
/// actually a Chromium app) rather than pretending the session started.
pub fn launch_chromium(target: &str, args: &[String]) -> io::Result<LaunchedChromium> {
    sweep_orphan_profiles();
    let user_data_dir = unique_temp_dir();
    std::fs::create_dir_all(&user_data_dir)?;

    let mut cmd = Command::new(target);
    cmd.arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg("--remote-debugging-port=0")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&user_data_dir);
            return Err(io::Error::new(e.kind(), format!("cannot launch chromium target '{target}': {e}")));
        }
    };

    let port_file = user_data_dir.join("DevToolsActivePort");
    let mut deadline = Instant::now() + Duration::from_secs(15);
    // Set once the process we spawned has exited, so its code can go into the error message.
    let mut child_exit: Option<String> = None;
    loop {
        if let Some(port) = read_active_port(&port_file) {
            return Ok(LaunchedChromium { child, port, user_data_dir });
        }
        // A target that dies immediately - wrong flags, not a Chromium app after all, a crash on
        // startup - used to cost the full 15 s and then a guess for an error message. Watch the
        // child instead. It does NOT end the wait outright: some launchers exit after handing off to
        // another process, which is exactly the shape that would still open the port. So the exit
        // shortens the wait to a grace period rather than failing on the spot, and names the exit
        // code if the port never appears.
        if child_exit.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                child_exit = Some(match status.code() {
                    Some(c) => format!(" (it exited with code {c})"),
                    None => " (it exited)".to_string(),
                });
                let grace = Instant::now() + Duration::from_secs(2);
                if grace < deadline {
                    deadline = grace;
                }
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&user_data_dir);
            let detail = child_exit.unwrap_or_default();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "chromium did not open a debug port{detail} \
                     (remote debugging disabled, or not a Chromium app?)"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The first line of `DevToolsActivePort` is the port; a `0` there means "not chosen yet".
fn read_active_port(port_file: &Path) -> Option<u16> {
    let contents = std::fs::read_to_string(port_file).ok()?;
    let port: u16 = contents.lines().next()?.trim().parse().ok()?;
    (port != 0).then_some(port)
}

/// Best-effort sweep of profile directories a force-killed driver left behind (P3, pre-release audit).
///
/// Only profiles whose OWNING DRIVER is gone are removed. The earlier version sweeps every
/// `chrono-cdp-*` directory and relied on "a live profile is locked, so removal fails" - which is
/// only half true: `remove_dir_all` walks a Windows directory file by file and stops at the first
/// locked one, after deleting everything it reached. Chromium keeps handles on a few profile files
/// (LOCK, Cookies, part of Local Storage) but not on the hundreds of others, so a PARALLEL live
/// session would be gutted mid-run - non-deterministic behaviour in the app under test, which is the
/// worst possible failure for a tool whose output is evidence. Nothing else enforces one CDP session
/// at a time (the native lock does not cover this path), so parallel sessions are allowed by the rest
/// of the code and must be respected here.
///
/// The directory name carries the driver's pid (`chrono-cdp-<pid>-<nanos>`), so ownership is
/// readable. A recycled pid reads as alive and the directory is left alone - stale bytes on disk
/// beat destroying a live session's profile. A name that does not parse is left alone too.
fn sweep_orphan_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        match owner_pid_of_profile(name) {
            Some(pid) if !chrono_mech::process_is_alive(pid) => {
                let _ = std::fs::remove_dir_all(&path); // best effort - a locked leftover survives
            }
            _ => {} // not ours, unparseable, or owned by a live driver: leave it
        }
    }
}

/// The driver pid encoded in a profile directory name (`chrono-cdp-<pid>-<nanos>`), or `None` when
/// the name is not one of ours or does not carry a readable pid.
fn owner_pid_of_profile(name: &str) -> Option<u32> {
    name.strip_prefix("chrono-cdp-")?.split('-').next()?.parse().ok()
}

fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("chrono-cdp-{}-{}", std::process::id(), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_chromium_folder_by_its_runtime_files() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("icudtl.dat"), b"x").unwrap();
        std::fs::write(dir.join("v8_context_snapshot.bin"), b"x").unwrap();
        let exe = dir.join("App.exe");
        std::fs::write(&exe, b"x").unwrap();
        assert!(is_chromium_target(exe.to_str().unwrap()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_plain_folder_is_not_a_chromium_target() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("native.exe");
        std::fs::write(&exe, b"x").unwrap();
        assert!(!is_chromium_target(exe.to_str().unwrap()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_the_port_from_a_devtools_file() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("DevToolsActivePort");
        std::fs::write(&f, "51234\n/devtools/browser/abc\n").unwrap();
        assert_eq!(read_active_port(&f), Some(51234));
        std::fs::write(&f, "0\n").unwrap();
        assert_eq!(read_active_port(&f), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// S-5, the naming half: ownership has to be readable out of the directory name, or the sweep
    /// cannot tell a dead driver's leftovers from a live parallel session's profile.
    #[test]
    fn a_profile_directory_names_its_owning_driver() {
        assert_eq!(owner_pid_of_profile("chrono-cdp-4321-99887766"), Some(4321));
        // Our own generator must stay parseable - the two are a pair.
        let mine = unique_temp_dir();
        let name = mine.file_name().unwrap().to_str().unwrap();
        assert_eq!(owner_pid_of_profile(name), Some(std::process::id()));
        // Anything else is left alone rather than guessed at.
        assert_eq!(owner_pid_of_profile("chrono-cdp-notapid-1"), None);
        assert_eq!(owner_pid_of_profile("chrome-user-data"), None);
        assert_eq!(owner_pid_of_profile("chrono-cdp-"), None);
    }

    /// S-5, the decision half. The sweep must remove a dead driver's profile and keep one whose
    /// driver is still running - the old version removed every `chrono-cdp-*` directory it could
    /// walk into, which gutted a parallel live session's profile file by file.
    #[test]
    fn the_sweep_spares_a_live_drivers_profile_and_removes_a_dead_ones() {
        // This process is alive by definition, so a directory named after it stands for a parallel
        // session. Pid 0 is never a live user process, so it stands for a dead driver's leftovers.
        let live = std::env::temp_dir().join(format!("chrono-cdp-{}-sweeptest", std::process::id()));
        let dead = std::env::temp_dir().join("chrono-cdp-0-sweeptest");
        for d in [&live, &dead] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("Preferences"), b"{}").unwrap();
        }
        sweep_orphan_profiles();
        let live_kept = live.exists();
        let dead_gone = !dead.exists();
        std::fs::remove_dir_all(&live).ok();
        std::fs::remove_dir_all(&dead).ok();
        assert!(live_kept, "a live driver's profile must survive the sweep");
        assert!(dead_gone, "a dead driver's profile is what the sweep is for");
    }
}

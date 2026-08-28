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
    pub fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.user_data_dir);
    }
}

/// Launch a Chromium/Electron target with an isolated profile and an auto-assigned debug port, then
/// wait for its `DevToolsActivePort` file and return the resolved port. The isolated `--user-data-dir`
/// sidesteps single-instance apps (a fresh profile is a new instance) and never touches the user's
/// real profile; `--remote-debugging-port=0` lets Chromium choose a free port (no collision) and
/// record it in the file. Fails loudly if the port never appears (remote debugging disabled, or not
/// actually a Chromium app) rather than pretending the session started.
pub fn launch_chromium(target: &str, args: &[String]) -> io::Result<LaunchedChromium> {
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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(port) = read_active_port(&port_file) {
            return Ok(LaunchedChromium { child, port, user_data_dir });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&user_data_dir);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "chromium did not open a debug port (remote debugging disabled, or not a Chromium app?)",
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
}

//! Detecting a Chromium/Electron target and launching it under our control (slice C2). Unlike a
//! native session - which injects a hook into whatever the user runs - the CDP mechanism OWNS the
//! instance it drives: it launches with an isolated profile and a debug port, and tears both down at
//! the end (chrono-mock 8.8 - leave nothing behind).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono_mech::{PlainChild, Target, TargetStdio};

/// Where a Chromium target keeps the Chromium runtime, which is what makes it one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromiumLayout {
    /// Beside the exe, as an Electron application or a portable Chromium ships it.
    Beside,
    /// In a folder named after its version beside the exe, as an installed Chromium browser keeps it.
    Installed,
}

/// Whether the target exe looks like a Chromium/Electron app - see [`chromium_layout`].
pub fn is_chromium_target(target: &str) -> bool {
    chromium_layout(target).is_some()
}

/// How the target exe looks like a Chromium/Electron app, if it does: its folder ships the Chromium
/// runtime (`icudtl.dat` plus a V8 snapshot). Electron additionally carries `resources/app.asar`, but
/// the runtime files alone are a strong signature that is stable across Chromium/Electron versions.
///
/// An installed Chromium browser keeps the runtime in a folder named after its version beside the exe
/// (`150.0.1234.56\` beside `chrome.exe`, measured on two installed browsers), together with the
/// library the exe loads the browser from: Chromium's own `chrome.dll`, or one named after the exe
/// (`browser.dll` beside `browser.exe`). Looked for only beside the exe, so such a browser ran in a
/// native session: it left the session within seconds and went on on the real clock while the report
/// said `works`. The library is part of the signature, so an application that keeps a Chromium runtime
/// of its own in a version folder is not taken for one.
pub fn chromium_layout(target: &str) -> Option<ChromiumLayout> {
    let exe = Path::new(target);
    let dir = exe.parent()?;
    let holds_runtime = |folder: &Path| {
        folder.join("icudtl.dat").exists()
            && (folder.join("v8_context_snapshot.bin").exists() || folder.join("snapshot_blob.bin").exists())
    };
    if holds_runtime(dir) {
        return Some(ChromiumLayout::Beside);
    }
    let own_library = exe.file_stem().map(|stem| format!("{}.dll", stem.to_string_lossy()));
    let installed = version_folders(dir).any(|folder| {
        holds_runtime(&folder)
            && (folder.join("chrome.dll").exists() || own_library.as_ref().is_some_and(|dll| folder.join(dll).exists()))
    });
    installed.then_some(ChromiumLayout::Installed)
}

/// The folders in `dir` named after a version - digits and dots only, as `150.0.1234.56`. A folder
/// that cannot be read has none.
fn version_folders(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir()) && entry.file_name().to_str().is_some_and(is_version_name)
        })
        .map(|entry| entry.path())
}

/// Digits in at least two parts split by dots, every part non-empty: `150.0.1234.56`, not `app-1.2.3`.
fn is_version_name(name: &str) -> bool {
    name.contains('.') && name.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether a Chromium target is an Electron application: its `resources` folder holds the packed app
/// (`app.asar`), the unpacked one (`app`), or Electron's own default app (`default_app.asar`, the bare
/// runtime). Its main process then runs JavaScript in Node, outside every page the session reaches, on
/// the real clock (R4-S19) - the report says so.
pub fn is_electron_target(target: &str) -> bool {
    let Some(dir) = Path::new(target).parent() else {
        return false;
    };
    let resources = dir.join("resources");
    resources.join("app.asar").exists() || resources.join("app").is_dir() || resources.join("default_app.asar").exists()
}

/// A Chromium target we launched: an isolated profile and a debug port, both owned by us. The child
/// is terminated and the temp profile removed on [`shutdown`], since (unlike a native target) this is
/// our own instance, not the user's running app.
pub struct LaunchedChromium {
    /// The browser, in a job that ends it and everything it starts when the job closes - which the
    /// kernel does for a killed core as well (see `chrono_mech::PlainChild`).
    child: PlainChild,
    pub port: u16,
    /// The browser endpoint's path from `DevToolsActivePort`, which the connection takes instead of
    /// asking `/json/version` (R5-WY9). Checked where it is used, because the browser wrote it.
    pub ws_path: Option<String>,
    user_data_dir: PathBuf,
    /// Set by [`cleanup`], read by [`Drop`]. Two jobs, and the second is the reason it exists: it
    /// keeps the drop from repeating work that already ran, and it keeps the drop from removing a
    /// profile AFTER the session has reported that profile as left behind - which would make the
    /// evidence a lie about the one thing it is there to record (untouchable rule 4).
    cleaned: bool,
}

impl LaunchedChromium {
    /// Whether the process the session launched is still running. It does not end the session: a
    /// launcher that hands the application over to another process and exits leaves the browser it
    /// started running, and that browser holds the debugging connection the session lives on (R4-S18).
    /// The session ends when that connection closes.
    pub fn is_running(&mut self) -> bool {
        self.child.is_alive()
    }

    /// The pid of the process the session launched - the target itself, which may be a launcher.
    pub fn pid(&self) -> u32 {
        self.child.pid
    }

    /// Terminate the launched instance and remove its temp profile. Best-effort: a QA tool must not
    /// leave a process or temp files behind, but a cleanup hiccup is not worth failing the session.
    pub fn shutdown(self) {
        let _ = self.shutdown_with_residue();
    }

    /// Like [`shutdown`] but reports cleanup residue: an empty vec means the process was terminated and
    /// the temp profile removed cleanly - a non-empty vec names what was left behind (e.g. the profile
    /// could not be removed because the OS had not released its file handles yet) so a session can
    /// report it honestly via `ended.residue_keys` instead of leaving a silent mess (untouchable rules
    /// 4 and 6).
    pub fn shutdown_with_residue(mut self) -> Vec<String> {
        self.cleanup()
    }

    /// The cleanup itself, idempotent so the explicit shutdown above and the [`Drop`] net below
    /// cannot both act on the same profile. The first caller does the work and gets the residue -
    /// any later one gets an empty vec, because by then there is nothing left to report on.
    fn cleanup(&mut self) -> Vec<String> {
        if self.cleaned {
            return Vec::new();
        }
        self.cleaned = true;
        // The whole job, not only the process we started, and the wait is for all of it. Some launchers
        // exit after handing off to another process - an installed Chromium browser does it as it
        // starts - and ending or waiting for a launcher that already exited ends and waits for nothing:
        // the profile was then removed while the browser was still closing its files, and left behind
        // in about one session of eight (measured 2026-11-09).
        let ended = self.child.terminate_and_wait(PROCESS_END_WAIT_MS);
        cleanup_residue(ended, remove_profile(&self.user_data_dir))
    }
}

/// What the cleanup left behind: the profile when it could not be removed, and the browser when its
/// processes could not be confirmed to have ended. A profile removed after all says nothing about them,
/// so the session would have said `clean` over a browser it never saw end (rule 6). The removal is
/// tried either way, because a process that did end has let the profile go.
fn cleanup_residue(browser_ended: bool, mut profile: Vec<String>) -> Vec<String> {
    if !browser_ended {
        profile.push("cleanup.chromium_process_left".to_string());
    }
    profile
}

/// Remove a profile directory, and name what is left of it. Chromium's own child processes (renderer,
/// GPU) briefly outlive the main process we killed and keep file locks on the profile, so an immediate
/// remove races and fails. They self-terminate a few hundred ms after the parent dies (broken IPC
/// channel), so retry over ~half a second, each attempt continuing to clear whatever the previous one
/// could not. Only if the profile is STILL on disk after the retries do we report it honestly, rather
/// than pretend it was clean or false-alarm on a dir that vanished just after the last attempt (rules 4,
/// 6). The common case succeeds on the first try with no delay.
fn remove_profile(dir: &Path) -> Vec<String> {
    for attempt in 0..5 {
        if !dir.exists() {
            return Vec::new();
        }
        if std::fs::remove_dir_all(dir).is_ok() {
            return Vec::new();
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    if dir.exists() { vec!["cleanup.chromium_profile_left".to_string()] } else { Vec::new() }
}

/// A launch that did not become a session, and what its cleanup left behind. A browser that never
/// opened its port was ended and its profile removed the way the end of a session does it (R5-Z1): the
/// timeout used to end the process alone, try the removal once and drop the outcome, and the session
/// then said `clean: true` over a profile that may hold the app's cookies.
#[derive(Debug)]
pub struct LaunchFailed {
    pub error: io::Error,
    /// `ended.residue_keys` for the start that failed, empty when nothing was left.
    pub residue: Vec<String>,
}

impl std::fmt::Display for LaunchFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl From<io::Error> for LaunchFailed {
    fn from(error: io::Error) -> LaunchFailed {
        LaunchFailed { error, residue: Vec::new() }
    }
}

/// The net under every other way out, for the same stated reason `Session` and `SessionLock` have
/// one in `chrono-mech`: the first `?` or early `return` added next to an explicit cleanup call
/// would leak - silently. This type holds heavier resources than either of those two, a live child
/// process and a temp directory, and `std::process::Child` deliberately does NOT kill on drop.
/// Every exit path out of `cdp_session` calls `shutdown*` today - this makes the cleanup a property
/// of the type instead of a property of the current shape of one function.
///
/// What it does NOT cover, so nobody reads more into it than it gives: a force-killed core. No
/// destructor runs on `TerminateProcess`, and that path is real - measured by killing the core
/// mid-session, after which the launched Electron app was still running with its debug port open, and
/// its profile survived until the NEXT session's orphan sweep removed it. That case is covered by the
/// job the browser runs in (`chrono_mech::PlainChild`), which lives in the kernel precisely because no
/// code of ours runs to be given the chance. This drop still owns the profile directory, which the job
/// knows nothing about.
impl Drop for LaunchedChromium {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// How long the cleanup waits for the browser and every process of its job to end once the job has
/// been told to end them, in real milliseconds. The profile can only be removed once the processes
/// holding it are gone.
const PROCESS_END_WAIT_MS: u32 = 2_000;

/// The switches that stop a Chromium engine slowing the timers of a minimized, hidden or covered
/// window (R4-N28). Measured on Chromium 154 without Chrono Mock: a 16 ms timer in a minimized window
/// ran 11 to 15 times in ten seconds without them and 625 times with them - as often as in a visible
/// one. One list for the browser of a Chromium target and the engines inside an application.
pub const BACKGROUND_TIMER_SWITCHES: [&str; 3] = [
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding",
    "--disable-backgrounding-occluded-windows",
];

/// The tester's own arguments for the browser, with [`BACKGROUND_TIMER_SWITCHES`] ahead of them when the
/// session keeps timers at speed in hidden windows. Ahead, so a switch of the tester's own on the same
/// subject comes later and is the one Chromium keeps.
pub fn with_background_timers(args: &[String], keep: bool) -> Vec<String> {
    let switches = BACKGROUND_TIMER_SWITCHES.iter().filter(|_| keep).map(|s| (*s).to_string());
    switches.chain(args.iter().cloned()).collect()
}

/// The switch that keeps an installed browser from opening its first-run windows on the session's
/// fresh profile. Without it every session opened a welcome window, and in the EU a choice that holds
/// the browser's window, in front of the page (measured 2026-11-09).
const NO_FIRST_RUN: &str = "--no-first-run";

/// The arguments the browser starts with: the isolated profile and a debug port Chromium picks
/// itself, then [`NO_FIRST_RUN`] for an installed browser, ahead of the user's own. An Electron
/// application is not given it - it would arrive among the application's own arguments, which the
/// application reads. Separated from [`launch_chromium`] so they can be checked without a Chromium on
/// the machine - there is no portable one to test against.
fn chromium_arguments(user_data_dir: &Path, layout: Option<ChromiumLayout>, args: &[String]) -> Vec<String> {
    let mut all = vec![format!("--user-data-dir={}", user_data_dir.display()), "--remote-debugging-port=0".to_string()];
    if layout == Some(ChromiumLayout::Installed) {
        all.push(NO_FIRST_RUN.to_string());
    }
    all.extend(args.iter().cloned());
    all
}

/// What the mechanism starts for the browser. It goes through `chrono_mech::launch_plain`, which puts
/// it in its job before its first instruction (R4-N30): the job used to be joined after the start, and
/// whatever the browser started in between stayed out of it. `std::process::Command` could not do
/// better on stable Rust, which offers neither the thread handle to resume a suspended start nor an
/// attribute list. Its output goes to NUL, as it did.
///
/// The working directory is the one the caller asked for. The wire has carried `cwd` since the
/// protocol was written and the native mechanism has always honoured it - this path did not, so a
/// folder set in the panel or on the command line was silently ignored for exactly the targets the
/// CDP mechanism owns (rule 6). It does NOT touch the profile: that stays in our own temp directory.
fn chromium_target<'a>(target: &'a str, arguments: &'a [String], cwd: Option<&'a str>) -> Target<'a> {
    Target { path: target, args: arguments, cwd, env: &[], stdio: TargetStdio::Discarded }
}

/// How long to wait for a launched Chromium to publish its debug port. Named so the client-side
/// watchdog can be checked against it rather than guessed at (see `RustTimeoutMirrorTests`).
pub const PORT_WAIT_SECS: u64 = 15;

/// Launch a Chromium/Electron target with an isolated profile and an auto-assigned debug port, then
/// wait for its `DevToolsActivePort` file and return the resolved port. The isolated `--user-data-dir`
/// sidesteps single-instance apps (a fresh profile is a new instance) and never touches the user's
/// real profile - `--remote-debugging-port=0` lets Chromium choose a free port (no collision) and
/// record it in the file. Fails loudly if the port never appears (remote debugging disabled, or not
/// actually a Chromium app) rather than pretending the session started.
///
/// `on_wait` is called on every pass of the wait loop (every [`PORT_POLL`]). It exists because this
/// wait is the longest stretch of silence in a CDP session: the caller has accepted `start` and can
/// emit nothing until the port appears, so a client watching for liveness cannot tell a browser
/// that is still unpacking from a core that has died. Beating the session heartbeat from here
/// removes that race without shortening the wait, which is the part a slow Electron needs (R3-3).
pub fn launch_chromium(
    target: &str,
    args: &[String],
    cwd: Option<&str>,
    mut on_wait: impl FnMut(),
) -> Result<LaunchedChromium, LaunchFailed> {
    // A user-supplied --user-data-dir would win over ours (Chromium takes the last one), and the
    // session would then run on the REAL profile of a real browser - the one thing the isolated
    // profile exists to prevent, and something the tester was told in writing does not happen
    // (R2-N8). Refused rather than silently overridden: we cannot honour the flag and the promise
    // at the same time, so we say which one we are keeping.
    if let Some(bad) = args.iter().find(|a| {
        let a = a.trim_start_matches('-').to_ascii_lowercase();
        a.starts_with("user-data-dir") || a.starts_with("remote-debugging-port")
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "'{bad}' collides with the isolated profile and debug port this session needs - \
                 remove it from --args"
            ),
        )
        .into());
    }

    sweep_orphan_profiles(&std::env::temp_dir());
    let user_data_dir = unique_temp_dir();
    create_profile_dir(&user_data_dir)?;

    let arguments = chromium_arguments(&user_data_dir, chromium_layout(target), args);
    let child = match chrono_mech::launch_plain(&chromium_target(target, &arguments, cwd)) {
        Ok(c) => c,
        Err(e) => {
            let residue = remove_profile(&user_data_dir);
            let error = io::Error::other(format!("cannot launch chromium target '{target}': {e}"));
            return Err(LaunchFailed { error, residue });
        }
    };
    // A browser the system would not start in a job still makes a session that works - it only loses
    // the guarantee, and that is said rather than left out (rule 6). Not `eprintln!`, which panics on a
    // closed standard error (R4-S11, tests/hygiene.rs H10), and this module names nothing in the crate,
    // so it writes the line itself.
    if let Some(why) = child.job_note() {
        let _ = writeln!(io::stderr(), "chrono: the browser runs without a job ({why}) - a core that is killed leaves it running");
    }

    let port_file = user_data_dir.join("DevToolsActivePort");
    let mut deadline = Instant::now() + Duration::from_secs(PORT_WAIT_SECS);
    // Set once the process we spawned has exited, so its code can go into the error message.
    let mut child_exit: Option<String> = None;
    loop {
        if let Some((port, ws_path)) = read_active_port(&port_file) {
            return Ok(LaunchedChromium { child, port, ws_path, user_data_dir, cleaned: false });
        }
        // A target that dies immediately - wrong flags, not a Chromium app after all, a crash on
        // startup - used to cost the full 15 s and then a guess for an error message. Watch the
        // child instead. It does NOT end the wait outright: some launchers exit after handing off to
        // another process, which is exactly the shape that would still open the port. So the exit
        // shortens the wait to a grace period rather than failing on the spot, and names the exit
        // code if the port never appears.
        if child_exit.is_none()
            && let Some(code) = child.wait_exit(0) {
                child_exit = Some(format!(" (it exited with code {code})"));
                let grace = Instant::now() + Duration::from_secs(2);
                if grace < deadline {
                    deadline = grace;
                }
            }
        if Instant::now() >= deadline {
            let detail = child_exit.unwrap_or_default();
            let error = io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "chromium did not open a debug port{detail} \
                     (remote debugging disabled, or not a Chromium app?)"
                ),
            );
            return Err(give_up(child, user_data_dir, error));
        }
        on_wait();
        std::thread::sleep(PORT_POLL);
    }
}

/// A launch that waited for the port in vain, ended the way a session ends: the whole job, then the
/// profile with its retries - what a launcher started in the meantime holds the profile too (R5-Z1).
fn give_up(child: PlainChild, user_data_dir: PathBuf, error: io::Error) -> LaunchFailed {
    let residue = LaunchedChromium { child, port: 0, ws_path: None, user_data_dir, cleaned: false }.shutdown_with_residue();
    LaunchFailed { error, residue }
}

/// What `DevToolsActivePort` says: the port on the first line - a `0` there means "not chosen yet" -
/// and the browser endpoint's path on the second, when there is one.
fn read_active_port(port_file: &Path) -> Option<(u16, Option<String>)> {
    active_port_from(&std::fs::read_to_string(port_file).ok()?)
}

/// What the text of a `DevToolsActivePort` file says - [`read_active_port`] without the file.
fn active_port_from(contents: &str) -> Option<(u16, Option<String>)> {
    let mut lines = contents.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next().map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    (port != 0).then_some((port, path))
}

/// How often the launch looks for `DevToolsActivePort` (R5-P7). The browser creates its first window
/// right after it writes the file, and a page that runs before auto-attach is armed is not paused for
/// the shim: measured on Electron 44, polling every 150 ms found the file about 60 ms later than every
/// 10 ms did, and the page's first script ran in that gap. A read of a file that is not there yet,
/// a hundred times a second for at most [`PORT_WAIT_SECS`].
const PORT_POLL: Duration = Duration::from_millis(10);

/// Best-effort sweep of profile directories a force-killed driver left behind (P3, pre-release audit).
///
/// `temp` is the directory to walk, which in production is always `std::env::temp_dir()` - the same
/// one [`unique_temp_dir`] puts profiles in, and the call site keeps the two on adjacent lines. It
/// is a parameter rather than a lookup inside so that the test can point the sweep at a directory of
/// its own: a sweep aimed at the shared `%TEMP%` removes whatever it finds there, which made the
/// test both flaky and destructive against a real session's leftovers.
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
fn sweep_orphan_profiles(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        // Name first, `is_dir` second, and the order is the whole point. The listing already carries
        // the name, but `Path::is_dir` opens the entry to read its metadata - one syscall per entry -
        // and %TEMP% is where every program on the machine drops its scratch. Measured on this
        // machine's %TEMP% (31 633 entries), interleaved A/B, 5 pairs: stat-first 3 280 ms,
        // name-first 25 ms. That is 3.3 seconds added to the start of EVERY Chromium session, spent
        // statting other people's temp files, when only our own `chrono-cdp-<pid>-<nanos>` names can
        // ever match. Same directories removed either way - this only stops asking the filesystem
        // about entries whose name already ruled them out.
        let raw_name = entry.file_name();
        let Some(name) = raw_name.to_str() else {
            continue;
        };
        let Some(pid) = owner_pid_of_profile(name) else {
            continue; // not ours, or the name carries no readable pid: leave it
        };
        if !chrono_mech::process_is_alive(pid) {
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path); // best effort - a locked leftover survives
            }
        }
    }
}

/// The driver pid encoded in a profile directory name (`chrono-cdp-<pid>-<nanos>`), or `None` when
/// the name is not one of ours or does not carry a readable pid.
fn owner_pid_of_profile(name: &str) -> Option<u32> {
    name.strip_prefix("chrono-cdp-")?.split('-').next()?.parse().ok()
}

/// Create the isolated profile directory, failing if something is already sitting on the name.
///
/// `create_dir_all` succeeds on a directory that already exists, so anything pre-created under our
/// name - a directory, or a junction pointing elsewhere - was silently adopted, and the browser
/// profile for the session landed wherever that thing pointed. `create_dir` refuses instead, which
/// turns a substituted profile into a loud failure rather than a session that quietly ran somewhere
/// else. The parent is still created leniently: a missing `%TEMP%` is a broken machine, not an
/// attack, and refusing there would be a regression for no security gain.
///
/// The name keeps `<pid>-<nanos>` and does NOT get a random component. That was considered and
/// rejected: `std`'s only randomness (`RandomState`) documents no source and no unpredictability
/// guarantee, so building a security argument on it would be a claim without backing, and pulling in
/// `BCryptGenRandom` costs a new `windows` feature for a threat that is already closed here - with
/// `create_dir`, guessing the name gets an error rather than an adopted directory. `%TEMP%` is
/// per-user on Windows besides.
fn create_profile_dir(dir: &std::path::Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(dir)
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

    /// The isolated profile must be a directory WE made. `create_dir_all` adopted whatever was
    /// already sitting on the name, so a pre-created directory (or a junction pointing elsewhere)
    /// became the session's browser profile without a word.
    /// Removes its directory even when an assertion above it fails. A plain call at the end of the
    /// test does not run on the failing path, and the first version of this test proved it: a revert
    /// run left `chrono-cdp-test-<pid>` sitting in %TEMP%, under a name close enough to the real one
    /// to be mistaken for a leaked session profile.
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_profile_directory_that_already_exists_is_refused_not_adopted() {
        // Deliberately NOT the `chrono-cdp-<pid>-<nanos>` shape: this is a test fixture, and it has
        // no business looking like a session profile to `sweep_orphan_profiles` or to a human
        // reading %TEMP%.
        let dir = std::env::temp_dir().join(format!("chrono-profile-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = TempDirGuard(dir.clone());

        create_profile_dir(&dir).expect("first create should succeed");
        assert!(dir.is_dir());

        let second = create_profile_dir(&dir);
        assert!(second.is_err(), "an existing directory was adopted instead of refused");

        drop(guard);
        assert!(!dir.exists(), "the guard left its fixture behind");
    }

    /// A long-lived stand-in for the launched browser, started the way the browser is. `ping` is on
    /// every Windows box and needs no shell, so the process we start IS the child we hold - a
    /// `cmd /c ...` wrapper would leave the real worker running when the wrapper is ended, which is
    /// exactly the confusion these tests must not have. Named by its full path, because the launch
    /// does not search `PATH`, and a browser target is always a file the caller found.
    fn spawn_placeholder_child() -> PlainChild {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let ping = Path::new(&root).join("System32").join("PING.EXE").display().to_string();
        let args = ["-n".to_string(), "30".to_string(), "127.0.0.1".to_string()];
        chrono_mech::launch_plain(&Target { path: &ping, args: &args, cwd: None, env: &[], stdio: TargetStdio::Discarded })
            .expect("ping should start on Windows")
    }

    fn process_is_running(pid: u32) -> bool {
        chrono_mech::process_is_alive(pid)
    }

    /// The net. An instance that goes out of scope without an explicit shutdown must still end the
    /// process and remove the profile. Every exit path out of `cdp_session` calls `shutdown*` today,
    /// so this guards the NEXT early return someone adds - the same reason `Session` and
    /// `SessionLock` in `chrono-mech` release in `Drop` rather than only in a named method.
    #[test]
    fn dropping_a_launched_instance_ends_the_process_and_removes_the_profile() {
        let dir = std::env::temp_dir().join(format!("chrono-drop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = TempDirGuard(dir.clone());
        std::fs::create_dir_all(&dir).expect("fixture profile");

        let child = spawn_placeholder_child();
        let pid = child.pid;
        assert!(process_is_running(pid), "the placeholder child never started");

        drop(LaunchedChromium { child, port: 1, ws_path: None, user_data_dir: dir.clone(), cleaned: false });

        assert!(!process_is_running(pid), "the drop left the launched process running");
        assert!(!dir.exists(), "the drop left the profile directory behind");
        drop(guard);
    }

    /// R5-Z1: a launch that gave up waiting for the port ends the browser and removes its profile the
    /// way a session's end does, and what it cannot remove it names. The timeout used to try once, drop
    /// the outcome, and the session said `clean: true` over the profile left in %TEMP%.
    #[test]
    fn a_launch_that_gives_up_ends_the_browser_and_names_the_profile_it_could_not_remove() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("chrono-give-up-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = TempDirGuard(dir.clone());
        std::fs::create_dir_all(&dir).expect("fixture profile");

        // A file held open with no sharing, as a renderer holds the cookies: the profile cannot go.
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(dir.join("Cookies"))
            .expect("held file");
        let child = spawn_placeholder_child();
        let pid = child.pid;
        let failed = give_up(child, dir.clone(), io::Error::from(io::ErrorKind::TimedOut));
        assert!(!process_is_running(pid), "the browser was left running");
        assert_eq!(failed.error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(failed.residue, vec!["cleanup.chromium_profile_left".to_string()], "the profile left behind is named");
        drop(held);

        // Nothing holding it: the profile goes, and nothing is named.
        let child = spawn_placeholder_child();
        let failed = give_up(child, dir.clone(), io::Error::from(io::ErrorKind::TimedOut));
        assert!(failed.residue.is_empty(), "{:?}", failed.residue);
        assert!(!dir.exists(), "the profile was left behind");
        drop(guard);
    }

    /// The CDP path ignored `cwd` entirely: the wire carried it, `chrono-mech` honoured it, and a
    /// folder set in the panel or with `--cwd` did nothing for exactly the targets this mechanism
    /// owns. Silently - which is the part that makes it worth a test rather than a comment.
    #[test]
    fn the_launch_starts_the_browser_in_the_requested_folder() {
        let arguments = chromium_arguments(Path::new("C:/temp/profile"), Some(ChromiumLayout::Beside), &[]);
        assert_eq!(chromium_target("app.exe", &arguments, Some("C:/work")).cwd, Some("C:/work"));

        // Absent means "wherever we are", not an empty directory.
        assert_eq!(chromium_target("app.exe", &arguments, None).cwd, None);
    }

    /// The working directory must not disturb the isolated profile: the profile is ours, lives in our
    /// own temp directory, and a target that starts elsewhere still gets that same profile. The user's
    /// own arguments come after ours, where Chromium reads them - a user profile flag never gets this
    /// far (see the refusal test below).
    #[test]
    fn a_working_folder_does_not_move_the_isolated_profile() {
        let arguments = chromium_arguments(Path::new("C:/temp/profile"), Some(ChromiumLayout::Beside), &["--mine".to_string()]);
        let target = chromium_target("app.exe", &arguments, Some("C:/work"));
        assert_eq!(target.args, ["--user-data-dir=C:/temp/profile", "--remote-debugging-port=0", "--mine"]);
        assert_eq!(target.stdio, TargetStdio::Discarded, "the browser's output went somewhere it was not before");
    }

    /// A browser that could not be confirmed to have ended is named even when its profile was removed:
    /// the profile says nothing about the processes, and `clean` would have said it of both.
    #[test]
    fn a_browser_not_seen_to_end_is_named_beside_the_profile() {
        assert!(cleanup_residue(true, Vec::new()).is_empty(), "ended and removed is clean");
        assert_eq!(cleanup_residue(false, Vec::new()), ["cleanup.chromium_process_left"]);
        assert_eq!(
            cleanup_residue(false, vec!["cleanup.chromium_profile_left".to_string()]),
            ["cleanup.chromium_profile_left", "cleanup.chromium_process_left"]
        );
        assert_eq!(cleanup_residue(true, vec!["cleanup.chromium_profile_left".to_string()]), ["cleanup.chromium_profile_left"]);
    }

    /// An installed browser starts without its first-run windows, which every fresh profile opened in
    /// front of the page, and the tester's own arguments still come last. An Electron application is not
    /// given the switch: it would arrive among the arguments the application reads as its own.
    #[test]
    fn only_an_installed_browser_is_started_without_its_first_run() {
        let mine = ["--mine".to_string()];
        let installed = chromium_arguments(Path::new("C:/temp/profile"), Some(ChromiumLayout::Installed), &mine);
        assert_eq!(installed, ["--user-data-dir=C:/temp/profile", "--remote-debugging-port=0", NO_FIRST_RUN, "--mine"]);
        let beside = chromium_arguments(Path::new("C:/temp/profile"), Some(ChromiumLayout::Beside), &mine);
        assert!(!beside.iter().any(|a| a == NO_FIRST_RUN), "{beside:?}");
    }

    /// The layout the launch reads the switch from: a runtime beside the exe is `Beside`, one in a
    /// version folder `Installed`, and a plain folder none.
    #[test]
    fn the_layout_says_where_the_runtime_is() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let installed = dir.join("chrome.exe");
        std::fs::write(&installed, b"x").unwrap();
        assert_eq!(chromium_layout(installed.to_str().unwrap()), None, "a plain folder");
        folder_with(&dir, "150.0.1234.56", &["icudtl.dat", "v8_context_snapshot.bin", "chrome.dll"]);
        assert_eq!(chromium_layout(installed.to_str().unwrap()), Some(ChromiumLayout::Installed));
        for runtime in ["icudtl.dat", "snapshot_blob.bin"] {
            std::fs::write(dir.join(runtime), b"x").unwrap();
        }
        assert_eq!(chromium_layout(installed.to_str().unwrap()), Some(ChromiumLayout::Beside), "beside wins");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An Electron application is told by its `resources` folder, in any of the three shapes it ships in.
    /// A Chromium runtime folder with no app in it is not one.
    #[test]
    fn an_electron_application_is_told_by_its_resources_folder() {
        let root = std::env::temp_dir().join(format!("chrono-electron-{}", std::process::id()));
        let exe = |dir: &std::path::Path| dir.join("app.exe").display().to_string();
        for (shape, make) in [("asar", "app.asar"), ("unpacked", "app"), ("default", "default_app.asar")] {
            let dir = root.join(shape);
            std::fs::create_dir_all(dir.join("resources")).expect("resources");
            if make == "app" {
                std::fs::create_dir_all(dir.join("resources").join("app")).expect("unpacked app");
            } else {
                std::fs::write(dir.join("resources").join(make), b"").expect("archive");
            }
            assert!(is_electron_target(&exe(&dir)), "{shape}");
        }
        let chromium = root.join("chromium");
        std::fs::create_dir_all(chromium.join("resources")).expect("resources");
        std::fs::write(chromium.join("resources").join("app.asar.txt"), b"").expect("a file of another name");
        assert!(!is_electron_target(&exe(&chromium)));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The switches that keep timers at speed in hidden windows go to the browser only when asked for,
    /// all three, ahead of the tester's own arguments (R4-N28).
    #[test]
    fn the_background_timer_switches_go_only_when_asked_for() {
        let mine = ["--mine".to_string()];
        assert_eq!(with_background_timers(&mine, false), mine);
        let kept = with_background_timers(&mine, true);
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[..3], BACKGROUND_TIMER_SWITCHES.map(str::to_string));
        assert_eq!(kept[3], "--mine");
    }

    /// Cleanup runs once. A second run is not merely wasted work: `shutdown_with_residue` may have
    /// just reported the profile as left behind, and a drop that removed it afterwards would turn
    /// that report into a lie about the one thing it records (untouchable rule 4).
    #[test]
    fn cleanup_runs_once_so_a_reported_leftover_stays_true() {
        let dir = std::env::temp_dir().join(format!("chrono-drop-once-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = TempDirGuard(dir.clone());
        std::fs::create_dir_all(&dir).expect("fixture profile");

        let mut inst = LaunchedChromium {
            child: spawn_placeholder_child(),
            port: 1,
            ws_path: None,
            user_data_dir: dir.clone(),
            cleaned: false,
        };
        assert!(inst.cleanup().is_empty(), "an unlocked profile should clear on the first pass");
        assert!(!dir.exists());

        // Stands in for a profile the session has already reported as left behind: if the drop below
        // cleaned up again, this directory would vanish and the report would no longer be true.
        std::fs::create_dir_all(&dir).expect("re-create fixture");
        drop(inst);
        assert!(dir.exists(), "the drop cleaned up a second time");
        drop(guard);
    }

    /// R2-N8: our isolated profile is added BEFORE the user's arguments, and Chromium takes the
    /// last --user-data-dir it is given - so a user-supplied one would have won and the session
    /// would have run on their real browser profile, which the launch doc promises never happens.
    /// The check runs before anything is spawned, so a target that does not exist is enough.
    #[test]
    fn a_user_supplied_profile_or_port_flag_is_refused_not_silently_overridden() {
        for arg in ["--user-data-dir=/home/me/real", "--remote-debugging-port=9222"] {
            match launch_chromium("no-such-app.exe", &[arg.to_string()], None, || {}) {
                Err(e) => assert_eq!(e.error.kind(), io::ErrorKind::InvalidInput, "for {arg}"),
                Ok(_) => panic!("a colliding flag must be refused: {arg}"),
            }
        }
        // An ordinary argument still passes the check (this one then fails to launch, which is a
        // different error entirely - the point is that it got that far).
        match launch_chromium("no-such-app.exe", &["--enable-logging".to_string()], None, || {}) {
            Err(e) => {
                assert_ne!(e.error.kind(), io::ErrorKind::InvalidInput);
                assert!(e.residue.is_empty(), "nothing was started, so nothing was left: {:?}", e.residue);
            }
            Ok(_) => panic!("a missing target cannot launch"),
        }
    }

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

    /// A folder `name` under `dir` holding `files`, each empty.
    fn folder_with(dir: &Path, name: &str, files: &[&str]) {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for file in files {
            std::fs::write(folder.join(file), b"x").unwrap();
        }
    }

    /// An installed Chromium browser, laid out as two installed browsers were measured (2026-11-09): the
    /// exe alone beside a folder named after its version, which holds the runtime and the library the exe
    /// loads - Chromium's own `chrome.dll`, or one named after the exe. An update can leave the previous
    /// version's folder beside the new one, and either one is enough.
    #[test]
    fn an_installed_browser_is_told_by_the_version_folder_beside_it() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let chromium = dir.join("chrome.exe");
        std::fs::write(&chromium, b"x").unwrap();
        folder_with(&dir, "150.0.1234.56", &["icudtl.dat", "v8_context_snapshot.bin", "chrome.dll"]);
        assert!(is_chromium_target(chromium.to_str().unwrap()), "Chromium's own library");

        let other_dir = dir.join("other");
        std::fs::create_dir_all(&other_dir).unwrap();
        let browser = other_dir.join("browser.exe");
        std::fs::write(&browser, b"x").unwrap();
        folder_with(&other_dir, "150.0.2345.67", &["icudtl.dat"]);
        folder_with(&other_dir, "151.0.2400.10", &["icudtl.dat", "snapshot_blob.bin", "browser.dll"]);
        assert!(is_chromium_target(browser.to_str().unwrap()), "a library named after the exe, an old version folder left beside");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What does not make a version folder a browser: the runtime without the library the exe loads (an
    /// application that keeps a Chromium runtime of its own), a folder named like an updater's
    /// (`app-1.2.3`), the library without the runtime, a file with a version's name, or a library named
    /// after neither Chromium nor the exe.
    #[test]
    fn a_version_folder_is_a_browser_only_with_the_runtime_and_the_library() {
        let cases: [(&str, &str, &[&str]); 5] = [
            ("app.exe", "1.2.3.4", &["icudtl.dat", "v8_context_snapshot.bin"]),
            ("chrome.exe", "app-1.2.3", &["icudtl.dat", "v8_context_snapshot.bin", "chrome.dll"]),
            ("chrome.exe", "150.0.1234.56", &["chrome.dll"]),
            ("chrome.exe", "150.0.1234.56", &[]),
            ("app.exe", "1.2.3.4", &["icudtl.dat", "snapshot_blob.bin", "libcef.dll"]),
        ];
        let root = unique_temp_dir();
        for (index, (exe_name, folder, files)) in cases.iter().enumerate() {
            let dir = root.join(index.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            let exe = dir.join(exe_name);
            std::fs::write(&exe, b"x").unwrap();
            if files.is_empty() {
                std::fs::write(dir.join(folder), b"x").unwrap();
            } else {
                folder_with(&dir, folder, files);
            }
            assert!(!is_chromium_target(exe.to_str().unwrap()), "{exe_name} beside {folder} holding {files:?}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_version_name_is_digits_and_dots() {
        for name in ["150.0.1234.56", "1.2", "10.0.0.0"] {
            assert!(is_version_name(name), "{name}");
        }
        for name in ["156", "app-1.2.3", "1..2", ".1.2", "1.2.", "1.2a.3", ""] {
            assert!(!is_version_name(name), "{name}");
        }
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
        assert_eq!(read_active_port(&f), Some((51234, Some("/devtools/browser/abc".to_string()))));
        // The port alone is enough - the path is asked over HTTP then.
        std::fs::write(&f, "51234\n").unwrap();
        assert_eq!(read_active_port(&f), Some((51234, None)));
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
        // A directory of our own, NOT `%TEMP%` itself. The dead profile's name has to carry a dead
        // pid and can carry nothing else, so it is the same name in every run - and `%TEMP%` is
        // shared, so two runs at once fought over that one name. Measured on this branch before the
        // fix, with a second process writing to `%TEMP%\chrono-cdp-0-sweeptest`: 26 failures in 40
        // runs, against 0 in 20 sequential ones. Aiming the sweep at a shared directory also made
        // the test destructive - one run deleted two unrelated `chrono-cdp-*` directories it had
        // never created, which in the field would be a real session's leftovers, evidence a tester
        // may still need.
        //
        // Deliberately NOT the `chrono-cdp-<pid>-<nanos>` shape: a fixture must not look like a
        // session profile to a real sweep running beside us, or to a human reading %TEMP%.
        let root = crate::testutil::unique_temp_dir(&format!("chrono-sweeptest-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // Removes the fixture even when an assertion below fails.
        let _guard = TempDirGuard(root.clone());

        // This process is alive by definition, so a directory named after it stands for a parallel
        // session. Pid 0 is never a live user process, so it stands for a dead driver's leftovers.
        let live = root.join(format!("chrono-cdp-{}-profile", std::process::id()));
        let dead = root.join("chrono-cdp-0-profile");
        for d in [&live, &dead] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("Preferences"), b"{}").unwrap();
        }

        sweep_orphan_profiles(&root);

        assert!(live.exists(), "a live driver's profile must survive the sweep");
        assert!(!dead.exists(), "a dead driver's profile is what the sweep is for");
    }

    mod properties {
        use super::*;
        use crate::cdp::is_browser_ws_path;
        use proptest::prelude::*;

        /// The first line of a port file: a port the browser chose, the zero it writes before it has
        /// chosen, or something no browser writes - an empty line, a number past the range, words.
        #[derive(Debug, Clone)]
        enum PortLine {
            Chosen(u16),
            NotYet(&'static str),
            Junk(&'static str),
        }

        fn port_line() -> impl Strategy<Value = PortLine> {
            prop_oneof![
                4 => (1u16..=u16::MAX).prop_map(PortLine::Chosen),
                1 => prop_oneof![Just("0"), Just("00")].prop_map(PortLine::NotYet),
                2 => prop_oneof![Just(""), Just("65536"), Just("99999"), Just("-1"), Just("92 22"), Just("0x1f"), Just("port")]
                    .prop_map(PortLine::Junk),
            ]
        }

        /// The second line, and whether it is a path of the shape a browser writes: an id of letters,
        /// digits and hyphens up to 64 long, or the same with something else in it or around it - a
        /// space, a carriage return inside, an escape, a slash, an id one past the limit, a page path.
        fn path_line() -> impl Strategy<Value = (Option<String>, bool)> {
            let good = "[A-Za-z0-9-]{1,64}".prop_map(|id| (Some(format!("/devtools/browser/{id}")), true));
            let bad = prop_oneof![
                ("[a-z0-9]{1,8}", prop_oneof![Just(" "), Just("\r"), Just("%0D"), Just("/"), Just("?"), Just("\t"), Just(".")], "[a-z0-9]{0,8}")
                    .prop_map(|(a, inside, b)| format!("/devtools/browser/{a}{inside}{b}")),
                "[a-z]{65,70}".prop_map(|id| format!("/devtools/browser/{id}")),
                Just("/devtools/page/abc".to_string()),
                Just("/devtools/browser/".to_string()),
                Just(String::new()),
            ]
            .prop_map(|path| (Some(path), false));
            prop_oneof![4 => good, 2 => bad, 1 => Just((None, false))]
        }

        proptest! {
            /// The port file is written by the browser, and its second line goes into the request line
            /// of the upgrade. Whatever the file says: a chosen port is read as itself and a zero as
            /// "not chosen yet", and the only path that reaches the request line is one of the shape a
            /// browser writes - read through either line ending and the blanks around it, so the path a
            /// browser did write is never lost on the way.
            #[test]
            fn a_port_file_gives_its_port_and_only_a_browser_path_reaches_the_request(
                port in port_line(),
                (path, good) in path_line(),
                blank in prop_oneof![Just(""), Just(" "), Just("\t ")],
                crlf in any::<bool>(),
                last_ends in any::<bool>(),
            ) {
                let end = if crlf { "\r\n" } else { "\n" };
                let first = match &port {
                    PortLine::Chosen(p) => p.to_string(),
                    PortLine::NotYet(text) | PortLine::Junk(text) => (*text).to_string(),
                };
                let mut contents = format!("{blank}{first}{blank}");
                if let Some(path) = &path {
                    contents.push_str(end);
                    contents.push_str(&format!("{blank}{path}{blank}"));
                }
                if last_ends {
                    contents.push_str(end);
                }
                let read = active_port_from(&contents);
                match port {
                    PortLine::Chosen(p) => prop_assert_eq!(read.as_ref().map(|r| r.0), Some(p), "for {:?}", contents),
                    PortLine::NotYet(_) | PortLine::Junk(_) => prop_assert_eq!(read.as_ref(), None, "for {:?}", contents),
                }
                let used = read.and_then(|(_, p)| p).filter(|p| is_browser_ws_path(p));
                if let Some(used) = &used {
                    let id = used.strip_prefix("/devtools/browser/").unwrap_or_default();
                    let shaped = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
                    prop_assert!(shaped, "{:?} reaches the request line from {:?}", used, contents);
                }
                if good && matches!(port, PortLine::Chosen(_)) {
                    prop_assert_eq!(used, path, "for {:?}", contents);
                }
            }
        }
    }
}

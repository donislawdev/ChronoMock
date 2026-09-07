//! Detecting a Chromium/Electron target and launching it under our control (slice C2). Unlike a
//! native session - which injects a hook into whatever the user runs - the CDP mechanism OWNS the
//! instance it drives: it launches with an isolated profile and a debug port, and tears both down at
//! the end (chrono-mock 8.8 - leave nothing behind).

use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

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
    /// The OS-level tie between the browser and this process. `None` only if the job could not be
    /// set up, in which case the session runs exactly as it did before this existed.
    job: Option<KillOnCloseJob>,
    pub port: u16,
    user_data_dir: PathBuf,
    /// Set by [`cleanup`], read by [`Drop`]. Two jobs, and the second is the reason it exists: it
    /// keeps the drop from repeating work that already ran, and it keeps the drop from removing a
    /// profile AFTER the session has reported that profile as left behind - which would make the
    /// evidence a lie about the one thing it is there to record (untouchable rule 4).
    cleaned: bool,
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
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Killing `child` is not the same as killing the browser. Some launchers exit after handing
        // off to another process - the wait loop above says so in as many words, because that shape
        // still opens the debug port - and killing a launcher that already exited ends nothing.
        // Dropping the job closes its last handle, and the OS terminates everything in it.
        drop(self.job.take());
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

/// The net under every other way out, for the same stated reason `Session` and `SessionLock` have
/// one in `chrono-mech`: the first `?` or early `return` added next to an explicit cleanup call
/// would leak - silently. This type holds heavier resources than either of those two, a live child
/// process and a temp directory, and `std::process::Child` deliberately does NOT kill on drop.
/// Every exit path out of `cdp_session` calls `shutdown*` today - this makes the cleanup a property
/// of the type instead of a property of the current shape of one function.
///
/// What it does NOT cover, so nobody reads more into it than it gives: a force-killed core. No
/// destructor runs on `TerminateProcess`, and that path is real - measured by killing the core
/// mid-session, after which the launched Pomotroid was still running with its debug port open, and
/// its profile survived until the NEXT session's orphan sweep removed it. That case is covered by
/// [`KillOnCloseJob`] instead, which lives in the kernel precisely because no code of ours runs to
/// be given the chance. This drop still owns the profile directory, which the job knows nothing
/// about.
impl Drop for LaunchedChromium {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// A job object holding the launched browser, with "kill on close" set: when the last handle to it
/// goes away, Windows terminates every process inside.
///
/// This is what covers the case [`Drop`] on `LaunchedChromium` explicitly does not - a force-killed
/// core. `TerminateProcess` runs no destructor, no `atexit`, nothing: measured by killing the core
/// mid-session, after which the launched Pomotroid was still running with its debug port open.
/// Handles, however, are closed by the kernel whatever way a process dies, so the tie has to live
/// where a dying process cannot skip it. A longer grace period in the client cannot reach this case
/// at all, since nothing in our code runs to be given the extra time.
///
/// It also covers a case that was already possible on the ordinary path: a launcher that exits after
/// handing the window off to another process. `child.kill()` then kills something that has already
/// gone, and the browser survives its own shutdown.
struct KillOnCloseJob(HANDLE);

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateJobObjectW and is closed exactly once - this type is
        // not Clone, and the only owner is the LaunchedChromium that took it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: a job handle is a kernel object usable from any thread - nothing here is thread-affine.
// Needed because the session that owns a LaunchedChromium is not pinned to one thread.
unsafe impl Send for KillOnCloseJob {}

/// Put `child` in a fresh kill-on-close job. `None` on any failure, which is deliberate: this is a
/// safety net, and a session that cannot get one is still a session that works - it simply loses the
/// guarantee, and says so on stderr rather than silently (rule 6).
fn tie_lifetime_to_ours(child: &Child) -> Option<KillOnCloseJob> {
    // SAFETY: all three calls are the documented sequence for a job object. The handle is owned by
    // KillOnCloseJob from the moment it is created, so no path leaks it - `info` outlives the call.
    unsafe {
        let job = match CreateJobObjectW(None, None) {
            Ok(h) => KillOnCloseJob(h),
            Err(e) => {
                eprintln!("chrono: could not create a job object for the browser: {e}");
                return None;
            }
        };
        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: windows::Win32::System::JobObjects::JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        if let Err(e) = SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            eprintln!("chrono: could not set kill-on-close on the browser's job object: {e}");
            return None;
        }
        if let Err(e) = AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) {
            eprintln!("chrono: could not put the browser in a job object: {e}");
            return None;
        }
        Some(job)
    }
}

/// The command that launches the browser. Separated from [`launch_chromium`] so the flags it
/// carries can be checked without a Chromium on the machine - there is no portable one to test
/// against, and the working directory in particular is a single call that nothing else would notice
/// going missing.
fn chromium_command(target: &str, user_data_dir: &Path, args: &[String], cwd: Option<&str>) -> Command {
    let mut cmd = Command::new(target);
    cmd.arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg("--remote-debugging-port=0")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The working directory the caller asked for. The wire has carried `cwd` since the protocol was
    // written and the native mechanism has always honoured it - this path did not, so a folder set in
    // the panel or on the command line was silently ignored for exactly the targets the CDP mechanism
    // owns (rule 6). It does NOT touch the profile: that stays in our own temp directory.
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd
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
/// `on_wait` is called on every pass of the wait loop (about every 150 ms). It exists because this
/// wait is the longest stretch of silence in a CDP session: the caller has accepted `start` and can
/// emit nothing until the port appears, so a client watching for liveness cannot tell a browser
/// that is still unpacking from a core that has died. Beating the session heartbeat from here
/// removes that race without shortening the wait, which is the part a slow Electron needs (R3-3).
pub fn launch_chromium(
    target: &str,
    args: &[String],
    cwd: Option<&str>,
    mut on_wait: impl FnMut(),
) -> io::Result<LaunchedChromium> {
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
        ));
    }

    sweep_orphan_profiles();
    let user_data_dir = unique_temp_dir();
    create_profile_dir(&user_data_dir)?;

    let mut cmd = chromium_command(target, &user_data_dir, args, cwd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&user_data_dir);
            return Err(io::Error::new(e.kind(), format!("cannot launch chromium target '{target}': {e}")));
        }
    };

    // Immediately after spawn, before the browser has had time to fan out into renderer and GPU
    // processes: children created after this inherit the job, children created before it would not.
    let job = tie_lifetime_to_ours(&child);

    let port_file = user_data_dir.join("DevToolsActivePort");
    let mut deadline = Instant::now() + Duration::from_secs(PORT_WAIT_SECS);
    // Set once the process we spawned has exited, so its code can go into the error message.
    let mut child_exit: Option<String> = None;
    loop {
        if let Some(port) = read_active_port(&port_file) {
            return Ok(LaunchedChromium { child, job, port, user_data_dir, cleaned: false });
        }
        // A target that dies immediately - wrong flags, not a Chromium app after all, a crash on
        // startup - used to cost the full 15 s and then a guess for an error message. Watch the
        // child instead. It does NOT end the wait outright: some launchers exit after handing off to
        // another process, which is exactly the shape that would still open the port. So the exit
        // shortens the wait to a grace period rather than failing on the spot, and names the exit
        // code if the port never appears.
        if child_exit.is_none()
            && let Ok(Some(status)) = child.try_wait() {
                child_exit = Some(match status.code() {
                    Some(c) => format!(" (it exited with code {c})"),
                    None => " (it exited)".to_string(),
                });
                let grace = Instant::now() + Duration::from_secs(2);
                if grace < deadline {
                    deadline = grace;
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
        on_wait();
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The first line of `DevToolsActivePort` is the port - a `0` there means "not chosen yet".
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

    /// A long-lived stand-in for the launched browser. `ping` is on every Windows box and needs no
    /// shell, so the process we spawn IS the child we hold - a `cmd /c ...` wrapper would leave the
    /// real worker running when the wrapper is killed, which is exactly the confusion these two
    /// tests must not have.
    fn spawn_placeholder_child() -> Child {
        std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("ping should spawn on Windows")
    }

    fn process_is_running(pid: u32) -> bool {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("tasklist should run");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
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
        let pid = child.id();
        assert!(process_is_running(pid), "the placeholder child never started");

        drop(LaunchedChromium { child, job: None, port: 1, user_data_dir: dir.clone(), cleaned: false });

        assert!(!process_is_running(pid), "the drop left the launched process running");
        assert!(!dir.exists(), "the drop left the profile directory behind");
        drop(guard);
    }

    /// The CDP path ignored `cwd` entirely: the wire carried it, `chrono-mech` honoured it, and a
    /// folder set in the panel or with `--cwd` did nothing for exactly the targets this mechanism
    /// owns. Silently - which is the part that makes it worth a test rather than a comment.
    #[test]
    fn the_launch_command_starts_the_browser_in_the_requested_folder() {
        let profile = PathBuf::from("C:/temp/profile");
        let with = chromium_command("app.exe", &profile, &[], Some("C:/work"));
        assert_eq!(with.get_current_dir(), Some(Path::new("C:/work")));

        // Absent means "wherever we are", not an empty directory.
        let without = chromium_command("app.exe", &profile, &[], None);
        assert_eq!(without.get_current_dir(), None);
    }

    /// The working directory must not disturb the isolated profile: the profile is ours, lives in our
    /// own temp directory, and a target that starts elsewhere still gets that same profile.
    #[test]
    fn a_working_folder_does_not_move_the_isolated_profile() {
        let profile = PathBuf::from("C:/temp/profile");
        let cmd = chromium_command("app.exe", &profile, &[], Some("C:/work"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            args.iter().any(|a| a == "--user-data-dir=C:/temp/profile"),
            "the isolated profile flag went missing: {args:?}"
        );
        assert!(args.iter().any(|a| a == "--remote-debugging-port=0"));
    }

    /// The case no destructor can reach: the core dies without running any code of its own.
    ///
    /// Dropping the job handle without touching the child stands in for exactly that - when a
    /// process is terminated, the kernel closes its handles whatever the process was doing, and
    /// kill-on-close is what turns that into the browser going away. Before this existed, killing
    /// the core mid-session left the launched browser running with its debug port open (measured on
    /// Pomotroid) - the placeholder child here plays that browser.
    #[test]
    fn closing_the_job_ends_the_browser_with_no_destructor_involved() {
        let mut child = spawn_placeholder_child();
        let pid = child.id();
        let job = tie_lifetime_to_ours(&child).expect("a job object should be available");
        assert!(process_is_running(pid), "the placeholder child never started");

        drop(job);

        // Termination is the kernel's to schedule, so give it a moment rather than racing it.
        let mut gone = false;
        for _ in 0..50 {
            if !process_is_running(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(gone, "closing the job left the process running - kill-on-close is not in effect");
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
            job: None,
            port: 1,
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
                Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "for {arg}"),
                Ok(_) => panic!("a colliding flag must be refused: {arg}"),
            }
        }
        // An ordinary argument still passes the check (this one then fails to launch, which is a
        // different error entirely - the point is that it got that far).
        match launch_chromium("no-such-app.exe", &["--enable-logging".to_string()], None, || {}) {
            Err(e) => assert_ne!(e.kind(), io::ErrorKind::InvalidInput),
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

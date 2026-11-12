//! A C runtime reads the zone once, on the first call that needs it, and keeps it (UCRT `__tzset`, Windows
//! SDK 10.0.28000 `ucrt/time/tzset.cpp`). The libraries a program imports start before the hook (ADR-25),
//! so a library that asks for the time while it starts makes the program's runtime keep this machine's
//! zone, with its daylight saving rules, which `localtime` then applies to the session's date. Measured
//! 2026-11-12 on a spreadsheet application whose core library reads the time while it starts: `NOW()`
//! under a session at +05:00 in July showed the machine's summer offset, +02:00. The hook has each shared C
//! runtime of the program read the zone again once it is in, so the zone it keeps is the session's.
//!
//! The probe is this binary. Its TLS callback, which runs while the process starts and so before the hook,
//! loads the two shared C runtimes every Windows has and reads the local time from each, the way such a
//! library does. The probe then writes the local time each of them gives.
//!
//! A runtime keeps the session's zone just the same once it has read it, so a program the session lets go
//! while it runs (ADR-14) would show the real time in the session's zone. The hook has the runtimes read
//! the zone again when it lets the program go, and the probe, kept running past the end of a session, checks
//! that each gives the local time Windows gives again.

use std::ffi::c_void;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{s, HSTRING, PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Environment::GetEnvironmentVariableW;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetLocalTime;

/// The session zone, a half-hour offset, so this machine's zone cannot give the same local time by
/// coincidence on any runner this test is expected to meet.
const SESSION_ZONE: &str = "+05:30";

/// The moment the session starts at, in its zone.
const SESSION_AT: &str = "2091-06-15T12:00:00";

/// How the local time of the first ten minutes of the session begins, as the probe writes it.
const SESSION_WALL: &str = "2091-06-15T12:0";

/// The shared C runtimes the probe reads through: the Universal CRT every current program built with
/// Visual C++ uses, and the system's own, which MinGW programs use.
const SHARED_C_RUNTIMES: [&str; 2] = ["ucrtbase.dll", "msvcrt.dll"];

/// Set for the probe, naming the file it writes. Unset, the probe returns at once.
const PROBE_FILE: &str = "CHRONO_CRT_ZONE_PROBE_FILE";

/// Set for the probe, so its TLS callback reads the local time from each runtime while the process starts.
const EARLY_READ: &str = "CHRONO_CRT_ZONE_EARLY_READ";

/// Set for the probe, so it stays past the end of the session and writes how far each runtime's local time
/// is then from the one Windows gives.
const OUTLIVE: &str = "CHRONO_CRT_ZONE_OUTLIVE";

/// How long the probe waits for the session to let it go, and then for the runtimes to read the zone again.
const LET_GO_LIMIT: Duration = Duration::from_secs(30);
const READ_AGAIN_LIMIT: Duration = Duration::from_secs(5);

/// How far apart, in seconds, two readings of the local time taken one after the other may be.
const SAME_LOCAL_TIME_S: i64 = 120;

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_writes_the_local_time_of_each_shared_c_runtime";

/// What the loader passes a TLS callback when the process attaches (MS Learn, the PE format, TLS callback
/// functions).
const DLL_PROCESS_ATTACH: u32 = 1;

/// Whether the TLS callback had the runtimes read the zone, which the probe writes down.
static READ_EARLY: AtomicBool = AtomicBool::new(false);

/// `struct tm` of the C runtime.
#[repr(C)]
#[derive(Default)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
}

/// One of the shared C runtimes, loaded if it is not yet.
fn c_runtime(name: &str) -> Option<HMODULE> {
    // SAFETY: loads a system library by name, which runs nothing of the caller's.
    unsafe { LoadLibraryW(PCWSTR(HSTRING::from(name).as_ptr())) }.ok()
}

/// One exported function of a C runtime, or `None` when it does not export it.
fn c_runtime_function(module: HMODULE, name: PCSTR) -> Option<unsafe extern "system" fn() -> isize> {
    // SAFETY: looks an export up by name in a loaded module.
    unsafe { GetProcAddress(module, name) }
}

/// The program's own TLS callback, run by the loader in every process of this binary while it starts. It
/// reads the local time, as a library that asks for the time while it starts does: the runtime's time
/// functions read the zone on their first call and keep it (`__tzset`). Calling `_tzset` here would not
/// do, because it reads the zone without marking it read.
unsafe extern "system" fn on_tls_event(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    // SAFETY: asks whether a variable is set, with no buffer.
    let early = unsafe { GetEnvironmentVariableW(&HSTRING::from(EARLY_READ), None) } != 0;
    if reason != DLL_PROCESS_ATTACH || !early {
        return;
    }
    let read = SHARED_C_RUNTIMES.iter().all(|name| local_tm(name).is_some());
    READ_EARLY.store(read, Ordering::SeqCst);
}

/// The callback, where the C runtime's TLS directory collects the program's callbacks (`.CRT$XLA` to
/// `.CRT$XLZ`, the standard library's own sits at `.CRT$XLB`).
#[used]
#[unsafe(link_section = ".CRT$XLM")]
static TLS_EVENT: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls_event;

/// The local time one shared C runtime gives now, or `None` when it cannot.
fn local_tm(name: &str) -> Option<Tm> {
    let module = c_runtime(name)?;
    let time = c_runtime_function(module, s!("_time64"))?;
    let localtime = c_runtime_function(module, s!("_localtime64_s"))?;
    // SAFETY: both in the C calling convention, `_time64(__time64_t*)` and
    // `_localtime64_s(struct tm*, const __time64_t*)`, given valid pointers.
    let (status, tm) = unsafe {
        let time = std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "C" fn(*mut i64) -> i64>(time);
        let localtime = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "C" fn(*mut Tm, *const i64) -> i32,
        >(localtime);
        let now = time(std::ptr::null_mut());
        let mut tm = Tm::default();
        (localtime(&mut tm, &now), tm)
    };
    (status == 0).then_some(tm)
}

/// The local time one shared C runtime gives now, `yyyy-mm-ddThh:mm:ss`, or `None` when it cannot.
fn local_time(name: &str) -> Option<String> {
    let tm = local_tm(name)?;
    Some(format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec))
}

/// Seconds since 1970 of a date and time read as if it were UTC, which makes two local times comparable.
fn civil_seconds(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    // Days from 1970-01-01 of a proleptic Gregorian date (H. Hinnant, `days_from_civil`).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + hour * 3_600 + minute * 60 + second
}

/// The local time Windows gives now, in `civil_seconds`.
fn windows_local_seconds() -> i64 {
    // SAFETY: fills a structure the call returns by value.
    let t = unsafe { GetLocalTime() };
    civil_seconds(t.wYear.into(), t.wMonth.into(), t.wDay.into(), t.wHour.into(), t.wMinute.into(), t.wSecond.into())
}

/// How far, in seconds, the local time of each shared runtime is from the one Windows gives.
fn distances_from_windows() -> Vec<i64> {
    SHARED_C_RUNTIMES
        .iter()
        .map(|name| {
            local_tm(name).map_or(i64::MAX, |tm| {
                let crt = civil_seconds(
                    i64::from(tm.year) + 1900,
                    i64::from(tm.mon) + 1,
                    tm.mday.into(),
                    tm.hour.into(),
                    tm.min.into(),
                    tm.sec.into(),
                );
                (crt - windows_local_seconds()).abs()
            })
        })
        .collect()
}

/// Stays until the session has let the probe go, which is when Windows gives a local time outside the
/// session's year again, then until each runtime gives the local time Windows gives, and writes how far
/// each one is from it at the end.
fn outlive_the_session() -> String {
    let start = Instant::now();
    // SAFETY: fills a structure the call returns by value.
    while unsafe { GetLocalTime() }.wYear == 2091 && start.elapsed() < LET_GO_LIMIT {
        std::thread::sleep(Duration::from_millis(100));
    }
    let let_go = Instant::now();
    let mut distances = distances_from_windows();
    while distances.iter().any(|&d| d > SAME_LOCAL_TIME_S) && let_go.elapsed() < READ_AGAIN_LIMIT {
        std::thread::sleep(Duration::from_millis(50));
        distances = distances_from_windows();
    }
    SHARED_C_RUNTIMES.iter().zip(distances).map(|(name, d)| format!("|after_{name}={d}")).collect()
}

/// Writes whether its runtimes read the zone while it started, and the local time each gives, then, when
/// asked to outlive the session, how far each one is from Windows once the session let it go.
#[test]
#[ignore = "the target of the session tests below, not a test on its own"]
fn probe_writes_the_local_time_of_each_shared_c_runtime() {
    let Some(file) = std::env::var_os(PROBE_FILE) else {
        return;
    };
    let mut line = format!("early={}", READ_EARLY.load(Ordering::SeqCst));
    for name in SHARED_C_RUNTIMES {
        line.push_str(&format!("|{name}={}", local_time(name).unwrap_or_else(|| "none".to_string())));
    }
    if std::env::var_os(OUTLIVE).is_some() {
        line.push_str(&outlive_the_session());
    }
    std::fs::write(file, line).expect("the probe's file");
}

/// Held by every test here that starts the core. The core allows one session at a time, and the tests of
/// one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One `name=value` field of the probe's line, or `None` when the line does not carry it.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split('|').find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// The arguments that run the probe and nothing else in this binary.
fn probe_args() -> [String; 5] {
    ["--ignored".into(), "--exact".into(), PROBE.into(), "--test-threads".into(), "1".into()]
}

/// What the probe wrote, run with `run`, from a scratch file removed afterwards, its stdout, and both in one
/// text for the messages of the assertions.
fn probe_line(name: &str, run: impl FnOnce(&std::path::Path) -> std::process::Output) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("chrono-crt-zone-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let file = dir.join("local.txt");
    let out = run(&file);
    let line = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let context = format!("The probe wrote {line:?}. stdout: {stdout} stderr: {}", String::from_utf8_lossy(&out.stderr));
    (line, stdout, context)
}

/// How the report counts the zone channel the C runtimes read through, for a program that never asked it
/// once the hook was in: the probe's runtimes had read the zone already, so only the hook's own reading of
/// it again could make it more.
const NO_ZONE_QUERY: &str = "GetTimeZoneInformation (0 calls)";

/// A shared C runtime that read the zone while the program started, before the hook, gives the session's
/// local time under a session all the same. Without a session the same probe reads this machine's, which
/// is what says it reads the clock at all. The hook's own reading of the zone again is not the program's,
/// so the report does not count it.
#[test]
fn a_c_runtime_that_read_the_zone_before_the_hook_gives_the_session_local_time() {
    let library = injected_library();
    assert!(library.is_file(), "this test drives a real session and needs {}", library.display());
    let me = std::env::current_exe().expect("the test binary knows its own path");

    let (control, _, context) = probe_line("control", |file| {
        Command::new(&me).args(probe_args()).env(PROBE_FILE, file).env(EARLY_READ, "1").output().expect("the probe must run")
    });
    assert_eq!(field(&control, "early"), Some("true"), "the probe's runtimes did not read the zone while it started. {context}");
    for name in SHARED_C_RUNTIMES {
        let local = field(&control, name).unwrap_or_default();
        assert!(
            local.len() == SESSION_WALL.len() + 4 && !local.starts_with("2091"),
            "{name} gives no local time of this machine without a session. {context}"
        );
    }

    let _session = one_real_session_at_a_time();
    let (seen, report, context) = probe_line("session", |file| {
        Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(["run", &me.display().to_string(), "--at", SESSION_AT, "--zone", SESSION_ZONE, "--args", &probe_args().join(" ")])
            .env(PROBE_FILE, file)
            .env(EARLY_READ, "1")
            .output()
            .expect("the tool must run")
    });
    assert_eq!(field(&seen, "early"), Some("true"), "the probe never ran under the session. {context}");
    for name in SHARED_C_RUNTIMES {
        assert!(
            field(&seen, name).is_some_and(|local| local.starts_with(SESSION_WALL)),
            "{name} kept the zone it read before the hook and gives this machine's local time. {context}"
        );
    }
    assert!(report.contains(NO_ZONE_QUERY), "the hook's own reading of the zone again is counted as the program's. {context}");
}

/// A program the session lets go while it runs (ADR-14) reads the real clock again, and its shared C
/// runtimes read this machine's zone again, so the local time each gives is the one Windows gives, not the
/// real time in the session's zone. The session ends after two heartbeats, and the probe stays.
#[test]
fn a_c_runtime_of_a_program_the_session_lets_go_reads_this_machines_zone_again() {
    let library = injected_library();
    assert!(library.is_file(), "this test drives a real session and needs {}", library.display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let _session = one_real_session_at_a_time();
    let (seen, _, context) = probe_line("outlive", |file| {
        Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args([
                "run",
                &me.display().to_string(),
                "--at",
                SESSION_AT,
                "--zone",
                SESSION_ZONE,
                "--ticks",
                "2",
                "--args",
                &probe_args().join(" "),
            ])
            .env(PROBE_FILE, file)
            .env(EARLY_READ, "1")
            .env(OUTLIVE, "1")
            .output()
            .expect("the tool must run")
    });
    for name in SHARED_C_RUNTIMES {
        assert!(
            field(&seen, name).is_some_and(|local| local.starts_with(SESSION_WALL)),
            "{name} did not give the session's local time while the session ran. {context}"
        );
        let after = field(&seen, &format!("after_{name}")).and_then(|d| d.parse::<i64>().ok());
        assert!(
            after.is_some_and(|d| d <= SAME_LOCAL_TIME_S),
            "{name} kept the session's zone after the session let the program go: {after:?} s from the local time \
             Windows gives. {context}"
        );
    }
}

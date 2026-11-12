//! A session's zone has to say it has no daylight saving time, or the JVM never sees it.
//!
//! The session zone is a fixed offset. The hook hands it out through `GetDynamicTimeZoneInformation`
//! under the key name "Chrono Session", which no registry knows. What a runtime does with a name it
//! cannot look up depends on one field of that answer, `DynamicDaylightTimeDisabled`. When it is set,
//! the JVM (every line from 8 to the current one, `TimeZone_md.c`) builds its zone from the `Bias` the
//! hook returned. When it is clear, the JVM looks the name up in its own mapping table, misses, and
//! reads `ActiveTimeBias` from the REAL registry instead. Until 2026-09-23 it was clear, so every Java
//! application under a session showed the host's zone while the audit reported the session as working.
//!
//! The JVM itself is measured by the harness, which CI does not have. What CI can check on a clean
//! runner is the field, read the way the JVM reads it: Windows PowerShell is part of every Windows
//! installation and can call the function through kernel32 with nothing outside the repository.
//!
//! The same probe asks `GetTimeZoneInformationForYear` too, which the Rust `chrono` crate reads the
//! current zone through and nothing else (2026-10-27), and writes down the `TZ` it was started with: a
//! `TZ` of the tester's own must not reach the program, or the runtimes that read it show its zone
//! instead of the session's (2026-11-12).
//!
//! The key name is read for one more reader, ICU (ADR-26). ICU names a whole-hour zone from the bias
//! and any other from the key name, through CLDR's table of Windows zones, where "Chrono Session" is not.
//! So a session at +05:30 hands out the key of India's zone, which ICU maps, from the date that zone has
//! kept +05:30 on, and its own name before it, with a warning in the report.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The session zone, a half-hour offset, so the bias it produces cannot be the host's by coincidence
/// on any runner this test is expected to meet.
const SESSION_ZONE: &str = "+05:30";

/// The bias that zone has to arrive as, in the Win32 sense (UTC = local + bias).
const SESSION_BIAS: &str = "-330";

/// The session zone's own name, which no registry knows: the standard name of every answer the hook
/// gives, so seeing it proves the session reached the probe, and the key name where no real zone lends
/// one.
const SESSION_NAME: &str = "Chrono Session";

/// The key name the session zone borrows at `SESSION_ZONE` from 1945-10-14T17:30Z on, when the IANA zone
/// CLDR maps it to has kept +05:30 without daylight saving time (`chrono_ctl::BORROWED_NAMES`).
const BORROWED_KEY: &str = "India Standard Time";

/// A moment before that date, at which the session zone keeps its own name, and the report says ICU
/// cannot name it. The shared session jumps here, and the session of its own starts here.
const BEFORE_THE_BORROWED_PERIOD: &str = "1944-06-15T12:00:00";

/// How the report names a session zone ICU cannot name.
const ZONE_UNNAMED: &str = "(session.zone_unnamed)";

/// How the report names a jump to before the period of the name the session zone borrowed.
const JUMP_BEFORE_ZONE_NAME: &str = "(session.jump_before_zone_name)";

/// A `TZ` of the tester's own, in the POSIX form the C runtime reads (UTC+2), set for the probe without a
/// session and for the session that runs it. The session must not hand it on.
const TESTER_TZ: &str = "EET-2";

/// How the report names a `TZ` of the tester's own that the session did not hand on.
const TZ_LEFT_OUT: &str = "(session.tz_variable_removed)";

/// The probe: one call to `GetDynamicTimeZoneInformation` through kernel32, written to a file,
/// because the output of a target running under a session does not reach the caller's pipe. Given a year,
/// it stays until its clock shows that year, 30 seconds at most, so a session that jumps there after its
/// first heartbeat lasts until the jump has landed, however late that heartbeat reaches the driver.
const PROBE: &str = r#"Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ZoneProbe {
    [StructLayout(LayoutKind.Sequential)]
    public struct SystemTime { public ushort Year, Month, DayOfWeek, Day, Hour, Minute, Second, Milliseconds; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DynamicZone {
        public int Bias;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string StandardName;
        public SystemTime StandardDate;
        public int StandardBias;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DaylightName;
        public SystemTime DaylightDate;
        public int DaylightBias;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string TimeZoneKeyName;
        [MarshalAs(UnmanagedType.U1)] public bool DynamicDaylightTimeDisabled;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct Zone {
        public int Bias;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string StandardName;
        public SystemTime StandardDate;
        public int StandardBias;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DaylightName;
        public SystemTime DaylightDate;
        public int DaylightBias;
    }
    [DllImport("kernel32.dll")]
    public static extern uint GetDynamicTimeZoneInformation(out DynamicZone zone);
    [DllImport("kernel32.dll")]
    public static extern bool GetTimeZoneInformationForYear(ushort year, IntPtr dynamicZone, out Zone zone);
    [DllImport("kernel32.dll", EntryPoint = "GetTimeZoneInformationForYear")]
    public static extern bool GetNamedZoneForYear(ushort year, ref DynamicZone dynamicZone, out Zone zone);
}
'@
$z = New-Object ZoneProbe+DynamicZone
$null = [ZoneProbe]::GetDynamicTimeZoneInformation([ref]$z)
$y = New-Object ZoneProbe+Zone
$current = [ZoneProbe]::GetTimeZoneInformationForYear(2038, [IntPtr]::Zero, [ref]$y)
$n = New-Object ZoneProbe+Zone
$named = [ZoneProbe]::GetNamedZoneForYear(2038, [ref]$z, [ref]$n)
$k = New-Object ZoneProbe+DynamicZone
$k.TimeZoneKeyName = $z.TimeZoneKeyName
$o = New-Object ZoneProbe+Zone
$keyonly = [ZoneProbe]::GetNamedZoneForYear(2038, [ref]$k, [ref]$o)
$tz = [Environment]::GetEnvironmentVariable('TZ')
if ($null -eq $tz) { $tz = 'unset' }
Set-Content -Path zone.txt -Value ("key={0}|bias={1}|dstoff={2}|year={3}:{4}:{5}|named={6}:{7}:{8}|keyonly={9}:{10}:{11}|tz={12}" -f $z.TimeZoneKeyName, $z.Bias, $z.DynamicDaylightTimeDisabled, $current, $y.Bias, $y.StandardName, $named, $n.Bias, $n.StandardName, $keyonly, $o.Bias, $o.StandardName, $tz)
if ($args.Count -gt 0) {
    $waited = [Diagnostics.Stopwatch]::StartNew()
    while ([DateTime]::UtcNow.Year -ne [int]$args[0] -and $waited.Elapsed.TotalSeconds -lt 30) { Start-Sleep -Milliseconds 100 }
}
"#;

/// Windows PowerShell, by full path, for the same reason `dry_run.rs` gives for the interpreter.
fn windows_powershell() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// The arguments that run the probe from the current directory.
const PROBE_ARGS: &str = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File zone.ps1";

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// A scratch directory for one run of this test holding the probe, removed afterwards.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-session-zone-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::write(dir.join("zone.ps1"), PROBE).expect("the probe script");
    dir
}

/// The line the probe wrote in `dir`, or an empty string when it wrote nothing.
fn written_zone(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("zone.txt")).unwrap_or_default().trim().to_string()
}

/// One `name=value` field of the probe's line, or `None` when the line does not carry it.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split('|').find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
}

/// What the probe wrote without a session and under one, the session's report, and all of it in one text
/// for the messages of the assertions.
struct ZoneLines {
    real: String,
    seen: String,
    report: String,
    context: String,
}

/// One session at `SESSION_ZONE` from `at` running the probe, started with `TESTER_TZ`, and jumping to
/// `jump_to` after its first heartbeat when one is given (the probe then stays until its clock shows the
/// year of the jump): the probe's line, the report on stdout, and stderr.
fn probe_session(name: &str, at: &str, jump_to: Option<&str>) -> (String, String, String) {
    let dir = scratch(name);
    let probe_args = match jump_to {
        Some(to) => format!("{PROBE_ARGS} {}", to.split_once('-').map_or(to, |(year, _)| year)),
        None => PROBE_ARGS.to_string(),
    };
    let mut args = vec!["run".to_string(), windows_powershell(), "--args".into(), probe_args];
    args.extend(["--cwd".to_string(), dir.display().to_string(), "--at".into(), at.into(), "--zone".into(), SESSION_ZONE.into()]);
    if let Some(to) = jump_to {
        args.extend(["--jump-after".to_string(), format!("1:{to}")]);
    }
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(&args)
        .env("TZ", TESTER_TZ)
        .output()
        .expect("the tool must run");
    let seen = written_zone(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    (seen, String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Whether the probe's line came from under a session: the zone of a year carries the session's own name,
/// which no machine's zone has.
fn reached_by_the_session(seen: &str) -> bool {
    field(seen, "year").is_some_and(|year| year.ends_with(SESSION_NAME))
}

/// The probe's line without a session and under one at `SESSION_ZONE` that jumps to before the period of
/// the name it borrows, both started with `TESTER_TZ`, after checking the control: the probe ran, read the
/// zone, and this machine's zone is not the one whose name the session borrows.
fn zone_lines() -> ZoneLines {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );

    // The control first: without a session the probe has to write a well-formed line that does NOT
    // carry the session key. Without it a probe that never ran, or one that always wrote the key,
    // would let the assertions below pass or fail for reasons unrelated to the hook.
    let control = scratch("control");
    let status = Command::new(windows_powershell())
        .args(PROBE_ARGS.split(' '))
        .current_dir(&control)
        .env("TZ", TESTER_TZ)
        .status()
        .expect("Windows PowerShell must run");
    assert!(status.success(), "the probe could not run without a session");
    let real = written_zone(&control);
    assert!(
        field(&real, "key").is_some() && field(&real, "bias").is_some() && field(&real, "dstoff").is_some(),
        "the probe wrote {real:?} without a session, so it cannot read the zone at all"
    );
    assert!(!reached_by_the_session(&real), "the host already reports the session's name: {real}");
    assert!(
        field(&real, "keyonly").is_some_and(|answer| answer.starts_with("True:")),
        "Windows does not answer for a zone named by its key alone, so the probe cannot tell whose answer it got: {real}"
    );
    assert_ne!(
        field(&real, "key"),
        Some(BORROWED_KEY),
        "this machine is in the zone whose name the session borrows, so the key cannot tell them apart: {real}"
    );
    let _ = std::fs::remove_dir_all(&control);

    let (seen, report, stderr) = probe_session("session", "2091-06-15T12:00:00", Some(BEFORE_THE_BORROWED_PERIOD));
    let context = format!("The probe wrote {seen:?} under the session and {real:?} without it. stdout: {report} stderr: {stderr}");
    assert!(reached_by_the_session(&seen), "the session never reached the probe. {context}");
    ZoneLines { real, seen, report, context }
}

/// Held by every test here that starts the core. The core allows one session at a time, and the
/// tests of one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The probe's session, run once by whichever test asks first: every question about the zone the
/// PowerShell probe can answer is in the same line.
fn lines() -> &'static ZoneLines {
    static LINES: OnceLock<ZoneLines> = OnceLock::new();
    LINES.get_or_init(|| {
        let _session = one_real_session_at_a_time();
        zone_lines()
    })
}

/// The source path the Rust `jiff` crate leaves in a build that links it, which is how the core tells
/// such a program (`pe::links_jiff`). This test binary carries it, read by the probe below so it stays in
/// the build, and so the core takes the binary for a program built on jiff.
static JIFF_SOURCE_PATH: &str = r"C:\cargo\registry\src\index\jiff-0.2.37\src\tz\system\windows\mod.rs";

/// Set for the probe, naming the file it writes. Unset, the probe returns at once.
const PROBE_FILE: &str = "CHRONO_SESSION_ZONE_PROBE_FILE";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const ZONE_VARIABLE_PROBE: &str = "probe_writes_the_zone_variable_it_was_given";

/// Writes down the `TZ` it was started with, and how long the jiff path it carries is.
#[test]
#[ignore = "the target of the session test below, not a test on its own"]
fn probe_writes_the_zone_variable_it_was_given() {
    let Some(file) = std::env::var_os(PROBE_FILE) else {
        return;
    };
    let carried = std::hint::black_box(JIFF_SOURCE_PATH).len();
    let tz = std::env::var("TZ").unwrap_or_else(|_| "unset".to_string());
    std::fs::write(file, format!("tz={tz}|carried={carried}")).expect("the probe's file");
}

/// A program built on the Rust `jiff` crate finds its zone by a name the session zone does not have, and
/// reads `TZ` before anything else, so the core gives it the session zone there: `CMS-5:30` for +05:30
/// (2026-10-27). No other program gets a `TZ` at all, because Node reads `TZ` too and breaks on that
/// value, and a `TZ` of the tester's own would replace the session zone in the runtimes that read it: the
/// PowerShell probe, started with one, sees it without a session and none under the session.
#[test]
fn a_program_built_on_jiff_is_given_the_session_zone_in_tz_and_no_other_program_is() {
    let ZoneLines { real, seen, context, .. } = lines();
    assert_eq!(field(real, "tz"), Some(TESTER_TZ), "the probe does not see the TZ it was started with. {context}");
    assert_eq!(field(seen, "tz"), Some("unset"), "a TZ of the tester's own reached a program not built on jiff. {context}");

    let library = injected_library();
    assert!(library.is_file(), "this test drives a real session and needs {}", library.display());
    let dir = scratch("jiff");
    let file = dir.join("tz.txt");
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let out = {
        let _session = one_real_session_at_a_time();
        Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args([
                "run",
                &me.display().to_string(),
                "--at",
                "2091-06-15T12:00:00",
                "--zone",
                SESSION_ZONE,
                "--args",
                &format!("--ignored --exact {ZONE_VARIABLE_PROBE} --test-threads 1"),
            ])
            .env(PROBE_FILE, &file)
            .env_remove("TZ")
            .output()
            .expect("the tool must run")
    };
    let written = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let report = String::from_utf8_lossy(&out.stdout);
    let context = format!("The probe wrote {written:?}. stdout: {report} stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(field(&written, "carried").is_some(), "the probe never ran under the session. {context}");
    assert_eq!(field(&written, "tz"), Some("CMS-5:30"), "{context}");
    assert!(!report.contains(TZ_LEFT_OUT), "a tester without TZ lost nothing, and the report says otherwise. {context}");
}

/// A `TZ` of the tester's own that the session did not hand on is said in the report, so the tester who
/// set it to test a zone knows why the application shows the session's instead.
#[test]
fn a_tz_of_the_testers_own_left_out_is_said_in_the_report() {
    let ZoneLines { report, context, .. } = lines();
    assert!(report.contains(TZ_LEFT_OUT), "the report does not say the TZ was left out. {context}");
}

/// The zone the hook hands out through `GetDynamicTimeZoneInformation` carries the session's bias, and
/// says daylight saving time is disabled, which is what makes the JVM build its zone from that bias
/// instead of from the real registry, whatever key name it carries.
#[test]
fn the_dynamic_zone_of_a_session_has_daylight_saving_time_disabled() {
    let ZoneLines { seen, context, .. } = lines();
    assert_eq!(
        field(seen, "bias"),
        Some(SESSION_BIAS),
        "the session zone {SESSION_ZONE} arrived with the wrong bias. {context}"
    );
    assert_eq!(
        field(seen, "dstoff"),
        Some("True"),
        "the session zone does not say daylight saving time is disabled, so the JVM looks its key up, \
         misses, and takes the host's zone from the real registry. {context}"
    );
}

/// The zone of a year is the session's: asked about the current zone, which is how the Rust `chrono`
/// crate reads it and the only way (measured 2026-10-27: under a session at +05:30 its local time showed
/// this machine's zone), and asked about the zone `GetDynamicTimeZoneInformation` handed out, by the key
/// name it carries. That name is a real zone's here, which Windows would answer for with that zone's own
/// standard name: the session's name in the answer is what says the hook knew the zone for its own.
/// Without the session the same two questions get this machine's answers.
#[test]
fn the_zone_of_a_year_is_the_session_zone_whether_asked_by_none_or_by_its_name() {
    let ZoneLines { real, seen, context, .. } = lines();
    assert!(
        field(real, "year").is_some_and(|y| y.starts_with("True:")),
        "without a session the zone of a year has to be this machine's. {context}"
    );
    assert_eq!(field(real, "named").map(|n| n.starts_with("True:")), Some(true), "{context}");
    assert_eq!(
        field(seen, "year"),
        Some(format!("True:{SESSION_BIAS}:{SESSION_NAME}").as_str()),
        "the current zone of a year is not the session's, so a program built on `chrono` shows this \
         machine's zone. {context}"
    );
    assert_eq!(
        field(seen, "named"),
        Some(format!("True:{SESSION_BIAS}:{SESSION_NAME}").as_str()),
        "the zone handed out under the key name it carries is not answered for a year as the session's. {context}"
    );
}

/// At +05:30 from 1945-10-14 on, the zone borrows the key name of India's zone, which CLDR maps to an IANA
/// zone, so the runtimes built on ICU name it instead of showing it without a name or in UTC (measured
/// 2026-11-12). Its standard name stays the session's own, and the report says nothing ICU could not do.
#[test]
fn the_session_zone_borrows_the_key_name_of_a_real_zone_at_its_offset() {
    let ZoneLines { seen, report, context, .. } = lines();
    assert_eq!(field(seen, "key"), Some(BORROWED_KEY), "ICU finds no zone under the key the session handed out. {context}");
    assert!(!report.contains(ZONE_UNNAMED), "a zone with a borrowed name is reported as one ICU cannot name. {context}");
}

/// A program can name the real zone whose key the session borrowed by itself: by that key alone, or as
/// `EnumDynamicTimeZoneInformation` lists it, with that zone's own standard name. That is a question about
/// the real zone, which Windows answers with its rules of the year (Iran's held daylight saving time until
/// 2022), so the hook leaves it alone: the session's answer is for the zone the hook handed out, which
/// carries the session's own standard name too. Measured 2026-11-12: Windows answers India's key alone with
/// India's bias and its own standard name.
#[test]
fn the_real_zone_of_the_borrowed_key_is_answered_by_windows() {
    let ZoneLines { seen, context, .. } = lines();
    let answer = field(seen, "keyonly").unwrap_or_default();
    assert!(
        answer.starts_with(&format!("True:{SESSION_BIAS}:")) && !answer.ends_with(SESSION_NAME),
        "the real zone named by the borrowed key alone got the session's answer instead of Windows'. {context}"
    );
}

/// ICU reads the zone once, when a program first asks, so after a jump to before the date the borrowed
/// name's zone took the session's offset on, those programs read that zone's history and not the session's
/// offset. The report says so.
#[test]
fn a_jump_to_before_the_period_of_the_borrowed_name_is_said_in_the_report() {
    let ZoneLines { report, context, .. } = lines();
    assert!(report.contains(JUMP_BEFORE_ZONE_NAME), "the report does not say the jump left the borrowed name's period. {context}");
}

/// Before the date India's zone took +05:30, ICU would read that zone's history and show the offset of the
/// time, not the session's, so the zone keeps its own key name there, which ICU cannot map, and the report
/// says so. With no borrowed name there is no period to jump out of either. That own key name, which no
/// registry has, finds the session zone by itself, where Windows would answer it with a failure.
#[test]
fn a_session_before_the_period_of_the_borrowed_name_keeps_its_own_and_says_icu_cannot_name_it() {
    let library = injected_library();
    assert!(library.is_file(), "this test drives a real session and needs {}", library.display());
    let (seen, report, stderr) = {
        let _session = one_real_session_at_a_time();
        probe_session("before", BEFORE_THE_BORROWED_PERIOD, None)
    };
    let context = format!("The probe wrote {seen:?}. stdout: {report} stderr: {stderr}");
    assert!(reached_by_the_session(&seen), "the session never reached the probe. {context}");
    assert_eq!(field(&seen, "key"), Some(SESSION_NAME), "a name was borrowed before its zone had the offset. {context}");
    assert_eq!(
        field(&seen, "keyonly"),
        Some(format!("True:{SESSION_BIAS}:{SESSION_NAME}").as_str()),
        "the session's own key name alone did not find the session zone. {context}"
    );
    assert!(report.contains(ZONE_UNNAMED), "the report does not say ICU cannot name the zone. {context}");
    assert!(!report.contains(JUMP_BEFORE_ZONE_NAME), "a session that borrowed no name reports a jump out of its period. {context}");
}

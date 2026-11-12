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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The session zone, a half-hour offset, so the bias it produces cannot be the host's by coincidence
/// on any runner this test is expected to meet.
const SESSION_ZONE: &str = "+05:30";

/// The bias that zone has to arrive as, in the Win32 sense (UTC = local + bias).
const SESSION_BIAS: &str = "-330";

/// The key name only the hook hands out, so seeing it proves the session reached the probe.
const SESSION_KEY: &str = "Chrono Session";

/// A `TZ` of the tester's own, in the POSIX form the C runtime reads (UTC+2), set for the probe without a
/// session and for the session that runs it. The session must not hand it on.
const TESTER_TZ: &str = "EET-2";

/// How the report names a `TZ` of the tester's own that the session did not hand on.
const TZ_LEFT_OUT: &str = "(session.tz_variable_removed)";

/// The probe: one call to `GetDynamicTimeZoneInformation` through kernel32, written to a file,
/// because the output of a target running under a session does not reach the caller's pipe.
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
$tz = [Environment]::GetEnvironmentVariable('TZ')
if ($null -eq $tz) { $tz = 'unset' }
Set-Content -Path zone.txt -Value ("key={0}|bias={1}|dstoff={2}|year={3}:{4}:{5}|named={6}:{7}|tz={8}" -f $z.TimeZoneKeyName, $z.Bias, $z.DynamicDaylightTimeDisabled, $current, $y.Bias, $y.StandardName, $named, $n.Bias, $tz)
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

/// The probe's line without a session and under one at `SESSION_ZONE`, both started with `TESTER_TZ`,
/// after checking the control: the probe ran, read the zone, and the host does not already report the
/// session key.
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
    assert_ne!(field(&real, "key"), Some(SESSION_KEY), "the host already reports the session key: {real}");
    let _ = std::fs::remove_dir_all(&control);

    let dir = scratch("session");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &windows_powershell(),
            "--args",
            PROBE_ARGS,
            "--cwd",
            &dir.display().to_string(),
            "--at",
            "2091-06-15T12:00:00",
            "--zone",
            SESSION_ZONE,
        ])
        .env("TZ", TESTER_TZ)
        .output()
        .expect("the tool must run");
    let seen = written_zone(&dir);
    let report = String::from_utf8_lossy(&out.stdout).into_owned();
    let context = format!(
        "The probe wrote {seen:?} under the session and {real:?} without it. stdout: {report} stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(field(&seen, "key"), Some(SESSION_KEY), "the session never reached the probe. {context}");
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

/// The zone the hook hands out through `GetDynamicTimeZoneInformation` carries the session's key and
/// bias, and says daylight saving time is disabled, which is what makes the JVM build its zone from
/// that bias instead of from the real registry.
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
/// this machine's zone), and asked about the zone `GetDynamicTimeZoneInformation` handed out by its own
/// name, which no registry knows. Without the session the same two questions get this machine's answers.
#[test]
fn the_zone_of_a_year_is_the_session_zone_whether_asked_by_none_or_by_its_name() {
    let ZoneLines { real, seen, context, .. } = lines();
    assert!(
        field(real, "year").is_some_and(|y| y.starts_with("True:") && !y.ends_with(SESSION_KEY)),
        "without a session the zone of a year has to be this machine's. {context}"
    );
    assert_eq!(field(real, "named").map(|n| n.starts_with("True:")), Some(true), "{context}");
    assert_eq!(
        field(seen, "year"),
        Some(format!("True:{SESSION_BIAS}:{SESSION_KEY}").as_str()),
        "the current zone of a year is not the session's, so a program built on `chrono` shows this \
         machine's zone. {context}"
    );
    assert_eq!(
        field(seen, "named"),
        Some(format!("True:{SESSION_BIAS}").as_str()),
        "the zone handed out under the session's own name is not answered for a year. {context}"
    );
}

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

use std::path::{Path, PathBuf};
use std::process::Command;

/// The session zone, a half-hour offset, so the bias it produces cannot be the host's by coincidence
/// on any runner this test is expected to meet.
const SESSION_ZONE: &str = "+05:30";

/// The bias that zone has to arrive as, in the Win32 sense (UTC = local + bias).
const SESSION_BIAS: &str = "-330";

/// The key name only the hook hands out, so seeing it proves the session reached the probe.
const SESSION_KEY: &str = "Chrono Session";

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
    [DllImport("kernel32.dll")]
    public static extern uint GetDynamicTimeZoneInformation(out DynamicZone zone);
}
'@
$z = New-Object ZoneProbe+DynamicZone
$null = [ZoneProbe]::GetDynamicTimeZoneInformation([ref]$z)
Set-Content -Path zone.txt -Value ("key={0}|bias={1}|dstoff={2}" -f $z.TimeZoneKeyName, $z.Bias, $z.DynamicDaylightTimeDisabled)
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

/// The zone the hook hands out through `GetDynamicTimeZoneInformation` carries the session's key and
/// bias, and says daylight saving time is disabled, which is what makes the JVM build its zone from
/// that bias instead of from the real registry.
#[test]
fn the_dynamic_zone_of_a_session_has_daylight_saving_time_disabled() {
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
        .output()
        .expect("the tool must run");
    let seen = written_zone(&dir);
    let context = format!(
        "The probe wrote {seen:?} under the session and {real:?} without it. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(field(&seen, "key"), Some(SESSION_KEY), "the session never reached the probe. {context}");
    assert_eq!(
        field(&seen, "bias"),
        Some(SESSION_BIAS),
        "the session zone {SESSION_ZONE} arrived with the wrong bias. {context}"
    );
    assert_eq!(
        field(&seen, "dstoff"),
        Some("True"),
        "the session zone does not say daylight saving time is disabled, so the JVM looks its key up, \
         misses, and takes the host's zone from the real registry. {context}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

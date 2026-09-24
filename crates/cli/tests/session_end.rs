//! An application that outlives its session is let go without its duration axes stepping back.
//!
//! A session ends while the application keeps running more often than not: a `--ticks` cutoff, a Stop in
//! the panel, a core that died. The hook then lets the application go, and until 2026-09-24 it did that by
//! handing back the real value of every clock. For the wall and the zone that is the point. For the tick
//! count, the unbiased interrupt time and QPC under `--scale-duration` / `--scale-qpc` it was a step BACK
//! by the whole acceleration - measured at x60 after 5.4 s, 316 s in one step, on x64 and x86 alike -
//! on the axis untouchable rule 3 says never rewinds.
//!
//! The target is this test binary itself, as in `duration_axis.rs`: the ignored probe below does its
//! work only when the variable names a file. It samples the three axes for longer than the session lasts
//! and writes the largest step back it saw on each, plus how fast the tick count moved at the start and
//! at the end, against a reference the hook never touches.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Where the probe writes what it measured. Unset, the probe returns at once.
const PROBE_OUT: &str = "CHRONO_SESSION_END_PROBE_OUT";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_samples_the_duration_axes_past_the_session_end";

/// How long the probe samples, in real milliseconds. The session below ends after two heartbeats, so the
/// probe sees about two seconds of the session and three of what comes after it.
const SAMPLE_MS: f64 = 5_000.0;

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn GetTickCount64() -> u64;
    fn QueryUnbiasedInterruptTime(time: *mut u64) -> i32;
}

#[cfg_attr(target_arch = "x86", link(name = "ntdll", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "ntdll", kind = "raw-dylib"))]
unsafe extern "system" {
    /// The reference clock. The hook never touches it (ADR-2), so it stays real under any session.
    fn NtQueryPerformanceCounter(counter: *mut i64, frequency: *mut i64) -> i32;
}

/// Real milliseconds by the unhooked reference clock.
fn real_ms() -> f64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    // SAFETY: both pointers are to live locals.
    unsafe { NtQueryPerformanceCounter(&mut counter, &mut frequency) };
    counter as f64 * 1000.0 / frequency as f64
}

/// One reading of the three axes, each in milliseconds: the tick count, the unbiased interrupt time and
/// QPC, which `Instant` reads.
fn axes(origin: Instant) -> [f64; 3] {
    let mut quit = 0u64;
    // SAFETY: the export's documented signature, no arguments and no state.
    let tick = unsafe { GetTickCount64() };
    // SAFETY: the export's documented signature, with a live out pointer.
    unsafe { QueryUnbiasedInterruptTime(&mut quit) };
    [tick as f64, quit as f64 / 10_000.0, origin.elapsed().as_secs_f64() * 1000.0]
}

/// Samples every few milliseconds for `SAMPLE_MS` of real time and writes, space separated: the rate of
/// the tick count, the interrupt time and QPC over the first real second, the tick rate over the last
/// real second, and the largest step back seen on each of the three axes (0 when it never went back).
///
/// A rate per axis at the start, not the tick count's alone: the step-back check on an axis the session
/// never scaled cannot fail, so without it a session that stopped scaling QUIT or QPC would pass here
/// with nothing measured on those two.
#[test]
#[ignore = "the target of `an_application_left_running_keeps_its_duration_axes_moving_forward`, not a test on its own"]
fn probe_samples_the_duration_axes_past_the_session_end() {
    let Some(out) = std::env::var_os(PROBE_OUT) else {
        return;
    };
    let origin = Instant::now();
    let start = real_ms();
    let mut last = axes(origin);
    let mut worst = [0.0f64; 3];
    let first = last;
    let mut head_rates = None;
    let mut tail_from = None;
    loop {
        // A sleep, not a spin: the session shortens it to its floor, which is still a millisecond, and
        // once the session is gone it is a plain millisecond again.
        std::thread::sleep(Duration::from_millis(5));
        let now = real_ms() - start;
        let reading = axes(origin);
        for (i, w) in worst.iter_mut().enumerate() {
            *w = w.min(reading[i] - last[i]);
        }
        last = reading;
        if head_rates.is_none() && now >= 1_000.0 {
            head_rates = Some([0, 1, 2].map(|i| (reading[i] - first[i]) / now));
        }
        if tail_from.is_none() && now >= SAMPLE_MS - 1_000.0 {
            tail_from = Some((now, reading[0]));
        }
        if now >= SAMPLE_MS {
            let (t0, tick0) = tail_from.unwrap_or((now, reading[0]));
            let tail_rate = (reading[0] - tick0) / (now - t0).max(1.0);
            let [head_tick, head_quit, head_qpc] = head_rates.unwrap_or([0.0; 3]);
            let line = format!(
                "{head_tick:.2} {head_quit:.2} {head_qpc:.2} {:.2} {:.1} {:.1} {:.1}",
                tail_rate,
                -worst[0],
                -worst[1],
                -worst[2]
            );
            std::fs::write(&out, line).expect("the probe writes its result");
            return;
        }
    }
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// What the probe wrote: `[head rate of tick, of QUIT, of QPC, tail rate, step back on tick, on QUIT,
/// on QPC]`.
fn measured(file: &std::path::Path) -> Option<[f64; 7]> {
    let text = std::fs::read_to_string(file).ok()?;
    let values: Vec<f64> = text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    values.try_into().ok()
}

/// An application still running when its session ends carries on at rate 1 from where its duration axes
/// stood, and the session says it left it running.
#[test]
fn an_application_left_running_keeps_its_duration_axes_moving_forward() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = std::env::temp_dir().join(format!("chrono-session-end-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let probe_args = ["--ignored", "--exact", PROBE, "--test-threads", "1"];

    // The control first: with no session every axis moves at about the real rate and never back.
    // Without it a probe that measured nothing would pass everything below.
    let control = dir.join("control.txt");
    let run = Command::new(&me)
        .args(probe_args)
        .env(PROBE_OUT, &control)
        .output()
        .expect("the probe must run without a session");
    assert!(run.status.success(), "the probe failed without a session: {}", String::from_utf8_lossy(&run.stdout));
    let [head_tick, head_quit, head_qpc, tail, ..] =
        measured(&control).expect("the probe wrote nothing without a session");
    assert!(
        head_tick < 2.0 && head_quit < 2.0 && head_qpc < 2.0 && tail < 2.0,
        "without a session the axes moved x{head_tick} (tick), x{head_quit} (interrupt time), x{head_qpc} \
         (QPC) then x{tail}, so this probe cannot tell"
    );

    // The session ends after two heartbeats. The probe inherits the core's output, so `output()`
    // returns once the probe is done as well - which is what the result file needs.
    let session = dir.join("session.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &me.display().to_string(),
            "--mode",
            "x60",
            "--scale-duration",
            "--scale-qpc",
            "--ticks",
            "2",
            "--json",
            "--args",
            &probe_args.join(" "),
        ])
        .env(PROBE_OUT, &session)
        .output()
        .expect("the tool must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let context = || format!("stdout: {stdout} stderr: {}", String::from_utf8_lossy(&out.stderr));
    let Some([head_tick, head_quit, head_qpc, tail, tick_back, quit_back, qpc_back]) = measured(&session) else {
        panic!("the probe wrote nothing under the session. {}", context());
    };
    // Every axis the step-back check reads has to have been scaled, or that check proves nothing on it.
    assert!(
        head_tick > 10.0 && head_quit > 10.0 && head_qpc > 10.0,
        "during the session the axes moved x{head_tick} (tick), x{head_quit} (interrupt time), x{head_qpc} \
         (QPC), so the session did not scale all three. {}",
        context()
    );
    assert!(tail < 2.0, "the tick count still moved x{tail} after the session, so the probe did not outlive it. {}", context());
    assert!(
        tick_back == 0.0 && quit_back == 0.0 && qpc_back == 0.0,
        "letting the application go stepped its duration axes back (tick {tick_back} ms, interrupt time \
         {quit_back} ms, QPC {qpc_back} ms) - untouchable rule 3. {}",
        context()
    );
    assert!(
        stdout.lines().any(|l| l.contains("\"session_verdict\"") && l.contains("\"session.left_running\"")),
        "the session let a running application go without saying so. {}",
        context()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

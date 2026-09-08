//! Chrono Mock command-line interface - a first-class interface from v0.1
//! (chrono-mock.md 11.1 item 15), not an add-on to the GUI.
//!
//! One binary, four roles and two questions - `chrono version`, which names the build, which of the
//! two cores this executable is, and the protocol it speaks, and `chrono license`, which names the
//! licence, the warranty disclaimer and the components linked into this build:
//!   * `chrono run <target> ...` - the friendly driver ([`run`]). Spawns the core process
//!     and speaks the machine protocol (ADR-6) to it over stdio.
//!   * `chrono calc ...` - the date calculator ([`calc`]), the product's second half.
//!   * `chrono __core` - the hidden core mode ([`core`]). Reads commands, drives the
//!     mechanism layer, emits protocol events on stdout.
//!   * `chrono __cdp-*` - hidden diagnostic probes for the Chromium path ([`cdp_probe`]).
//!
//! This file is the dispatcher and nothing else. Everything it names lives in a module
//! beside it, and the direction of the dependencies between them is the point: the
//! grammar knows nothing of the calculator, the calculator nothing of the driver, and
//! neither of them anything of the mechanism.

/// `chrono calc` - the date calculator.
mod calc;
/// Reading a shipped calendar catalogue and validating it.
mod calendar;
/// The Chromium/Electron substitution mechanism (CDP faketime), a parallel path to the native core.
mod cdp;
/// What a Chromium session covered, and the verdict that follows from it.
mod cdp_audit;
/// The clock a Chromium session runs on, and the arithmetic that moves it.
mod cdp_clock;
/// Hidden diagnostic probes for the Chromium path.
mod cdp_probe;
/// The Chromium/Electron session - the second substitution mechanism.
mod cdp_session;
/// The command surface: version, bitness, usage texts.
mod cli;
/// `chrono __core` - the hidden core mode.
mod core;
/// Writing protocol events, and the shapes both mechanisms share.
mod events;
/// The step grammar shared by the calculator flags and the preset reader.
mod grammar;
/// Presets: a named moment with parameters (docs/04 section 4).
mod preset;
/// The terminal report and the evidence export for a finished session.
mod report;
/// `chrono run` - the friendly driver.
mod run;
/// Test-only helpers shared by more than one module.
#[cfg(test)]
mod testutil;
/// One NDJSON line off the machine protocol, bounded.
mod wire;
/// Session zone and instant conversions (untouchable rule 2).
mod zone;

use calc::calc_run;
use cdp_probe::{cdp_date_probe, cdp_launch_probe, cdp_probe, cdp_shim_probe};
use cli::{print_license, print_usage, print_version};
use core::core_mode;
use run::driver_run;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        Some("__core") => core_mode(),
        Some("run") => driver_run(&args[2..]),
        Some("calc") => calc_run(&args[2..]),
        Some("__cdp-probe") => cdp_probe(&args[2..]),
        Some("__cdp-launch") => cdp_launch_probe(&args[2..]),
        Some("__cdp-shim") => cdp_shim_probe(&args[2..]),
        Some("__cdp-date") => cdp_date_probe(&args[2..]),
        Some("version") | Some("--version") | Some("-V") => {
            print_version();
            0
        }
        Some("license") | Some("--license") => print_license(&args[2..]),
        Some("--help") | Some("-h") | None => {
            print_usage();
            0
        }
        Some(other) => {
            eprintln!("chrono: unknown command '{other}'");
            print_usage();
            1
        }
    };
    std::process::exit(code);
}

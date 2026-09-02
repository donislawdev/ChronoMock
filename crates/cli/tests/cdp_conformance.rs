//! Opt-in end-to-end CDP conformance (RELEASE-005).
//!
//! No portable CDP target ships with the repo. The `is_chromium_target` detection keys off an
//! Electron/Chromium app's runtime files (`icudtl.dat` + a V8 snapshot) sitting BESIDE the exe, which an
//! installed browser does not satisfy - Chrome and Edge keep those in a versioned subfolder, not next to
//! the launcher - and some hardened Electron apps (e.g. Discord) refuse the remote-debugging port. So there
//! is no fixed target a CI job could rely on. This test is therefore PARAMETERISED: point it at a permissive
//! Chromium/Electron target and it drives the shipped `chrono` binary end to end (launch -> attach -> shim
//! -> the page's own JS `Date`), asserting the page reads the faked clock rather than the real one.
//!
//! It is `#[ignore]` (it does not run in a normal `cargo test`, since it launches a real browser) and skips
//! cleanly when the target is absent or not Chromium-shaped, so it is safe to invoke anywhere.
//!
//! Run it:
//!   set CHRONO_CDP_TARGET=C:\path\to\electron-app.exe
//!   cargo test -p chrono-cli --test cdp_conformance -- --ignored
//!
//! The full chain was verified manually against Pomotroid (Electron) during the CDP bring-up - see the
//! README support matrix, "Electron / Chromium ... measured (Pomotroid, x64)". This harness makes that
//! repeatable against any permissive target.

use std::process::Command;

#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches that app"]
fn cdp_shim_fakes_the_page_clock() {
    let Ok(target) = std::env::var("CHRONO_CDP_TARGET") else {
        eprintln!("CHRONO_CDP_TARGET not set - skipping (no portable CDP target ships with the repo)");
        return;
    };

    let output = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["__cdp-date", &target, "2038-01-19T03:14:07"])
        .output()
        .expect("run chrono __cdp-date");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The target was not Chromium-shaped (an installed browser, or the wrong exe): skip, do not fail - the
    // runner simply pointed at a target this mode cannot drive.
    if stdout.contains("not a chromium target") {
        eprintln!("'{target}' is not a Chromium/Electron app (runtime files not beside the exe) - skipping");
        return;
    }

    assert!(
        output.status.success(),
        "chrono __cdp-date failed (exit {:?}) - the target may block the remote-debugging port\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    // The page's own new Date()/Date.now() must read the faked 2038 clock, proving the whole CDP chain took
    // effect (launch -> attach -> shim -> JS time API), not merely that the browser opened.
    assert!(stdout.contains("2038"), "the page did not read the faked 2038 clock\nstdout: {stdout}");
}

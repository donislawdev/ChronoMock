//! Opt-in end-to-end CDP conformance (RELEASE-005).
//!
//! No portable CDP target ships with the repo. The `is_chromium_target` detection keys off the Chromium
//! runtime files (`icudtl.dat` + a V8 snapshot) beside the exe, or, as an installed Chromium browser keeps
//! them, in a folder named after its version beside the exe together with the library the exe loads -
//! and some hardened Electron apps refuse the remote-debugging port outright. So there
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
//! The tests take turns on their own (`one_browser`), so they need no `--test-threads=1`.
//!
//! The full chain was verified by hand against a real Electron application during the CDP bring-up - see
//! the README support matrix, "Electron / Chromium ... measured by hand on an Electron app (x64)". This
//! harness makes that repeatable against any permissive target.

use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held by each test for as long as it runs, so the tests start one browser at a time whichever way they
/// are run. Side by side they see each other: `page_reads` asks the newest session profile, which can be
/// another test's - the zone test read a session in another zone - and the closed-window test skips
/// while the target runs outside its session. Cargo runs tests in parallel unless told otherwise.
static ONE_BROWSER: Mutex<()> = Mutex::new(());

/// Take the one browser for this test. A test that panicked holding it leaves nothing behind the lock
/// to protect, so the next test takes it all the same.
fn one_browser() -> MutexGuard<'static, ()> {
    ONE_BROWSER.lock().unwrap_or_else(PoisonError::into_inner)
}

#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches that app"]
fn cdp_shim_fakes_the_page_clock() {
    let _one = one_browser();
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

    // The target was not Chromium-shaped (the wrong exe): skip, do not fail - the runner simply pointed at
    // a target this mode cannot drive.
    if stdout.contains("not a chromium target") {
        eprintln!("'{target}' is not a Chromium/Electron app (no Chromium runtime beside the exe or in its version folder) - skipping");
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

/// R2-W1 end to end: a session whose window is CLOSED while it runs must still report what it covered.
///
/// This is the shape the unit test on `cdp_verdict` cannot reach. That test pins the rule the function
/// applies - this one pins what the session hands it. Before the fix the session counted the contexts
/// still ATTACHED, and Chromium destroys its targets while shutting down - so a healthy run turned into
/// "DID NOT TAKE EFFECT (contexts: 0)" with exit code 11 and no coverage at all. Measured on a real
/// Electron application: a taskkill does NOT reproduce it (the socket dies before the destroy events
/// arrive), only a graceful window close does.
#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches and closes that app"]
fn a_closed_window_does_not_erase_what_the_session_covered() {
    let _one = one_browser();
    let Ok(target) = std::env::var("CHRONO_CDP_TARGET") else {
        eprintln!("CHRONO_CDP_TARGET not set - skipping");
        return;
    };
    // The windows are closed by the executable's name, so a browser of the tester's own running beside
    // the session would have its windows closed too.
    if running_count(&target) > 0 {
        eprintln!("'{target}' is already running outside the session, and its windows would be closed - skipping");
        return;
    }

    let child = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", "--at", "2038-01-01T00:00:00", "--mode", "x60", "--ticks", "30", &target])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run chrono");

    // Let the app attach, shim and actually call a time API, then close its window the polite way.
    std::thread::sleep(std::time::Duration::from_secs(12));
    let closing = std::time::Instant::now();
    let all_closed = close_windows_of(&target);
    let closed = std::time::Instant::now();

    let out = child.wait_with_output().expect("collect chrono output");
    let after_close = closed.elapsed();
    eprintln!("closing the windows took {:?}, and the session ended {after_close:?} after", closed - closing);
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !all_closed {
        // Measured on an installed browser on a fresh profile: a welcome window, then a dialog that holds
        // the browser's window open until the person makes a choice. The session rightly ran on, and
        // nothing here says what a close would have done.
        eprintln!("the browser kept a window open through 15 s of polite closing - skipping:\n{stdout}");
        return;
    }

    if stdout.contains("not a Chromium") || stdout.contains("launch_failed") {
        eprintln!("target is not Chromium-shaped or refused the debug port - skipping: {stdout}");
        return;
    }

    assert!(
        !stdout.contains("contexts: 0"),
        "the session covered contexts; a closing window must not erase them:
{stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "a covered session must not exit as a failure:
{stdout}");
    // Closing the windows ends the session, well before its 30 ticks would have.
    assert!(after_close < std::time::Duration::from_secs(10), "the session ran on {after_close:?} after the windows closed:\n{stdout}");
    // A browser that runs from the process the session launched hands nothing over when it closes
    // (R4-S18): its connection drops within milliseconds of the exit, inside the margin. An installed
    // browser is not that browser - it hands its work to a new process of its own as it starts
    // (measured on two installed browsers, 2026-11-09), so its session follows that one from the start,
    // and the report rightly says so.
    let runtime_beside = std::path::Path::new(&target).parent().is_some_and(|dir| dir.join("icudtl.dat").exists());
    if runtime_beside {
        assert!(!stdout.contains("followed:"), "a browser that closed handed nothing over:\n{stdout}");
        assert!(!stdout.contains("handing the application over"), "and the report does not say it did:\n{stdout}");
    }
}

/// The pages read the session's zone (R4/16). Every context is put on it through the engine's zone
/// override, and the shim in each checks the offset it reads - so a session in `+05:30`, which no host
/// in this project's measurements is in, would say `chromium.zone_is_host` if any page or worker read
/// the machine's zone. Before R4/16 the pages always read the machine's.
///
/// The page is also asked directly, by a second DevTools client while the session runs: a session that
/// set no zone at all would check none, and say nothing either.
#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches that app"]
fn the_pages_read_the_session_zone() {
    let _one = one_browser();
    let Ok(target) = std::env::var("CHRONO_CDP_TARGET") else {
        eprintln!("CHRONO_CDP_TARGET not set - skipping");
        return;
    };
    let child = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &target, "--at", "2038-01-19T03:14:07", "--zone", "+05:30", "--json", "--ticks", "10"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run chrono");
    std::thread::sleep(std::time::Duration::from_secs(5));
    let read = page_reads("new Date(Date.UTC(2038,0,19,3,14,7)).getTimezoneOffset()");
    let out = child.wait_with_output().expect("collect chrono output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains("target.launch_failed") || stdout.contains("target.attach_failed") {
        eprintln!("the target refused the debug port - skipping: {stdout}");
        return;
    }
    assert_eq!(read.trim(), "-330", "the page reads +05:30, the session's zone:\n{stdout}");
    assert!(stdout.contains("\"chromium.contexts_covered\""), "the session covered the pages:\n{stdout}");
    assert!(!stdout.contains("chromium.zone_is_host"), "every page and worker read the session's zone:\n{stdout}");
    assert_eq!(out.status.code(), Some(0), "{stdout}");
}

/// R4-N28 end to end, in two sessions at x60 whose browser window is minimized: without
/// `--keep-background-timers` the report says the hidden page's timers may have been slowed, and with
/// it the browser was started with the three switches (read off its command line while it runs) and
/// the report says nothing of it. An Electron target is also said to have a main process the session
/// does not reach (R4-S19), and any other is not.
#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches and minimizes that app"]
fn a_hidden_window_is_said_unless_its_timers_were_kept() {
    let _one = one_browser();
    let Ok(target) = std::env::var("CHRONO_CDP_TARGET") else {
        eprintln!("CHRONO_CDP_TARGET not set - skipping");
        return;
    };
    let session = |keep: bool| {
        let mut args = vec!["run", target.as_str(), "--at", "2038-01-19T03:14:07", "--mode", "x60", "--json", "--ticks", "12"];
        if keep {
            args.push("--keep-background-timers");
        }
        let child = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run chrono");
        std::thread::sleep(std::time::Duration::from_secs(5));
        let command_line = browser_powershell(
            "(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*chrono-cdp-*' -and $_.CommandLine -notlike '*--type=*' } | Select-Object -First 1).CommandLine",
        );
        browser_powershell(
            "Add-Type -Namespace R416 -Name Win -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool ShowWindow(System.IntPtr h, int c);'
             Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*chrono-cdp-*' -and $_.CommandLine -notlike '*--type=*' } |
             ForEach-Object { $p = Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
                              if ($p -and $p.MainWindowHandle -ne [IntPtr]::Zero) { [void][R416.Win]::ShowWindow($p.MainWindowHandle, 6) } }",
        );
        let out = child.wait_with_output().expect("collect chrono output");
        (String::from_utf8_lossy(&out.stdout).into_owned(), command_line)
    };

    let (slowed, _) = session(false);
    if slowed.contains("target.launch_failed") || slowed.contains("target.attach_failed") {
        eprintln!("the target refused the debug port - skipping: {slowed}");
        return;
    }
    assert!(slowed.contains("\"chromium.background_timers_slowed\""), "a page hidden at x60 is said:\n{slowed}");

    let (kept, command_line) = session(true);
    assert!(command_line.contains("--disable-background-timer-throttling"), "the browser got the switches: {command_line}");
    assert!(command_line.contains("--disable-renderer-backgrounding"), "{command_line}");
    assert!(command_line.contains("--disable-backgrounding-occluded-windows"), "{command_line}");
    assert!(!kept.contains("chromium.background_timers_slowed"), "kept timers are not said to be slowed:\n{kept}");

    let resources = std::path::Path::new(&target).parent().map(|d| d.join("resources"));
    let electron = resources.is_some_and(|r| r.join("app.asar").exists() || r.join("app").is_dir() || r.join("default_app.asar").exists());
    assert_eq!(kept.contains("\"chromium.main_process_uncovered\""), electron, "the main process is said of an Electron app only:\n{kept}");
}

/// Run a Windows PowerShell script for a check of the browser the session started, and hand back what
/// it printed. Written to a file and run with `-File`, because the quotes a script carries do not
/// survive a trip through a command line intact.
fn browser_powershell(script: &str) -> String {
    let file = std::env::temp_dir().join(format!("chrono-conformance-{}-{}.ps1", std::process::id(), script.len()));
    std::fs::write(&file, script).expect("script file");
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &file.display().to_string()])
        .output()
        .expect("run powershell");
    let _ = std::fs::remove_file(&file);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the first page of the running session's browser evaluates `expr` to, asked by a second
/// DevTools client - the browser takes several. The port comes from the session profile's
/// `DevToolsActivePort`, under TMP or TEMP (they differ on some machines, and the core reads TMP),
/// and the client speaks only to the loopback address. Windows PowerShell, which every Windows has.
fn page_reads(expr: &str) -> String {
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$file = @($env:TMP, $env:TEMP) | Select-Object -Unique | ForEach-Object {{ Get-ChildItem -Path $_ -Filter 'chrono-cdp-*' -Directory -ErrorAction SilentlyContinue }} |
    ForEach-Object {{ Join-Path $_.FullName 'DevToolsActivePort' }} | Where-Object {{ Test-Path $_ }} |
    Sort-Object {{ (Get-Item $_).LastWriteTime }} -Descending | Select-Object -First 1
$port = [int](Get-Content $file | Select-Object -First 1)
$page = (Invoke-RestMethod "http://127.0.0.1:$port/json/list") | Where-Object {{ $_.type -eq 'page' }} | Select-Object -First 1
$ws = New-Object System.Net.WebSockets.ClientWebSocket
$cts = New-Object System.Threading.CancellationTokenSource 10000
$ws.ConnectAsync([Uri]$page.webSocketDebuggerUrl, $cts.Token).Wait()
$msg = @{{ id = 7; method = 'Runtime.evaluate'; params = @{{ expression = '{expr}'; returnByValue = $true }} }} | ConvertTo-Json -Compress -Depth 5
$bytes = [Text.Encoding]::UTF8.GetBytes($msg)
$ws.SendAsync((New-Object ArraySegment[byte] -ArgumentList (,$bytes)), 'Text', $true, $cts.Token).Wait()
$buf = New-Object byte[] 65536
while ($true) {{
    $text = ''
    do {{
        $r = $ws.ReceiveAsync((New-Object ArraySegment[byte] -ArgumentList (,$buf)), $cts.Token).Result
        $text += [Text.Encoding]::UTF8.GetString($buf, 0, $r.Count)
    }} while (-not $r.EndOfMessage)
    $reply = $text | ConvertFrom-Json
    if ($reply.id -eq 7) {{ $reply.result.result.value; break }}
}}
"#
    );
    let out = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().expect("run powershell");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// R4-S18 end to end: a target that starts the browser and exits - a launcher - must not end the
/// session. Before the fix the session ended with the launcher in about 0.4 s, with no verdict, and
/// took the browser with it.
///
/// The launcher is a script beside empty runtime files, so the folder reads as a Chromium target, and
/// it hands the session's own arguments (profile and debugging port) to the real target with `start`.
#[test]
#[ignore = "opt-in: set CHRONO_CDP_TARGET to a permissive Chromium/Electron exe; it launches that app through a script"]
fn a_launcher_that_hands_the_app_over_does_not_end_the_session() {
    let _one = one_browser();
    let Ok(target) = std::env::var("CHRONO_CDP_TARGET") else {
        eprintln!("CHRONO_CDP_TARGET not set - skipping");
        return;
    };
    let stand = std::env::temp_dir().join(format!("chrono-s18-launcher-{}", std::process::id()));
    std::fs::create_dir_all(&stand).expect("stand folder");
    for runtime in ["icudtl.dat", "snapshot_blob.bin"] {
        std::fs::write(stand.join(runtime), b"").expect("runtime marker");
    }
    let launcher = stand.join("launcher.cmd");
    std::fs::write(&launcher, format!("@echo off\r\nstart \"\" \"{target}\" %*\r\n")).expect("launcher script");

    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &launcher.display().to_string(), "--at", "2038-01-19T03:14:07", "--json", "--ticks", "6"])
        .output()
        .expect("run chrono");
    let _ = std::fs::remove_dir_all(&stand);
    let stdout = String::from_utf8_lossy(&out.stdout);

    if stdout.contains("target.launch_failed") || stdout.contains("target.attach_failed") {
        eprintln!("the target refused the debug port - skipping: {stdout}");
        return;
    }
    let states = stdout.lines().filter(|l| l.contains("\"type\":\"state\"")).count();
    assert!(states >= 6, "the session ran to its ticks rather than ending with the launcher ({states} states):\n{stdout}");
    assert!(stdout.contains("\"chromium.followed_browser\""), "the report says why the session outlived its target:\n{stdout}");
    let image = std::path::Path::new(&target).file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let verdict = stdout.lines().find(|l| l.contains("\"type\":\"session_verdict\"")).unwrap_or_default();
    assert!(
        verdict.to_ascii_lowercase().contains(&format!("\"image\":\"{}\"", image.to_ascii_lowercase())),
        "the browser the session went on with is named in `followed`:\n{verdict}"
    );
    assert_eq!(out.status.code(), Some(0), "a covered session exits 0:\n{stdout}");
}

/// Ask every window of that executable to close, the way a user would - not a kill, which tears the
/// socket down before Chromium sends the target-destroyed events this test is about. In rounds until no
/// window is left, for up to 15 seconds: an installed browser on a fresh profile opens with a welcome
/// window, and closing it opens the browser's own window (measured 2026-11-09), so one round closed the
/// welcome and the browser ran on. Whether every window closed.
fn close_windows_of(target: &str) -> bool {
    let script = format!(
        "$deadline = (Get-Date).AddSeconds(15)
         do {{
             $open = @(Get-Process -Name '{name}' -ErrorAction SilentlyContinue | Where-Object {{ $_.MainWindowHandle -ne [IntPtr]::Zero }})
             $open | ForEach-Object {{ $_.CloseMainWindow() | Out-Null }}
             if ($open.Count -gt 0) {{ Start-Sleep -Milliseconds 1000 }}
         }} while ($open.Count -gt 0 -and (Get-Date) -lt $deadline)
         $open.Count",
        name = image_stem(target)
    );
    let out = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().expect("run powershell");
    String::from_utf8_lossy(&out.stdout).trim() == "0"
}

/// How many processes of that executable's name are running.
fn running_count(target: &str) -> usize {
    let script = format!("@(Get-Process -Name '{}' -ErrorAction SilentlyContinue).Count", image_stem(target));
    let out = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().expect("run powershell");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

/// The executable's name without its extension, as `Get-Process -Name` takes it.
fn image_stem(target: &str) -> &str {
    std::path::Path::new(target).file_stem().and_then(|s| s.to_str()).unwrap_or(target)
}

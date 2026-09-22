//! Embedded web engines: a Chromium that lives INSIDE the application under test (WebView2 in a
//! .NET or native host, Qt WebEngine in a Qt or Python host, CEF in whatever embeds it), as opposed
//! to a Chromium that IS the application, which the `cdp` path drives.
//!
//! Measured 2026-09-21 on one host of each of the first two kinds: the native session covers the
//! host and the browser process, and the RENDERER - the process the page's `Date.now()` and timers
//! live in - is spawned through the Chromium sandbox, which bypasses the CreateProcess* detours, so
//! nothing injects into it and the page runs on the real clock while the host runs on the session
//! clock. This module is what lets the audit say so (untouchable rule 4): it reads the role of an
//! uncovered child off its command line and recognises an engine's subprocesses by image name.
//! Reaching them is a later slice, and it starts here too.

/// The `--type=` value every Chromium-based engine puts on the command line of each subprocess it
/// spawns: `renderer`, `gpu-process`, `utility`, `crashpad-handler` and so on. The one signal that
/// tells a renderer apart from the rest, and the same for WebView2, Qt WebEngine and CEF - a CEF
/// subprocess is usually the application's own executable relaunched, so its image name says
/// nothing and this is all there is.
///
/// Returns the value only when the whole of it is a word a Chromium role is made of - ASCII
/// letters, digits, `-` and `_`, at most 32 of them. Anything else is NOT a role and reads as none:
/// the command line is text from the target's world, the value goes on the wire and into the report
/// as a word, and a token that had to be cleaned up to look like `renderer` must not be taken for
/// one (a stripped-down `ren|derer` would have made the strong claim on no evidence). A token inside
/// quotes is not matched - Chromium never quotes this one.
pub(crate) fn role_from_command_line(command_line: &str) -> Option<String> {
    const MAX_ROLE_CHARS: usize = 32;
    let value = command_line.split_whitespace().find_map(|token| token.strip_prefix("--type="))?;
    let is_role_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let valid = !value.is_empty() && value.len() <= MAX_ROLE_CHARS && value.chars().all(is_role_char);
    valid.then(|| value.to_string())
}

/// Whether a role read by [`role_from_command_line`] is the renderer - the process the page's
/// JavaScript runs in. An uncovered renderer means every page in it read the real clock.
pub(crate) fn is_renderer_role(role: &str) -> bool {
    role == "renderer"
}

/// Whether an uncovered child is a subprocess of an embedded Chromium web engine, by the image name
/// its runtime gives every renderer, GPU and utility process. These are the names of RUNTIMES, not
/// of any application: the WebView2 Runtime's `msedgewebview2.exe` and Qt WebEngine's
/// `QtWebEngineProcess.exe`. Case-insensitive, because the OS is.
///
/// This says "an engine is in this application", not "its renderer is uncovered" - the same name
/// belongs to the GPU and utility processes. The renderer question is answered by the role.
pub(crate) fn is_web_engine_subprocess(image: &str) -> bool {
    const ENGINE_SUBPROCESSES: &[&str] = &["msedgewebview2.exe", "qtwebengineprocess.exe"];
    let lower = image.to_ascii_lowercase();
    ENGINE_SUBPROCESSES.contains(&lower.as_str())
}

/// The variable WebView2 reads extra browser switches from. Its value is APPENDED to whatever the
/// host passes in `additionalBrowserArguments`, so the host's own switches survive (Microsoft
/// Learn, WebView2 Win32 reference, "Globals"). It takes precedence over the registry override of
/// the same name, which is the one thing the channel cannot preserve.
pub(crate) const WEBVIEW2_ARGUMENTS_VAR: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// The variable Qt WebEngine opens its DevTools endpoint from. The port has to be explicit:
/// measured 2026-09-21, a zero opens nothing.
pub(crate) const QT_DEBUGGING_VAR: &str = "QTWEBENGINE_REMOTE_DEBUGGING";

/// The switch that makes a Chromium open a DevTools port. Zero is an ephemeral port, documented for
/// WebView2 - the channel finds the number in the TCP table, so it never needs to know it up front.
const REMOTE_DEBUGGING_PORT_SWITCH: &str = "--remote-debugging-port";

/// The two variables that make an embedded engine open a DevTools port, merged with what the
/// tester's environment already says - `current` is looked up by name, without regard to case, the
/// way the system looks variables up.
///
/// - WebView2: our switch is added after whatever the tester set, unless they already chose a
///   debugging port or pipe themselves - two port switches on one command line are a coin toss,
///   and the table read finds their port as well as ours.
/// - Qt: the tester's value stands untouched when there is one (an explicit port of their own
///   choosing), otherwise `127.0.0.1:<qt_port>`, a port the caller found free a moment ago.
///
/// Pure over `current`, so the merge is tested without an environment.
pub(crate) fn engine_env(current: &[(String, String)], qt_port: u16) -> Vec<(String, String)> {
    let lookup = |name: &str| {
        current.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    };
    let mut out = Vec::new();

    let webview2 = lookup(WEBVIEW2_ARGUMENTS_VAR).unwrap_or("");
    if !has_debugging_switch(webview2) {
        let value = if webview2.trim().is_empty() {
            format!("{REMOTE_DEBUGGING_PORT_SWITCH}=0")
        } else {
            format!("{} {REMOTE_DEBUGGING_PORT_SWITCH}=0", webview2.trim_end())
        };
        out.push((WEBVIEW2_ARGUMENTS_VAR.to_string(), value));
    }

    if lookup(QT_DEBUGGING_VAR).is_none_or(|v| v.trim().is_empty()) {
        out.push((QT_DEBUGGING_VAR.to_string(), format!("127.0.0.1:{qt_port}")));
    }

    out
}

/// Whether a switch list already asks for a DevTools endpoint, by port or by pipe.
fn has_debugging_switch(switches: &str) -> bool {
    switches
        .split_whitespace()
        .any(|t| t.starts_with(REMOTE_DEBUGGING_PORT_SWITCH) || t.starts_with("--remote-debugging-pipe"))
}

/// Whether a `/json/version` reply is a Chromium DevTools endpoint's: it names the WebSocket URL
/// of its browser target. Any listener the family holds gets asked, and an application's own HTTP
/// server answering with something else is not an engine.
pub(crate) fn is_devtools_version(reply: &serde_json::Value) -> bool {
    reply.get("webSocketDebuggerUrl").and_then(serde_json::Value::as_str).is_some_and(|u| !u.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(n: &str, v: &str) -> (String, String) {
        (n.to_string(), v.to_string())
    }

    #[test]
    fn a_clean_environment_gets_both_variables() {
        let env = engine_env(&[], 9333);
        assert_eq!(
            env,
            vec![
                pair(WEBVIEW2_ARGUMENTS_VAR, "--remote-debugging-port=0"),
                pair(QT_DEBUGGING_VAR, "127.0.0.1:9333"),
            ]
        );
    }

    #[test]
    fn the_tester_s_own_webview2_switches_survive_with_ours_appended() {
        let env = engine_env(&[pair("webview2_additional_browser_arguments", "--disable-gpu ")], 1);
        assert_eq!(env[0], pair(WEBVIEW2_ARGUMENTS_VAR, "--disable-gpu --remote-debugging-port=0"));
    }

    #[test]
    fn a_tester_who_chose_a_debugging_endpoint_keeps_it_and_gets_no_second_one() {
        let port = engine_env(&[pair(WEBVIEW2_ARGUMENTS_VAR, "--remote-debugging-port=9222")], 1);
        assert!(port.iter().all(|(n, _)| n != WEBVIEW2_ARGUMENTS_VAR));
        let pipe = engine_env(&[pair(WEBVIEW2_ARGUMENTS_VAR, "--remote-debugging-pipe")], 1);
        assert!(pipe.iter().all(|(n, _)| n != WEBVIEW2_ARGUMENTS_VAR));
        let qt = engine_env(&[pair("QtWebEngine_Remote_Debugging", "127.0.0.1:5555")], 1);
        assert!(qt.iter().all(|(n, _)| n != QT_DEBUGGING_VAR));
        // An empty Qt value is no choice at all.
        let empty = engine_env(&[pair(QT_DEBUGGING_VAR, "  ")], 7);
        assert!(empty.contains(&pair(QT_DEBUGGING_VAR, "127.0.0.1:7")));
    }

    #[test]
    fn a_devtools_version_reply_names_its_browser_endpoint_and_anything_else_does_not() {
        assert!(is_devtools_version(&serde_json::json!({ "Browser": "Edg/153", "webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/browser/x" })));
        assert!(!is_devtools_version(&serde_json::json!({ "webSocketDebuggerUrl": "" })));
        assert!(!is_devtools_version(&serde_json::json!({ "status": "ok" })));
        assert!(!is_devtools_version(&serde_json::json!("text")));
    }

    #[test]
    fn recognises_both_engines_case_insensitively() {
        assert!(is_web_engine_subprocess("msedgewebview2.exe"));
        assert!(is_web_engine_subprocess("MSEDGEWEBVIEW2.EXE"));
        assert!(is_web_engine_subprocess("QtWebEngineProcess.exe"));
    }

    #[test]
    fn an_ordinary_child_is_not_an_engine() {
        assert!(!is_web_engine_subprocess("helper.exe"));
        assert!(!is_web_engine_subprocess("conhost.exe"));
        assert!(!is_web_engine_subprocess(""));
    }

    /// The role sits anywhere on a real Chromium command line, among quoted paths and other flags.
    #[test]
    fn reads_the_role_off_a_chromium_command_line() {
        let line = r#""C:\Program Files\Engine\engine.exe" --type=renderer --field-trial-handle=1 --lang=en"#;
        assert_eq!(role_from_command_line(line).as_deref(), Some("renderer"));
        let gpu = r#""C:\x\app.exe" --disable-gpu-sandbox --type=gpu-process --gpu-preferences=abc"#;
        assert_eq!(role_from_command_line(gpu).as_deref(), Some("gpu-process"));
        assert!(is_renderer_role("renderer"));
        assert!(!is_renderer_role("gpu-process"));
    }

    /// No `--type=` means no role - a plain child says nothing about web engines. A quoted token,
    /// a `--type` without a value and an empty value all read as none rather than as something.
    #[test]
    fn a_command_line_without_a_role_reads_as_none() {
        assert_eq!(role_from_command_line(r#"C:\tools\p1.exe 500 3"#), None);
        assert_eq!(role_from_command_line(r#"app.exe "--type=renderer""#), None);
        assert_eq!(role_from_command_line("app.exe --type renderer"), None);
        assert_eq!(role_from_command_line("app.exe --type="), None);
        assert_eq!(role_from_command_line(""), None);
    }

    /// The value is the target's text: a token that is not wholly a word is not a role, so nothing
    /// the target writes there can reach the report as anything but a word - and nothing it writes
    /// can be cleaned up INTO the one word that makes the strong claim.
    #[test]
    fn a_role_is_a_whole_word_or_nothing() {
        let long = format!("app.exe --type={}", "x".repeat(100));
        assert_eq!(role_from_command_line(&long), None, "too long is not a role");
        assert_eq!(role_from_command_line("app.exe --type=ren|derer\"rm"), None, "never stripped into one");
        // A Unicode line separator is white space to the tokenizer: it ends the token, it is never
        // part of one, so the value before it is the whole role and the value after it is another token.
        assert_eq!(role_from_command_line("app.exe --type=renderer\u{2028}x").as_deref(), Some("renderer"));
        assert_eq!(role_from_command_line("app.exe --type=\u{2028}"), None);
        assert_eq!(role_from_command_line("app.exe --type=crashpad-handler").as_deref(), Some("crashpad-handler"));
        assert_eq!(role_from_command_line("app.exe --type=ppapi_broker").as_deref(), Some("ppapi_broker"));
    }
}

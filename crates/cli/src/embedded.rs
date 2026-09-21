//! Embedded web engines: a Chromium that lives INSIDE the application under test (WebView2 in a
//! .NET or native host, Qt WebEngine in a Qt or Python host), as opposed to a Chromium that IS the
//! application, which the `cdp` path drives.
//!
//! Measured 2026-09-21 on one host of each kind: the native session covers the host and the browser
//! process, and the RENDERER - the process the page's `Date.now()` and timers live in - is spawned
//! through the Chromium sandbox, which bypasses the CreateProcess* detours, so nothing injects into
//! it and the page runs on the real clock while the host runs on the session clock. This module is
//! what lets the audit say so (untouchable rule 4): it recognises the engine's subprocesses among
//! the children the hook did not follow into. Reaching them is a later slice, and it starts here too.

/// Whether an uncovered child is a subprocess of an embedded Chromium web engine, by the image name
/// its runtime gives every renderer, GPU and utility process. These are the names of RUNTIMES, not
/// of any application: the WebView2 Runtime's `msedgewebview2.exe` and Qt WebEngine's
/// `QtWebEngineProcess.exe`. Case-insensitive, because the OS is.
///
/// A CEF host is NOT recognised here yet: its subprocess is usually the application's own executable
/// relaunched with `--type=renderer`, which only the command line tells apart, and this slice does
/// not read command lines.
pub(crate) fn is_web_engine_subprocess(image: &str) -> bool {
    const ENGINE_SUBPROCESSES: &[&str] = &["msedgewebview2.exe", "qtwebengineprocess.exe"];
    let lower = image.to_ascii_lowercase();
    ENGINE_SUBPROCESSES.contains(&lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

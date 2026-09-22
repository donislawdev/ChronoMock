//! The one registry value the embedded-engine channel has to know about (docs/09 section 12.10).
//!
//! WebView2 reads extra browser arguments from two places: the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
//! environment variable, and a registry value under
//! `Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments` named after the host
//! executable (or `*` for every host). Microsoft Learn (`CreateCoreWebView2EnvironmentWithOptions`):
//! the registry is examined only when none of the environment variables exist. A session sets the
//! variable, so for its duration a value the tester put in the registry is not read - their own
//! debugging port, say. This module answers one question, read-only: is such a value there, so the
//! session can say that it is being hidden. Nothing here writes.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{NO_ERROR, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_RT_REG_SZ,
};

/// The key both hives are asked under.
const KEY: &str = r"Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments";

/// Whether a WebView2 `AdditionalBrowserArguments` policy value applies to a host executable: a
/// value named after the executable's file name, or the `*` value, in HKLM or HKCU. The order and
/// the names are the ones the runtime uses (Microsoft Learn), and a value that is not a string does
/// not count - the runtime would not read it either.
pub fn webview2_arguments_policy_present(exe_name: &str) -> bool {
    policy_present_with(exe_name, reg_get_value_status)
}

/// One registry read: the status `RegGetValueW` returns when asked whether `value` under `key` in
/// `hive` exists as the given kind, with no data buffer. The boundary the tests stand in for.
type ValueStatus = fn(HKEY, &[u16], &[u16], REG_ROUTINE_FLAGS) -> WIN32_ERROR;

/// The lookup over any reader, so the walk - both hives, the executable's name before the
/// wildcard, string values only - is tested with a reader of the test's making.
fn policy_present_with(exe_name: &str, status: ValueStatus) -> bool {
    let key = wide(KEY);
    [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER].into_iter().any(|hive| {
        [exe_name, "*"].into_iter().any(|name| status(hive, &key, &wide(name), RRF_RT_REG_SZ) == NO_ERROR)
    })
}

fn reg_get_value_status(hive: HKEY, key: &[u16], value: &[u16], flags: REG_ROUTINE_FLAGS) -> WIN32_ERROR {
    let mut size: u32 = 0;
    // SAFETY: both strings are NUL-terminated (`wide` appends the terminator) and outlive the call,
    // no data buffer is asked for (null pointer with a size out-parameter is the documented way to ask
    // whether a value exists and how large it is), and the call touches nothing else.
    unsafe { RegGetValueW(hive, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), flags, None, None, Some(&mut size)) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;

    /// A name no policy on this machine carries: the answer is a plain false, not an error and not a
    /// panic, whichever hive is missing the key altogether.
    #[test]
    fn an_absent_value_is_false_in_both_hives() {
        let key = wide(KEY);
        let name = wide("chrono-mock-no-such-host-3f9a.exe");
        assert_ne!(reg_get_value_status(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ), NO_ERROR);
        assert_ne!(reg_get_value_status(HKEY_LOCAL_MACHINE, &key, &name, RRF_RT_REG_SZ), NO_ERROR);
    }

    /// Every question the reader is asked, in order, so the walk itself is pinned: the key path,
    /// both hives with HKLM first, the executable's name before the wildcard, string values only.
    static ASKED: Mutex<Vec<(isize, String, String, u32)>> = Mutex::new(Vec::new());

    fn recording(answer_for: &'static str) -> ValueStatus {
        fn text(w: &[u16]) -> String {
            String::from_utf16_lossy(&w[..w.len().saturating_sub(1)])
        }
        // A fn pointer cannot capture, so the answer is chosen by a global the test sets.
        ANSWER_FOR.lock().unwrap().replace(answer_for.to_string());
        |hive, key, value, flags| {
            ASKED.lock().unwrap().push((hive.0 as isize, text(key), text(value), flags.0));
            if Some(text(value)) == *ANSWER_FOR.lock().unwrap() { NO_ERROR } else { ERROR_FILE_NOT_FOUND }
        }
    }

    static ANSWER_FOR: Mutex<Option<String>> = Mutex::new(None);

    /// The positive path, without touching the registry: a reader that says the wildcard value is
    /// there makes the policy present, and the questions it was asked are exactly the runtime's.
    #[test]
    fn a_present_value_is_found_and_the_reader_is_asked_the_runtime_questions() {
        ASKED.lock().unwrap().clear();

        assert!(policy_present_with("host.exe", recording("*")));
        let asked = ASKED.lock().unwrap().clone();
        assert_eq!(asked.len(), 2, "HKLM host.exe (absent), HKLM * (present) - and it stops there: {asked:?}");
        assert_eq!(asked[0].0, HKEY_LOCAL_MACHINE.0 as isize);
        assert_eq!(asked[0].1, KEY);
        assert_eq!(asked[0].2, "host.exe");
        assert_eq!(asked[0].3, RRF_RT_REG_SZ.0);
        assert_eq!(asked[1].2, "*");

        ASKED.lock().unwrap().clear();
        assert!(!policy_present_with("host.exe", recording("something-else")));
        let asked = ASKED.lock().unwrap().clone();
        assert_eq!(asked.len(), 4, "both hives, both names, none present: {asked:?}");
        assert_eq!(asked[2].0, HKEY_CURRENT_USER.0 as isize);
    }
}

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
use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

/// The key both hives are asked under.
const KEY: &str = r"Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments";

/// Whether a WebView2 `AdditionalBrowserArguments` policy value applies to a host executable: a
/// value named after the executable's file name, or the `*` value, in HKLM or HKCU. The order and
/// the names are the ones the runtime uses (Microsoft Learn), and a value that is not a string does
/// not count - the runtime would not read it either.
pub fn webview2_arguments_policy_present(exe_name: &str) -> bool {
    [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
        .into_iter()
        .any(|hive| [exe_name, "*"].into_iter().any(|name| string_value_exists(hive, name)))
}

fn string_value_exists(hive: HKEY, name: &str) -> bool {
    let key = wide(KEY);
    let value = wide(name);
    let mut size: u32 = 0;
    // SAFETY: both strings are NUL-terminated and outlive the call, no data buffer is asked for
    // (null pointer with a size out-parameter is the documented way to ask whether a value exists and
    // how large it is), and the call touches nothing else.
    let status = unsafe {
        RegGetValueW(hive, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut size))
    };
    status == NO_ERROR
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name no policy on this machine carries: the answer is a plain false, not an error and not a
    /// panic, whichever hive is missing the key altogether.
    #[test]
    fn an_absent_value_is_false_in_both_hives() {
        assert!(!string_value_exists(HKEY_CURRENT_USER, "chrono-mock-no-such-host-3f9a.exe"));
        assert!(!string_value_exists(HKEY_LOCAL_MACHINE, "chrono-mock-no-such-host-3f9a.exe"));
    }
}

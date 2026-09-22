//! The environment block a launched target inherits, with the session's own variables added.
//!
//! `CreateProcessW` with a null `lpEnvironment` hands the child a copy of this process's environment,
//! and that is what every session did until the embedded-engine channel (docs/09) needed to put two
//! variables in front of the target. Once a caller supplies a block, the system stops doing three
//! things for it, and this module does them instead:
//!
//! - the drive-directory entries (`=C:=C:\...`) are not propagated - so the block is built from
//!   `GetEnvironmentStringsW`, which carries them, never from `std::env`, which may not,
//! - the block must be sorted by name, case-insensitively in Unicode order,
//! - a Unicode block ends with four zero bytes.
//!
//! All three are documented for `CreateProcessW` and "Changing Environment Variables" on Microsoft
//! Learn. The merge and the encoding are pure functions over `(name, value)` pairs, so they are
//! tested without launching anything - and a live launch with a block is what `launch_plain` proves.

use windows::Win32::System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW};

/// Build the Unicode environment block for a child: this process's environment with `extra` merged
/// in (a name already present is replaced), sorted as the system requires, doubly terminated.
pub fn environment_block(extra: &[(String, String)]) -> Vec<u16> {
    encode_block(&merge_entries(current_environment(), extra))
}

/// This process's environment as it stands in memory, drive-directory entries included. A name is
/// everything before the first `=` past the first character, so an entry like `=C:=C:\work` keeps its
/// leading `=` as part of the name, exactly as the system stores it.
pub fn current_environment() -> Vec<(String, String)> {
    // SAFETY: GetEnvironmentStringsW returns a block owned by the system that stays valid until the
    // matching FreeEnvironmentStringsW, which runs after the last read below. A null pointer (the
    // documented failure) yields an empty environment rather than a read through null.
    unsafe {
        let block = GetEnvironmentStringsW();
        if block.is_null() {
            return Vec::new();
        }
        let mut entries = Vec::new();
        let mut cursor = block.0;
        loop {
            let len = (0..).take_while(|&i| *cursor.add(i) != 0).count();
            if len == 0 {
                break;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(cursor, len));
            entries.push(split_entry(&text));
            cursor = cursor.add(len + 1);
        }
        let _ = FreeEnvironmentStringsW(block);
        entries
    }
}

/// `name=value` into its two halves. The split is at the first `=` AFTER the first character, so a
/// drive-directory entry's leading `=` stays with the name. By character, not by byte: a name may
/// start with a character wider than one byte, and a byte slice at offset one would panic on it.
fn split_entry(text: &str) -> (String, String) {
    match text.char_indices().skip(1).find(|(_, c)| *c == '=') {
        Some((at, _)) => (text[..at].to_string(), text[at + 1..].to_string()),
        None => (text.to_string(), String::new()),
    }
}

/// The current entries with `extra` merged in: a name already present is replaced in place (names
/// compare without regard to case, as the system compares them), a new one is appended. Then the
/// whole list is sorted the way the system requires, so the caller never has to know that a block
/// is sorted at all.
pub fn merge_entries(
    mut entries: Vec<(String, String)>,
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    for (name, value) in extra {
        match entries.iter_mut().find(|(n, _)| names_equal(n, name)) {
            Some(slot) => slot.1 = value.clone(),
            None => entries.push((name.clone(), value.clone())),
        }
    }
    entries.sort_by_key(|(name, _)| sort_key(name));
    entries
}

/// The order the system sorts an environment block in: by name, case-insensitively, in Unicode
/// order without regard to locale. Upper-casing per character is that comparison for every script
/// with a simple case mapping, and the names in a Windows environment are ASCII in practice.
fn sort_key(name: &str) -> Vec<char> {
    name.chars().flat_map(char::to_uppercase).collect()
}

fn names_equal(a: &str, b: &str) -> bool {
    sort_key(a) == sort_key(b)
}

/// The block as `CreateProcessW` reads it with `CREATE_UNICODE_ENVIRONMENT`: each `name=value`
/// zero-terminated, then one more zero after the last - four zero bytes at the end, as documented.
/// An empty list still yields a valid (empty) block of one terminating zero pair, because a block
/// that ends in two zero bytes is the one shape the caller can hand over unconditionally.
pub fn encode_block(entries: &[(String, String)]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for (name, value) in entries {
        out.extend(name.encode_utf16());
        out.push(u16::from(b'='));
        out.extend(value.encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(n: &str, v: &str) -> (String, String) {
        (n.to_string(), v.to_string())
    }

    #[test]
    fn a_present_name_is_replaced_without_regard_to_case_and_a_new_one_is_appended() {
        let merged = merge_entries(
            vec![pair("Path", "C:\\bin"), pair("TEMP", "C:\\t")],
            &[pair("path", "D:\\bin"), pair("CHRONO_X", "1")],
        );
        assert_eq!(
            merged,
            vec![pair("CHRONO_X", "1"), pair("Path", "D:\\bin"), pair("TEMP", "C:\\t")]
        );
    }

    #[test]
    fn the_block_is_sorted_by_name_case_insensitively_with_drive_entries_first() {
        // `=` sorts below every letter, which is why the system keeps the drive-directory entries at
        // the front - and this is the order the system requires, not a preference.
        let merged = merge_entries(
            vec![pair("zeta", "1"), pair("Alpha", "2"), pair("=C:", "C:\\work"), pair("beta", "3")],
            &[],
        );
        let names: Vec<&str> = merged.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["=C:", "Alpha", "beta", "zeta"]);
    }

    #[test]
    fn a_drive_directory_entry_keeps_its_leading_equals_sign_in_the_name() {
        assert_eq!(split_entry("=C:=C:\\work"), pair("=C:", "C:\\work"));
        assert_eq!(split_entry("PATH=C:\\bin;D:\\bin"), pair("PATH", "C:\\bin;D:\\bin"));
        assert_eq!(split_entry("EMPTY="), pair("EMPTY", ""));
        assert_eq!(split_entry("NOEQUALS"), pair("NOEQUALS", ""));
        // A name that starts with a character wider than one byte, and a name that IS one.
        assert_eq!(split_entry("\u{c4}_NAME=v"), pair("\u{c4}_NAME", "v"));
        assert_eq!(split_entry("\u{c4}=v"), pair("\u{c4}", "v"));
        assert_eq!(split_entry("\u{c4}"), pair("\u{c4}", ""));
    }

    #[test]
    fn the_encoded_block_ends_in_four_zero_bytes_and_separates_entries_with_one() {
        let block = encode_block(&[pair("A", "1"), pair("B", "")]);
        let expected: Vec<u16> = "A=1\0B=\0\0".encode_utf16().collect();
        assert_eq!(block, expected);
        assert_eq!(encode_block(&[]), vec![0]);
    }

    #[test]
    fn the_live_environment_reads_back_with_the_names_this_process_can_see() {
        // Not a fixed list - the machine decides its environment - but a process always has a
        // PATH-like set, and the entries come back split at the right place.
        let live = current_environment();
        assert!(!live.is_empty());
        assert!(live.iter().all(|(n, _)| !n.is_empty()));
        assert!(live.iter().any(|(n, _)| n.eq_ignore_ascii_case("SystemRoot") || n.eq_ignore_ascii_case("Path")));
    }
}

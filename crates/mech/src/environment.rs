//! The environment block a launched target inherits, with the session's own changes made to it.
//!
//! `CreateProcessW` with a null `lpEnvironment` hands the child a copy of this process's environment,
//! and that is what a session does when it has nothing to change. The embedded-engine channel (docs/09)
//! puts two variables in front of the target, and the session zone takes a `TZ` of the tester's own out
//! of it. Once a caller supplies a block, the system stops doing three things for it, and this module
//! does them instead:
//!
//! - the drive-directory entries (`=C:=C:\...`) are not propagated - so the block is built from
//!   `GetEnvironmentStringsW`, which carries them, never from `std::env`, which may not,
//! - the block must be sorted by name, case-insensitively in Unicode order,
//! - a Unicode block ends with four zero bytes.
//!
//! All three are documented for `CreateProcessW` and "Changing Environment Variables" on Microsoft
//! Learn. The entries travel as the UTF-16 units the system holds them in (R4-N17): Windows does not
//! require a variable to be valid UTF-16, and a value with an unpaired surrogate used to reach the
//! target with that unit replaced, which no session without the block ever did. The merge and the
//! encoding are pure functions over those entries, so they are tested without launching anything - and
//! a live launch with a block is what `launch_plain` proves.

use windows::Win32::System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW};

/// `=` as a UTF-16 unit. It is never half of a surrogate pair, so splitting at it by unit is safe.
const EQUALS: u16 = 0x3D;

/// One change a launch makes to the environment the target inherits from this process. Names compare
/// without regard to case, as the system compares them, and the changes apply in the order given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvChange {
    /// The variable gets this value: a name already present is replaced, a new one is added.
    Set(String, String),
    /// The variable is not handed on, every entry of that name in whatever case it is written.
    Remove(String),
}

impl EnvChange {
    /// Each `name=value` pair as a variable set, in the order given.
    pub fn set_each(variables: &[(String, String)]) -> Vec<EnvChange> {
        variables.iter().map(|(name, value)| EnvChange::Set(name.clone(), value.clone())).collect()
    }
}

/// Build the Unicode environment block for a child: this process's environment with `changes` made to
/// it, sorted as the system requires, doubly terminated.
pub fn environment_block(changes: &[EnvChange]) -> Vec<u16> {
    encode_block(&merge_entries(raw_environment(), changes))
}

/// This process's environment as text, drive-directory entries included - for READING a variable, as
/// the embedded-engine channel reads its two. A name is everything before the first `=` past the
/// first character, so an entry like `=C:=C:\work` keeps its leading `=` as part of the name, exactly
/// as the system stores it. A unit that is not valid UTF-16 reads as U+FFFD here, which is why the
/// block a child gets is never built from this view.
pub fn current_environment() -> Vec<(String, String)> {
    raw_environment().iter().map(|entry| split_entry(&String::from_utf16_lossy(entry))).collect()
}

/// This process's environment exactly as the system holds it: one `name=value` entry each, in UTF-16
/// units, without its terminating zero.
fn raw_environment() -> Vec<Vec<u16>> {
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
            entries.push(std::slice::from_raw_parts(cursor, len).to_vec());
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

/// The name of a raw entry: its units before the first `=` past the first unit, the same split as
/// `split_entry`.
fn name_of(entry: &[u16]) -> &[u16] {
    match entry.iter().skip(1).position(|&unit| unit == EQUALS) {
        Some(at) => &entry[..=at],
        None => entry,
    }
}

/// The current entries with `changes` made to them, in order: a name set that is already present is
/// replaced in place (names compare without regard to case, as the system compares them), a new one is
/// appended, and a name removed leaves with every entry that carries it. Then the whole list is sorted
/// the way the system requires, so the caller never has to know that a block is sorted at all. Entries
/// no change names are kept unit for unit.
pub(crate) fn merge_entries(mut entries: Vec<Vec<u16>>, changes: &[EnvChange]) -> Vec<Vec<u16>> {
    let key_of = |name: &str| sort_key(&name.encode_utf16().collect::<Vec<u16>>());
    for change in changes {
        match change {
            EnvChange::Set(name, value) => {
                let wanted = key_of(name);
                let entry: Vec<u16> = format!("{name}={value}").encode_utf16().collect();
                match entries.iter_mut().find(|present| sort_key(name_of(present)) == wanted) {
                    Some(slot) => *slot = entry,
                    None => entries.push(entry),
                }
            }
            EnvChange::Remove(name) => {
                let wanted = key_of(name);
                entries.retain(|present| sort_key(name_of(present)) != wanted);
            }
        }
    }
    entries.sort_by_key(|entry| sort_key(name_of(entry)));
    entries
}

/// The order the system sorts an environment block in: by name, case-insensitively, in Unicode
/// order without regard to locale. Upper-casing per character is that comparison for every script
/// with a simple case mapping, and the names in a Windows environment are ASCII in practice. A unit
/// that is half of no pair sorts as itself.
fn sort_key(name: &[u16]) -> Vec<u32> {
    char::decode_utf16(name.iter().copied())
        .flat_map(|decoded| match decoded {
            Ok(c) => c.to_uppercase().map(u32::from).collect::<Vec<u32>>(),
            Err(unpaired) => vec![u32::from(unpaired.unpaired_surrogate())],
        })
        .collect()
}

/// The block as `CreateProcessW` reads it with `CREATE_UNICODE_ENVIRONMENT`: each `name=value`
/// zero-terminated, then one more zero after the last - four zero bytes at the end, as documented.
/// An empty list still yields a valid (empty) block of one terminating zero pair, because a block
/// that ends in two zero bytes is the one shape the caller can hand over unconditionally.
pub(crate) fn encode_block(entries: &[Vec<u16>]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for entry in entries {
        out.extend_from_slice(entry);
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

    fn set(n: &str, v: &str) -> EnvChange {
        EnvChange::Set(n.to_string(), v.to_string())
    }

    fn remove(n: &str) -> EnvChange {
        EnvChange::Remove(n.to_string())
    }

    fn entry(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn texts(entries: &[Vec<u16>]) -> Vec<String> {
        entries.iter().map(|e| String::from_utf16_lossy(e)).collect()
    }

    #[test]
    fn a_present_name_is_replaced_without_regard_to_case_and_a_new_one_is_appended() {
        let merged = merge_entries(
            vec![entry("Path=C:\\bin"), entry("TEMP=C:\\t")],
            &[set("path", "D:\\bin"), set("CHRONO_X", "1")],
        );
        assert_eq!(texts(&merged), ["CHRONO_X=1", "path=D:\\bin", "TEMP=C:\\t"]);
    }

    /// A name removed leaves with every entry that carries it, in whatever case each is written - a block
    /// handed on by a parent may hold one name twice. The changes apply in order, so a name set after it
    /// was removed is there, and one removed after it was set is not. A name nobody holds changes nothing.
    #[test]
    fn a_removed_name_leaves_in_every_case_it_is_written_and_the_changes_apply_in_order() {
        let present = || vec![entry("TZ=EET-2"), entry("Path=C:\\bin"), entry("tz=UTC")];
        assert_eq!(texts(&merge_entries(present(), &[remove("Tz")])), ["Path=C:\\bin"]);
        assert_eq!(texts(&merge_entries(present(), &[remove("TZ"), set("TZ", "CMS-5:30")])), ["Path=C:\\bin", "TZ=CMS-5:30"]);
        assert_eq!(texts(&merge_entries(present(), &[set("TZ", "CMS-5:30"), remove("tz")])), ["Path=C:\\bin"]);
        assert_eq!(texts(&merge_entries(present(), &[remove("NOBODY")])), ["Path=C:\\bin", "TZ=EET-2", "tz=UTC"]);
    }

    #[test]
    fn the_block_is_sorted_by_name_case_insensitively_with_drive_entries_first() {
        // `=` sorts below every letter, which is why the system keeps the drive-directory entries at
        // the front - and this is the order the system requires, not a preference.
        let merged =
            merge_entries(vec![entry("zeta=1"), entry("Alpha=2"), entry("=C:=C:\\work"), entry("beta=3")], &[]);
        assert_eq!(texts(&merged), ["=C:=C:\\work", "Alpha=2", "beta=3", "zeta=1"]);
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
        // The raw split agrees, a name made of a surrogate pair included.
        assert_eq!(name_of(&entry("=C:=C:\\work")), entry("=C:").as_slice());
        assert_eq!(name_of(&entry("\u{1F600}=v")), entry("\u{1F600}").as_slice());
        assert_eq!(name_of(&entry("NOEQUALS")), entry("NOEQUALS").as_slice());
    }

    /// A value Windows holds with an unpaired surrogate reaches the child unit for unit (R4-N17), and so
    /// does a name with one. Read as text first, both came out with U+FFFD in its place.
    #[test]
    fn an_entry_that_is_not_valid_utf16_reaches_the_child_unit_for_unit() {
        let odd_value: Vec<u16> = vec![u16::from(b'A'), EQUALS, u16::from(b'x'), 0xD800, u16::from(b'y')];
        let odd_name: Vec<u16> = vec![u16::from(b'B'), 0xDC00, EQUALS, u16::from(b'1')];
        let merged = merge_entries(vec![odd_name.clone(), odd_value.clone()], &[set("CHRONO_X", "1")]);
        assert!(merged.contains(&odd_value) && merged.contains(&odd_name), "{merged:?}");
        let block = encode_block(&merged);
        assert!(block.windows(odd_value.len()).any(|w| w == odd_value.as_slice()), "{block:?}");
        assert!(!block.contains(&0xFFFD), "a unit was replaced on the way: {block:?}");
    }

    /// The whole road, from the live environment: a variable this process holds with an unpaired
    /// surrogate is in the block a child gets, unit for unit (R4-N17).
    #[test]
    fn a_live_variable_that_is_not_valid_utf16_reaches_the_block_unit_for_unit() {
        use windows::core::PCWSTR;
        use windows::Win32::System::Environment::SetEnvironmentVariableW;
        let name: Vec<u16> = "CHRONO_N17_PROBE".encode_utf16().chain([0]).collect();
        let value: Vec<u16> = vec![u16::from(b'x'), 0xD800, u16::from(b'y'), 0];
        // SAFETY: both strings are zero-terminated and outlive the call. The name is this test's own.
        unsafe { SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR(value.as_ptr())) }.expect("the variable is set");
        let block = environment_block(&[]);
        let wanted: Vec<u16> = "CHRONO_N17_PROBE=x".encode_utf16().chain([0xD800, u16::from(b'y'), 0]).collect();
        assert!(block.windows(wanted.len()).any(|w| w == wanted.as_slice()), "the variable did not reach the block unit for unit");
    }

    #[test]
    fn the_encoded_block_ends_in_four_zero_bytes_and_separates_entries_with_one() {
        let block = encode_block(&[entry("A=1"), entry("B=")]);
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

    /// The random half: any environment and any variables of a session's, merged and encoded, against
    /// a model of the block `CreateProcessW` is owed (R4-N17).
    mod properties {
        use super::*;
        use proptest::prelude::*;
        use proptest::sample::select;
        use std::collections::BTreeMap;

        /// The drive-directory entries and the exit code the system keeps at the front of a block.
        const DRIVE_NAMES: [&str; 4] = ["=C:", "=D:", "=::", "=ExitCode"];
        /// Units of a value: text of every width, the separators a value holds, and halves of a pair
        /// standing alone at both ends of each range - Windows keeps them, so a block must too.
        const VALUE_PIECES: [&[u16]; 9] =
            [&[0x43, 0x3A, 0x5C, 0x62], &[0x3D], &[0x3B], &[0x20], &[0x0105], &[0xD83D, 0xDE00], &[0xD800], &[0xDBFF], &[0xDC00]];

        /// A variable name: letters in either case, digits and the marks names hold (`ProgramFiles(x86)`).
        fn name() -> impl Strategy<Value = String> {
            "[A-Za-z_][A-Za-z0-9_().-]{0,8}"
        }

        fn value() -> impl Strategy<Value = Vec<u16>> {
            prop_oneof![
                3 => proptest::collection::vec(select(VALUE_PIECES.to_vec()), 0..5).prop_map(|pieces| pieces.concat()),
                1 => Just(vec![0xDFFF]),
            ]
        }

        /// The name in every case at once, which is how the system tells two names apart.
        fn folded(name: &str) -> String {
            name.to_ascii_uppercase()
        }

        /// The same name written in another mix of cases.
        fn recased(name: &str, mask: u64) -> String {
            name.chars()
                .enumerate()
                .map(|(i, c)| if mask >> (i % 64) & 1 == 1 { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() })
                .collect()
        }

        /// An environment: names that differ in more than case, drive entries among them, any values.
        fn environment() -> impl Strategy<Value = Vec<(String, Vec<u16>)>> {
            let drives = proptest::sample::subsequence(DRIVE_NAMES.to_vec(), 0..=DRIVE_NAMES.len());
            (drives, proptest::collection::vec((name(), value()), 0..8), proptest::collection::vec(value(), 4)).prop_map(
                |(drives, named, drive_values)| {
                    let mut seen = std::collections::HashSet::new();
                    let drives = drives.into_iter().zip(drive_values).map(|(n, v)| (n.to_string(), v));
                    drives.chain(named).filter(|(n, _)| seen.insert(folded(n))).collect()
                },
            )
        }

        /// A session's changes: names of the environment's in another case and new ones, each set or
        /// removed, and one name changed twice - the second time set or removed as the mask says.
        fn extra(environment: Vec<(String, Vec<u16>)>) -> impl Strategy<Value = Vec<EnvChange>> {
            let names: Vec<String> = environment.into_iter().map(|(n, _)| n).filter(|n| !n.starts_with('=')).collect();
            let present = if names.is_empty() { name().boxed() } else { select(names).boxed() };
            let one = prop_oneof![2 => (present, any::<u64>()).prop_map(|(n, mask)| recased(&n, mask)), 2 => name()].boxed();
            let change = prop_oneof![
                3 => (one.clone(), "[a-z=;\\\\ ]{0,6}|.{0,4}").prop_map(|(n, v)| EnvChange::Set(n, v)),
                1 => one.prop_map(EnvChange::Remove),
            ];
            (proptest::collection::vec(change, 0..4), any::<u64>()).prop_map(|(mut extra, mask)| {
                let first = extra.first().map(|change| match change {
                    EnvChange::Set(n, _) | EnvChange::Remove(n) => recased(n, mask),
                });
                if let Some(again) = first {
                    extra.push(if mask & 1 == 1 { EnvChange::Remove(again) } else { EnvChange::Set(again, "twice".into()) });
                }
                extra
            })
        }

        fn units(text: &str) -> Vec<u16> {
            text.encode_utf16().collect()
        }

        proptest! {
            /// Every entry the session does not name reaches the block unit for unit, every variable the
            /// session set last stands in it once with the value it was given last, every one it removed
            /// last is gone, nothing else is there, the entries are sorted by name without regard to case,
            /// and the block ends as the system reads it.
            #[test]
            fn the_block_holds_the_environment_and_the_sessions_variables_and_nothing_else(
                (environment, extra) in environment().prop_flat_map(|env| (Just(env.clone()), extra(env))),
            ) {
                let entries: Vec<Vec<u16>> =
                    environment.iter().map(|(n, v)| [units(n), vec![0x3D], v.clone()].concat()).collect();
                let block = encode_block(&merge_entries(entries, &extra));

                let mut wanted: BTreeMap<String, Vec<u16>> = environment
                    .iter()
                    .map(|(n, v)| (folded(n), [units(n), vec![0x3D], v.clone()].concat()))
                    .collect();
                for change in &extra {
                    match change {
                        EnvChange::Set(n, v) => wanted.insert(folded(n), units(&format!("{n}={v}"))),
                        EnvChange::Remove(n) => wanted.remove(&folded(n)),
                    };
                }
                let owed: Vec<u16> = wanted.into_values().flat_map(|entry| entry.into_iter().chain([0])).chain([0]).collect();
                prop_assert_eq!(&block, &owed, "the block of {:?} with {:?}", environment, extra);
            }
        }
    }
}

//! A program that ends at once ends its session, unless a windowed program was in its family (ADR-4,
//! owner decision 2026-10-27).
//!
//! The guard window after the start reads an exit inside it as the second instance of a single-instance
//! application handing its work to the first. Until 2026-10-27 it read every such exit that way, so a
//! console tool that did its work in a few milliseconds - a date printed, a commit made, a file written,
//! each measured with the session's date in it - was reported as a session that never took effect, with
//! exit code 12. The suspicion now needs a windowed program in the family, which is what a hand-off is,
//! and the hook says which kind each process is (`Cov.subsystem`).
//!
//! The system targets come with every Windows installation, so this runs on a clean runner: the command
//! interpreter as the console program, and `rundll32.exe` with nothing to run as the windowed one, which
//! ends at once without showing anything (measured: 1 to 3 ms after the start, both bitnesses). The
//! 32-bit interpreter of a 64-bit system is the child the hook cannot enter. The targets that are not system
//! programs are written here, a few bytes of console program each: one whose PE header lies past the first
//! page, the same with its header in the first page as the control, and one that ends by returning
//! from its entry point.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// Held by every test here that starts the core. The core allows one session at a time, and the
/// tests of one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// A program from the system folder, by full path, because the target is never searched for on PATH.
fn system_program(name: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\{name}")
}

/// One session over `program`, with the protocol on stdout: its exit code, stdout and stderr. Every
/// program here ends within milliseconds, so the session is given a minute: a session that does not end
/// fails the test with exit code 6 instead of holding the run.
fn session(program: &str, args: Option<&str>) -> (Option<i32>, String, String) {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this test drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    let _session = one_real_session_at_a_time();
    let mut command = Command::new(env!("CARGO_BIN_EXE_chrono"));
    command.args(["run", program, "--at", "2091-06-15T12:00:00", "--zone", "+00:00", "--timeout", "60", "--json"]);
    if let Some(args) = args {
        command.args(["--args", args]);
    }
    let out = command.output().expect("the tool must run");
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The command interpreter told to end at once is a console program that does its work inside the guard
/// window, and its session ends the way any other does: a verdict for the family, no vanish, exit 0.
#[test]
fn a_console_program_that_ends_at_once_ends_its_session_instead_of_vanishing() {
    let (code, stdout, stderr) = session(&system_program("cmd.exe"), Some("/c exit 0"));
    let context = format!("exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("\"vanished\""), "a console program that ended was called a vanish. {context}");
    assert!(stdout.contains("\"session_verdict\""), "the session ended without a family verdict. {context}");
    assert_eq!(code, Some(0), "the interpreter read the session's clock, so the session works. {context}");
}

/// The guard window asks the hook's ring which children ran uncovered, and the ring names each child
/// once. A console family that ended inside the window goes on to its report, so the children the window
/// named have to reach it: the 32-bit interpreter a 64-bit one starts is such a child, which the hook
/// cannot enter, and both end at once. The report names as many children as it counts.
#[test]
#[cfg(target_pointer_width = "64")]
fn a_console_family_that_ends_at_once_names_the_children_it_ran_uncovered() {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let other_bitness = format!(r"{root}\SysWOW64\cmd.exe /c exit 0");
    let (code, stdout, stderr) = session(&system_program("cmd.exe"), Some(&format!("/c {other_bitness}")));
    let context = format!("exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("\"vanished\""), "a console family that ended was called a vanish. {context}");
    let verdict = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|event| event["type"] == "session_verdict")
        .unwrap_or_else(|| panic!("the session gave no verdict. {context}"));
    let total = verdict["uncovered_children_total"].as_u64().unwrap_or(0);
    let named = verdict["uncovered_children"].as_array().map_or(0, Vec::len) as u64;
    assert!(total >= 1, "the 32-bit child was not counted as uncovered. {context}");
    assert_eq!(named, total, "the report counts children it does not name. {context}");
}

/// The exit code the program written by `write_console_program` ends with, either way it ends.
const WRITTEN_PROGRAM_EXIT: u8 = 7;

/// How the program written by `write_console_program` ends.
#[derive(Clone, Copy)]
enum Ending {
    /// Through `ExitProcess`, as the C runtime of any program ends it.
    ExitProcess,
    /// By returning from its entry point, which ends its only thread, and Windows ends a process when its
    /// last thread ends. A program without a C runtime can end so.
    Return,
}

/// Writes a console program of this test's bitness to `path`, with its PE header at `pe` and its sections
/// aligned to `section_align`, that ends at once as `ending` says. One section holds the code and the
/// import of `ExitProcess` from KERNEL32.dll, at a fixed base without relocations, laid out the way a
/// linker lays a program out (the PE format, MS Learn).
fn write_console_program(path: &Path, pe: usize, section_align: usize, ending: Ending) {
    const FILE_ALIGN: usize = 0x200;
    // Where each part sits in the section, which starts at the relative address `section_align`.
    const IMPORTS: usize = 0x40;
    const LOOKUP: usize = 0x70;
    const ADDRESSES: usize = 0x80;
    const FUNCTION: usize = 0x90;
    const LIBRARY: usize = 0xA0;
    const IMAGE_BASE_32: usize = 0x40_0000;
    let wide = cfg!(target_pointer_width = "64");
    let optional_len = if wide { 240 } else { 224 };
    let headers_end = pe + 4 + 20 + optional_len + 40;
    let size_of_headers = headers_end.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    assert!(size_of_headers <= section_align, "the headers must fit under the first section");
    let rva = |offset: usize| section_align + offset;
    let mut b = vec![0u8; size_of_headers + FILE_ALIGN];
    let mut put = |at: usize, bytes: &[u8]| b[at..at + bytes.len()].copy_from_slice(bytes);
    let u16le = |v: u16| v.to_le_bytes();
    let u32le = |v: usize| (v as u32).to_le_bytes();
    put(0, b"MZ");
    put(0x3C, &u32le(pe));
    put(pe, b"PE\0\0");
    let file = pe + 4;
    put(file, &u16le(if wide { 0x8664 } else { 0x14C }));
    put(file + 2, &u16le(1));
    put(file + 16, &u16le(optional_len as u16));
    // An executable without relocations, large-address-aware (64-bit) or 32-bit-machine (32-bit).
    put(file + 18, &u16le(if wide { 0x0023 } else { 0x0103 }));
    let o = file + 20;
    put(o, &u16le(if wide { 0x20B } else { 0x10B }));
    put(o + 4, &u32le(FILE_ALIGN));
    put(o + 16, &u32le(section_align));
    put(o + 20, &u32le(section_align));
    if wide {
        put(o + 24, &0x1_4000_0000u64.to_le_bytes());
    } else {
        put(o + 24, &u32le(section_align));
        put(o + 28, &u32le(IMAGE_BASE_32));
    }
    put(o + 32, &u32le(section_align));
    put(o + 36, &u32le(FILE_ALIGN));
    put(o + 40, &u16le(6));
    put(o + 48, &u16le(6));
    put(o + 56, &u32le(2 * section_align));
    put(o + 60, &u32le(size_of_headers));
    put(o + 68, &u16le(3));
    put(o + 70, &u16le(0x8100));
    let (reserve, commit) = (0x10_0000, 0x1000);
    let directories = if wide {
        for (at, v) in [(72, reserve), (80, commit), (88, reserve), (96, commit)] {
            put(o + at, &(v as u64).to_le_bytes());
        }
        o + 112
    } else {
        for (at, v) in [(72, reserve), (76, commit), (80, reserve), (84, commit)] {
            put(o + at, &u32le(v));
        }
        o + 96
    };
    put(directories - 4, &u32le(16));
    // The import directory: one library and the empty entry that ends the list.
    put(directories + 8, &u32le(rva(IMPORTS)));
    put(directories + 12, &u32le(40));
    let section = o + optional_len;
    put(section, b".text");
    put(section + 8, &u32le(0x100));
    put(section + 12, &u32le(section_align));
    put(section + 16, &u32le(FILE_ALIGN));
    put(section + 20, &u32le(size_of_headers));
    // Code, read, write and execute: the loader writes the address of `ExitProcess` into the section.
    put(section + 36, &0xE000_0060u32.to_le_bytes());
    let raw = size_of_headers;
    if let Ending::Return = ending {
        // mov eax, code / ret, and on 32-bit `ret 4` for the one stack argument of a thread's start.
        put(raw, &[0xB8]);
        put(raw + 1, &u32::from(WRITTEN_PROGRAM_EXIT).to_le_bytes());
        put(raw + 5, if wide { &[0xC3][..] } else { &[0xC2, 0x04, 0x00][..] });
    } else if wide {
        // sub rsp, 0x28 / mov ecx, code / call [rip + to the address slot] / int3
        let after_call = rva(15);
        let to_slot = (rva(ADDRESSES) - after_call) as u32;
        put(raw, &[0x48, 0x83, 0xEC, 0x28, 0xB9]);
        put(raw + 5, &u32::from(WRITTEN_PROGRAM_EXIT).to_le_bytes());
        put(raw + 9, &[0xFF, 0x15]);
        put(raw + 11, &to_slot.to_le_bytes());
        put(raw + 15, &[0xCC]);
    } else {
        // push code / call [the address slot] / int3
        put(raw, &[0x6A, WRITTEN_PROGRAM_EXIT, 0xFF, 0x15]);
        put(raw + 4, &u32le(IMAGE_BASE_32 + rva(ADDRESSES)));
        put(raw + 8, &[0xCC]);
    }
    put(raw + IMPORTS, &u32le(rva(LOOKUP)));
    put(raw + IMPORTS + 12, &u32le(rva(LIBRARY)));
    put(raw + IMPORTS + 16, &u32le(rva(ADDRESSES)));
    // The lookup and address tables each name the function by its hint and name, then end.
    put(raw + LOOKUP, &u32le(rva(FUNCTION)));
    put(raw + ADDRESSES, &u32le(rva(FUNCTION)));
    put(raw + FUNCTION + 2, b"ExitProcess\0");
    put(raw + LIBRARY, b"KERNEL32.dll\0");
    std::fs::write(path, b).expect("the scratch folder takes the program");
}

/// A console program whose PE header lies past the first page of its image is a console program all the
/// same, and the hook reads its subsystem wherever the header is, so its session ends the way any other
/// does. The same program with its header in the first page is the control: a refusal or a crash there
/// means the bytes written here are wrong, not the reading.
#[test]
fn a_console_program_with_its_pe_header_past_the_first_page_ends_its_session() {
    let dir = std::env::temp_dir().join(format!("chrono-quick-exit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch folder can be made");
    let cases = [("header-in-first-page", 0x80, 0x1000), ("header-past-first-page", 0x1100, 0x2000)];
    let mut results = Vec::new();
    for (name, pe, align) in cases {
        let program = dir.join(format!("{name}.exe"));
        write_console_program(&program, pe, align, Ending::ExitProcess);
        let (code, stdout, stderr) = session(&program.to_string_lossy(), None);
        let own_code = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|event| event["type"] == "ended")
            .and_then(|ended| ended["target_exit_code"].as_i64());
        results.push((name, code, own_code, format!("{name}: exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}")));
    }
    let _ = std::fs::remove_dir_all(&dir);
    for (name, code, own_code, context) in results {
        assert_eq!(own_code, Some(i64::from(WRITTEN_PROGRAM_EXIT)), "{name} did not end with its own code. {context}");
        assert!(!context.contains("target.single_instance_suspected"), "{name} was suspected of handing off. {context}");
        assert_eq!(code, Some(0), "{name} ran under the session, which works. {context}");
    }
}

/// A program that ends by returning from its entry point ends its only thread, and Windows ends a process
/// with its last thread, with that thread's code. The hook's watcher is a thread of the process too, and it
/// waits on the core for as long as the session runs, so it lets the program go once the program's own
/// threads are gone: the session ends with the program's code, the same as without a session.
#[test]
fn a_program_that_returns_from_its_entry_point_ends_its_session() {
    let dir = std::env::temp_dir().join(format!("chrono-quick-exit-return-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch folder can be made");
    let program = dir.join("returns.exe");
    write_console_program(&program, 0x80, 0x1000, Ending::Return);
    let (code, stdout, stderr) = session(&program.to_string_lossy(), None);
    let _ = std::fs::remove_dir_all(&dir);
    let context = format!("exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    let own_code = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|event| event["type"] == "ended")
        .and_then(|ended| ended["target_exit_code"].as_i64());
    assert_ne!(code, Some(6), "the program ended, yet its session ran until the timeout. {context}");
    assert_eq!(own_code, Some(i64::from(WRITTEN_PROGRAM_EXIT)), "the program did not end with its own code. {context}");
    assert_eq!(code, Some(0), "the program ran under the session, which works. {context}");
}

/// A windowed program that ends at once is what the second instance of a single-instance application
/// looks like from outside, and the suspicion stays: exit 12, named.
#[test]
fn a_windowed_program_that_ends_at_once_is_still_a_suspected_hand_off() {
    let (code, stdout, stderr) = session(&system_program("rundll32.exe"), None);
    let context = format!("exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("target.single_instance_suspected"),
        "a windowed program that ended at once was not suspected of handing off. {context}"
    );
    assert_eq!(code, Some(12), "{context}");
}

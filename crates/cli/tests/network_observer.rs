//! Every way an application connects to a network has to reach the connection observer.
//!
//! The audit warns `source.network_at_start` when an application opens a connection, because it may
//! then take the time from a server, which no local substitution reaches. Until 2026-09-23 the
//! observer sat on ws2_32's `connect`, and two of the three ways to connect never call it:
//! `WSAConnect`, and `ConnectEx`, which WinHTTP, WinINet and the .NET, Node.js and Go runtimes connect
//! through. Those applications got no caution and a clean result. The observer now counts at the
//! socket driver, in ntdll, where all three meet.
//!
//! The target is this test binary itself: `probe_connects_three_ways` connects to a loopback listener
//! the test opens, once through each of the three, and does its work only when the variables below
//! are set. The count has to be exactly three - fewer means a path is missed, more means one of them
//! is counted twice.

use std::ffi::c_void;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::{null, null_mut};

/// Where the probe writes how many of its three connections succeeded. Unset, the probe returns at
/// once, so running the ignored tests by hand does nothing.
const PROBE_OUT: &str = "CHRONO_NETWORK_OBSERVER_PROBE_OUT";

/// The loopback port the test listens on.
const PROBE_PORT: &str = "CHRONO_NETWORK_OBSERVER_PROBE_PORT";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_connects_three_ways";

const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
const IPPROTO_TCP: i32 = 6;
const WSA_FLAG_OVERLAPPED: u32 = 1;
const SIO_GET_EXTENSION_FUNCTION_POINTER: u32 = 0xC800_0006;
const WSA_IO_PENDING: i32 = 997;
const INVALID_SOCKET: usize = usize::MAX;

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `WSAID_CONNECTEX` from mswsock.h.
const WSAID_CONNECTEX: Guid =
    Guid { data1: 0x25a2_07b9, data2: 0xddf3, data3: 0x4660, data4: [0x8e, 0xe9, 0x76, 0xe5, 0x8c, 0x74, 0x06, 0x3e] };

#[repr(C)]
struct SockAddrIn {
    family: u16,
    port: u16,
    addr: [u8; 4],
    zero: [u8; 8],
}

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut c_void,
}

type ConnectExFn =
    unsafe extern "system" fn(usize, *const SockAddrIn, i32, *const c_void, u32, *mut u32, *mut Overlapped) -> i32;

#[cfg_attr(target_arch = "x86", link(name = "ws2_32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "ws2_32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn WSAStartup(version: u16, data: *mut u8) -> i32;
    fn WSASocketW(af: i32, kind: i32, protocol: i32, info: *const c_void, group: u32, flags: u32) -> usize;
    fn WSAConnect(
        socket: usize,
        name: *const SockAddrIn,
        name_len: i32,
        caller: *const c_void,
        callee: *mut c_void,
        sqos: *const c_void,
        gqos: *const c_void,
    ) -> i32;
    fn WSAIoctl(
        socket: usize,
        code: u32,
        input: *const c_void,
        input_len: u32,
        output: *mut c_void,
        output_len: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
        completion: *const c_void,
    ) -> i32;
    fn WSAGetOverlappedResult(socket: usize, overlapped: *const Overlapped, bytes: *mut u32, wait: i32, flags: *mut u32) -> i32;
    fn WSAGetLastError() -> i32;
    fn bind(socket: usize, name: *const SockAddrIn, name_len: i32) -> i32;
    fn closesocket(socket: usize) -> i32;
}

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn CreateEventW(attributes: *const c_void, manual: i32, initial: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

fn loopback(port: u16) -> SockAddrIn {
    SockAddrIn { family: AF_INET as u16, port: port.to_be(), addr: [127, 0, 0, 1], zero: [0; 8] }
}

/// A connection through `WSAConnect`, which never calls the `connect` export.
fn through_wsa_connect(port: u16) -> bool {
    let target = loopback(port);
    // SAFETY: a fresh socket, a live address of the documented size, and every optional argument null.
    unsafe {
        let socket = WSASocketW(AF_INET, SOCK_STREAM, IPPROTO_TCP, null(), 0, 0);
        if socket == INVALID_SOCKET {
            return false;
        }
        let result = WSAConnect(socket, &target, 16, null(), null_mut(), null(), null());
        closesocket(socket);
        result == 0
    }
}

/// A connection through `ConnectEx`, the extension function WinHTTP and the .NET, Node.js and Go
/// runtimes connect through. It is not an export: its address comes from `WSAIoctl`, and the socket
/// has to be bound and overlapped first.
fn through_connect_ex(port: u16) -> bool {
    let target = loopback(port);
    let any = SockAddrIn { family: AF_INET as u16, port: 0, addr: [0; 4], zero: [0; 8] };
    // SAFETY: every pointer is to a live local of the documented layout, the event outlives the wait,
    // and the extension function is called with the signature mswsock.h gives it.
    unsafe {
        let socket = WSASocketW(AF_INET, SOCK_STREAM, IPPROTO_TCP, null(), 0, WSA_FLAG_OVERLAPPED);
        if socket == INVALID_SOCKET {
            return false;
        }
        let mut function: usize = 0;
        let mut returned: u32 = 0;
        let resolved = bind(socket, &any, 16) == 0
            && WSAIoctl(
                socket,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                &WSAID_CONNECTEX as *const Guid as *const c_void,
                size_of::<Guid>() as u32,
                &mut function as *mut usize as *mut c_void,
                size_of::<usize>() as u32,
                &mut returned,
                null_mut(),
                null(),
            ) == 0
            && function != 0;
        if !resolved {
            closesocket(socket);
            return false;
        }
        let connect_ex: ConnectExFn = std::mem::transmute::<usize, ConnectExFn>(function);
        let event = CreateEventW(null(), 1, 0, null());
        let mut overlapped = Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event };
        let started = connect_ex(socket, &target, 16, null(), 0, null_mut(), &mut overlapped) != 0
            || WSAGetLastError() == WSA_IO_PENDING;
        if started {
            WaitForSingleObject(event, 5000);
        }
        let (mut bytes, mut flags) = (0u32, 0u32);
        let connected = started && WSAGetOverlappedResult(socket, &overlapped, &mut bytes, 0, &mut flags) != 0;
        CloseHandle(event);
        closesocket(socket);
        connected
    }
}

/// Connects to the test's listener three ways and writes how many connected.
#[test]
#[ignore = "the target of `every_way_to_connect_reaches_the_connection_observer`, not a test on its own"]
fn probe_connects_three_ways() {
    let Some(out) = std::env::var_os(PROBE_OUT) else {
        return;
    };
    let port: u16 = std::env::var(PROBE_PORT).ok().and_then(|p| p.parse().ok()).expect("the test passes its port");
    let mut data = [0u8; 512];
    // SAFETY: a buffer larger than WSADATA on either bitness. The standard library starts Winsock the
    // same way, and the count is per process, so a second start is harmless.
    unsafe { WSAStartup(0x0202, data.as_mut_ptr()) };
    let connected = [
        // The standard library connects through the `connect` export.
        TcpStream::connect(("127.0.0.1", port)).is_ok(),
        through_wsa_connect(port),
        through_connect_ex(port),
    ];
    std::fs::write(&out, connected.iter().filter(|c| **c).count().to_string()).expect("the probe writes its result");
    // Alive past the session's opening guard window (ADR-4). The session here scales nothing, so a
    // plain sleep is real.
    std::thread::sleep(std::time::Duration::from_millis(600));
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// The highest `connect` count and whether `source.network_at_start` was raised, over every coverage
/// event on the session's stdout. The highest, because the session reports each process twice, once
/// after the opening guard window and once at the end.
fn observed(stdout: &str) -> (Option<u64>, bool) {
    let mut calls = None;
    let mut warned = false;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event["type"] != "coverage" {
            continue;
        }
        let entries = event["observed"].as_array().into_iter().flatten();
        for entry in entries.filter(|e| e["channel"] == "connect") {
            calls = calls.max(entry["calls"].as_u64());
        }
        warned |= event["warning_keys"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|k| k == "source.network_at_start"));
    }
    (calls, warned)
}

/// Three connections, one through each API, are counted as three, and the audit raises the network
/// caution for them.
#[test]
fn every_way_to_connect_reaches_the_connection_observer() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    // Nothing accepts: the system completes a loopback connection into the backlog on its own, and the
    // listener is closed when the test ends.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
    let port = listener.local_addr().expect("the listener has an address").port().to_string();
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = std::env::temp_dir().join(format!("chrono-network-observer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let probe_args = ["--ignored", "--exact", PROBE, "--test-threads", "1"];

    // The control first: the probe with no session must make all three connections. Without it a
    // count of three under the session could not be told from three paths that simply failed.
    let control = dir.join("control.txt");
    let run = Command::new(&me)
        .args(probe_args)
        .env(PROBE_OUT, &control)
        .env(PROBE_PORT, &port)
        .output()
        .expect("the probe must run without a session");
    assert!(run.status.success(), "the probe failed without a session: {}", String::from_utf8_lossy(&run.stdout));
    let made = std::fs::read_to_string(&control).unwrap_or_default();
    assert_eq!(made, "3", "without a session the probe made {made:?} of its three connections, so this test cannot tell");

    let session = dir.join("session.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &me.display().to_string(), "--json", "--args", &probe_args.join(" ")])
        .env(PROBE_OUT, &session)
        .env(PROBE_PORT, &port)
        .output()
        .expect("the tool must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let made = std::fs::read_to_string(&session).unwrap_or_default();
    let (calls, warned) = observed(&stdout);
    assert_eq!(made, "3", "under the session the probe made {made:?} of its three connections. stdout: {stdout}");
    assert_eq!(
        calls,
        Some(3),
        "three connections - through connect, WSAConnect and ConnectEx - were counted as {calls:?}. \
         Fewer means a way to connect bypasses the observer, more means one is counted twice. \
         stdout: {stdout}"
    );
    assert!(warned, "the connections were counted but source.network_at_start was not raised. stdout: {stdout}");
    drop(listener);
    let _ = std::fs::remove_dir_all(&dir);
}

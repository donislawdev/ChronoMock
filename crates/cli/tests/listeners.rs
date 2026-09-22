//! The mechanism's read of the local TCP table names this process and the port it bound.
//!
//! Here rather than beside the code, because a listener has to be bound to be found, and the
//! mechanism crate is one the network guard keeps silent (crates/cli/tests/network.rs): a socket
//! there, even in a test, would be the product reaching out. This file is registered in that guard
//! for exactly this bind - a loopback listener on a port the system picks, closed before the test
//! ends, with nothing ever connecting to it.

use std::net::TcpListener;

#[test]
fn a_bound_loopback_listener_is_in_the_table_under_this_pid_and_gone_once_dropped() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port is free");
    let port = listener.local_addr().expect("the bound address is readable").port();
    let me = std::process::id();

    let while_bound = chrono_mech::listening_sockets().expect("the table is readable");
    let ours: Vec<_> = while_bound.iter().filter(|l| l.pid == me && l.port == port).collect();
    assert_eq!(ours.len(), 1, "the table lists this process's listener once: {ours:?}");
    assert!(ours[0].loopback, "a 127.0.0.1 listener is a loopback listener");

    drop(listener);
    let after = chrono_mech::listening_sockets().expect("the table is readable");
    assert!(
        !after.iter().any(|l| l.pid == me && l.port == port),
        "the listener is still in the table after it was closed"
    );
}

#[test]
fn a_listener_on_every_address_is_not_a_loopback_listener() {
    // The session only ever speaks to a loopback endpoint. An engine bound to every interface would
    // be reachable from off the machine, and the table read says so rather than treating it as ours.
    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("a port on every address is free");
    let port = listener.local_addr().expect("the bound address is readable").port();
    let me = std::process::id();

    let table = chrono_mech::listening_sockets().expect("the table is readable");
    let ours = table
        .iter()
        .find(|l| l.pid == me && l.port == port)
        .expect("the table lists this process's listener");
    assert!(!ours.loopback);
}

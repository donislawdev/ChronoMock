//! The wire as the window reads it (R5-D64): one line per event the core can send, with every field present.
//!
//! `tests/wire/events.ndjson` is written from `every_event` below and nowhere else. The window's own test reads
//! it with unknown members refused, so a field or an event added here and never taught to the window turns
//! that test red, rather than being dropped by the window's parser without a word (docs/08 section 2 lets a
//! reader ignore what it does not know, which is exactly what makes a forgotten field invisible).

use chrono_proto::{
    Clock, CoveredChannel, Event, FollowedProcess, PROTOCOL_VERSION, ReachedEngine, UncoveredChild,
};
use std::path::{Path, PathBuf};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/wire/events.ndjson")
}

/// One of each event, every field set to a value its serializer writes out. A field left at its default is
/// skipped by serde (`skip_serializing_if`) and would be missing from the line - so no None, no empty list, no
/// false and no zero in the body below, which the test checks. Struct literals, so a field added to an event
/// does not compile here until it is given a value.
fn every_event() -> Vec<Event> {
    let v = PROTOCOL_VERSION;
    let engine = ReachedEngine { pid: 4400, port: 9333, browser: "Engine/1.0".into(), context_count: 2 };
    let child = UncoveredChild {
        pid: 4300,
        parent_pid: 4200,
        image: Some("helper.exe".into()),
        role: Some("renderer".into()),
    };
    let followed = FollowedProcess { pid: 4500, image: Some("app.exe".into()) };
    let read = |channel: &str| CoveredChannel { channel: channel.into(), calls: 12, capped: true };
    vec![
        Event::Ready {
            v,
            protocol: PROTOCOL_VERSION,
            core_version: "0.4.0".into(),
            bitness: "x64".into(),
            capabilities: vec!["jump".into()],
        },
        Event::Coverage {
            v,
            pid: 4200,
            kind: "process".into(),
            covered: vec![read("GetSystemTimePreciseAsFileTime")],
            observed: vec![read("QueryPerformanceCounter")],
            uncovered: vec!["NtQuerySystemTime".into()],
            unobserved: vec!["timeGetTime".into()],
            installed_late: vec!["GetSystemTimePreciseAsFileTime".into()],
            warning_keys: vec!["wait.timeout_collapsed".into()],
        },
        Event::Verdict {
            v,
            id: Some(1),
            verdict: "fails".into(),
            refuse_start: true,
            reason_key: "coverage.time_channels_uncovered".into(),
            left_running: vec![followed.clone()],
            family_search_incomplete: true,
        },
        Event::Ack { v, id: 11 },
        Event::State {
            v,
            fake: Clock { wall: "2038-01-19T03:14:08".into(), zone_bias_min: -60 },
            real: Clock { wall: "2026-10-20T10:00:01".into(), zone_bias_min: -120 },
            multiplier: 60,
            elapsed_fake_ms: 60_000,
            elapsed_real_ms: 1000,
        },
        Event::Vanished { v, pid: 4200, reason_key: "target.single_instance_suspected".into(), lived_ms: 40 },
        Event::SessionVerdict {
            v,
            verdict: "partial".into(),
            reason_key: "session.family_partial_children".into(),
            process_count: 3,
            warning_keys: vec!["time.host_slept".into()],
            uncovered_children: vec![child],
            uncovered_children_total: 2,
            context_count: 2,
            engines: vec![engine],
            followed: vec![followed],
            copies_outside: vec![FollowedProcess { pid: 4600, image: Some("app.exe".into()) }],
            host_slept_ms: 7_200_000,
            browser: Some("Engine/1.0".into()),
        },
        Event::HostSlept { v, slept_ms: 3_600_000, total_slept_ms: 7_200_000 },
        Event::Ended {
            v,
            clean: true,
            residue_keys: vec!["cdp.profile_left".into()],
            target_exit_code: Some(3),
            elapsed_real_ms: 2000,
            elapsed_fake_ms: 120_000,
            fake_end_wall: Some("2038-01-19T03:16:07".into()),
        },
        Event::Error { v, id: Some(12), code: 2, key: "time.bad_multiplier".into(), origin: "core".into() },
    ]
}

/// The event's place among the kinds, without a catch-all arm: a new event does not compile here until it is
/// counted, and the test below then requires it in `every_event`.
fn kind(event: &Event) -> usize {
    match event {
        Event::Ready { .. } => 0,
        Event::Coverage { .. } => 1,
        Event::Verdict { .. } => 2,
        Event::Ack { .. } => 3,
        Event::State { .. } => 4,
        Event::Vanished { .. } => 5,
        Event::SessionVerdict { .. } => 6,
        Event::HostSlept { .. } => 7,
        Event::Ended { .. } => 8,
        Event::Error { .. } => 9,
    }
}

const KINDS: usize = 10;

#[test]
fn the_golden_events_are_one_of_each_kind_with_every_field_written_out() {
    let events = every_event();
    let mut seen = [0usize; KINDS];
    for event in &events {
        seen[kind(event)] += 1;
    }
    assert!(seen.iter().all(|n| *n == 1), "every_event must hold each kind of event exactly once: {seen:?}");

    // A default value is a field serde leaves out, so the window's test would never see it.
    let source = include_str!("wire_golden.rs");
    let start = source.find("fn every_event()").expect("every_event is in this file");
    let body = &source[start..start + source[start..].find("\n}\n").expect("every_event ends")];
    for default in ["None", "vec![]", "Vec::new()", "false", "String::new()", ": 0,", ": 0 }"] {
        assert!(!body.contains(default), "every_event sets a field to {default:?}, which serde may leave out of the line");
    }

    let got: String = events.iter().map(|e| e.to_ndjson() + "\n").collect();
    let path = golden();
    if std::env::var_os("CHRONO_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the golden file has a folder")).expect("the folder is writable");
        std::fs::write(&path, &got).expect("the golden file is writable");
        return;
    }
    let want = std::fs::read_to_string(&path).expect("tests/wire/events.ndjson exists").replace("\r\n", "\n");
    assert!(
        got == want,
        "the events the core sends no longer match tests/wire/events.ndjson - record a deliberate change with \
         CHRONO_BLESS=1, read the diff, and teach the window any field it adds"
    );
}

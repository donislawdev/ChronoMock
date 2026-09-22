//! `__cdp-embedded`: the probe that proves the embedded-engine channel end to end (docs/09).
//!
//! It starts a host the way slice C's native session will - with the two variables that make an
//! embedded engine open a DevTools port - or takes a host already running, then does what the
//! session will do: keeps the family current, lets discovery find the port, attaches to it, shims
//! every page and worker, and reads each page's title once a second. The bench page writes its
//! `Date.now()`, `performance.now()` and tick count to the title, so the printout IS the evidence:
//! the year the session asked for, at the rate it asked for.
//!
//! No hook and no protocol here: this is a probe, like `__cdp-shim` and `__cdp-date`, and it prints
//! text. Exit 0 when at least one context was shimmed, 2 otherwise.

use std::thread;
use std::time::{Duration, Instant};

use crate::cdp;
use crate::cdp_attach::{Attacher, Pumped};
use crate::cdp_discover::{Discovery, Notice};
use crate::embedded::engine_env;
use crate::zone::{moment_epoch_ms, now_epoch_ms};

const USAGE: &str = "usage: chrono __cdp-embedded --at <YYYY-MM-DDTHH:MM:SS> [--multiplier N] [--seconds S] \
                     [--cwd <dir>] (--launch <host.exe> [args...] | --pid <pid>)";

/// How long a turn with nothing to pump waits before the next one. With no attacher yet, nothing
/// in the loop blocks - discovery is polled, not awaited - and a host that takes a minute to open
/// its engine would otherwise cost a core for that minute.
const TURN: Duration = Duration::from_millis(50);

/// The probe's arguments, parsed by hand like the other probes: everything after `--launch <exe>`
/// belongs to the host.
struct Args {
    at: String,
    multiplier: i64,
    seconds: u64,
    cwd: Option<String>,
    host: Host,
}

enum Host {
    Launch { path: String, args: Vec<String> },
    Pid(u32),
}

fn parse(argv: &[String]) -> Option<Args> {
    let mut at = None;
    let mut multiplier = 60;
    let mut seconds = 30;
    let mut cwd = None;
    let mut host = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--at" => {
                at = Some(argv.get(i + 1)?.clone());
                i += 2;
            }
            "--multiplier" => {
                multiplier = argv.get(i + 1)?.parse().ok()?;
                i += 2;
            }
            "--seconds" => {
                seconds = argv.get(i + 1)?.parse().ok()?;
                i += 2;
            }
            "--cwd" => {
                cwd = Some(argv.get(i + 1)?.clone());
                i += 2;
            }
            "--pid" => {
                host = Some(Host::Pid(argv.get(i + 1)?.parse().ok()?));
                i += 2;
            }
            "--launch" => {
                let path = argv.get(i + 1)?.clone();
                host = Some(Host::Launch { path, args: argv[i + 2..].to_vec() });
                break;
            }
            _ => return None,
        }
    }
    Some(Args { at: at?, multiplier, seconds, cwd, host: host? })
}

pub(crate) fn cdp_embedded_probe(argv: &[String]) -> i32 {
    let Some(args) = parse(argv) else {
        eprintln!("{USAGE}");
        return 1;
    };
    let real = now_epoch_ms();
    let Some(fake) = moment_epoch_ms(&args.at, Some(0)) else {
        eprintln!("chrono: --at is not a moment: {}", args.at);
        return 1;
    };
    let origin = (fake, real, args.multiplier);

    // The host: launched with the engine variables (the session's future behaviour), or given.
    let launched;
    let root = match &args.host {
        Host::Launch { path, args: host_args } => {
            let qt_port = match cdp::free_loopback_port() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("chrono: no free loopback port: {e}");
                    return 2;
                }
            };
            let env = engine_env(&chrono_mech::current_environment(), qt_port);
            for (name, value) in &env {
                println!("env {name}={value}");
            }
            let target = chrono_mech::Target { path, args: host_args, cwd: args.cwd.as_deref(), env: &env };
            match chrono_mech::launch_plain(&target) {
                Ok(child) => {
                    println!("launched pid {}", child.pid);
                    let pid = child.pid;
                    launched = Some(child);
                    pid
                }
                Err(e) => {
                    eprintln!("chrono: {e}");
                    return 2;
                }
            }
        }
        Host::Pid(pid) => {
            launched = None;
            *pid
        }
    };

    let family = family_of(root);
    // `launched`, when there is one, terminates its host on every way out of this function - the
    // early returns below included - because PlainChild does that on drop.
    let discovery = match Discovery::start(family) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("chrono: discovery thread did not start: {e}");
            return 2;
        }
    };
    let mut attachers: Vec<Attacher> = Vec::new();
    let mut next_index = 0u32;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.seconds);
    let mut last_tick = Instant::now();

    while Instant::now() < deadline {
        if let Some(child) = &launched
            && !child.is_alive()
        {
            println!("host exited");
            break;
        }
        while let Some(notice) = discovery.try_recv() {
            match notice {
                Notice::Found(found) => {
                    // Discovery forgets a pair whose socket left one sweep of the table, so a port
                    // that blinks out and back is found twice - and one attacher per port is the rule.
                    if attachers.iter().any(|a| a.port() == found.port) {
                        continue;
                    }
                    println!(
                        "t+{:.1}s found port {} on pid {} ({})",
                        started.elapsed().as_secs_f64(),
                        found.port,
                        found.pid,
                        found.browser
                    );
                    match Attacher::connect(found.port) {
                        Ok(mut attacher) => {
                            let existing = attacher.attach_existing(origin, &mut next_index).unwrap_or(0);
                            println!("attached port {}: {existing} existing context(s) shimmed by name", found.port);
                            attachers.push(attacher);
                        }
                        Err(e) => println!("attach to port {} failed: {e}", found.port),
                    }
                }
                Notice::Unavailable(why) => {
                    eprintln!("chrono: discovery unavailable: {why}");
                    return 2;
                }
            }
        }
        attachers.retain_mut(|attacher| match attacher.pump(origin, &mut next_index) {
            Pumped::Closed => {
                println!("port {} closed", attacher.port());
                false
            }
            Pumped::Attached => {
                println!("t+{:.1}s port {} attached a context (live {})", started.elapsed().as_secs_f64(), attacher.port(), attacher.contexts().len());
                true
            }
            Pumped::Detached | Pumped::Idle => true,
        });
        if attachers.is_empty() {
            thread::sleep(TURN);
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
            discovery.update_family(family_of(root));
            for attacher in &mut attachers {
                let live: Vec<(u32, String)> =
                    attacher.contexts().iter().map(|c| (c.index, c.ty.clone())).collect();
                for (index, ty) in live {
                    if let Some(title) = attacher.evaluate_string(index, "String(globalThis.document ? document.title : '')") {
                        println!(
                            "t+{:.1}s port {} #{index} {ty} title={}",
                            started.elapsed().as_secs_f64(),
                            attacher.port(),
                            cdp::sanitise_target_text(&title)
                        );
                    }
                }
            }
        }
    }

    let shimmed: usize = attachers.iter().map(|a| a.seen().len()).sum();
    let failed: usize = attachers.iter().map(Attacher::failed).sum();
    let overflow: usize = attachers.iter().map(Attacher::overflow).sum();
    println!("summary: ports {} contexts {shimmed} failed {failed} past-ceiling {overflow}", attachers.len());
    if launched.is_some() {
        // Explicitly here for the printout - the drop at the end of the function would do it too.
        drop(launched);
        println!("host terminated");
    }
    if shimmed > 0 { 0 } else { 2 }
}

/// The family under the root, or the root alone when the snapshot cannot be read - said once on
/// stderr rather than silently narrowing the search.
fn family_of(root: u32) -> Vec<u32> {
    match chrono_mech::family_of(root) {
        Ok(family) => family,
        Err(e) => {
            eprintln!("chrono: process tree unreadable, searching the root alone: {e}");
            vec![root]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn everything_after_the_launched_host_belongs_to_the_host() {
        let parsed = parse(&argv(&["--at", "2030-01-01T12:00:00", "--multiplier", "7", "--seconds", "5", "--launch", "host.exe", "--at", "x"]))
            .expect("parses");
        assert_eq!((parsed.multiplier, parsed.seconds), (7, 5));
        match parsed.host {
            Host::Launch { path, args } => {
                assert_eq!(path, "host.exe");
                assert_eq!(args, ["--at", "x"]);
            }
            Host::Pid(_) => panic!("a launch, not a pid"),
        }
    }

    #[test]
    fn a_pid_host_and_the_defaults() {
        let parsed = parse(&argv(&["--pid", "4242", "--at", "2030-01-01T12:00:00"])).expect("parses");
        assert!(matches!(parsed.host, Host::Pid(4242)));
        assert_eq!((parsed.multiplier, parsed.seconds, parsed.cwd), (60, 30, None));
    }

    #[test]
    fn a_missing_moment_or_host_or_a_stray_word_is_a_usage_error() {
        assert!(parse(&argv(&["--pid", "1"])).is_none(), "no moment");
        assert!(parse(&argv(&["--at", "2030-01-01T12:00:00"])).is_none(), "no host");
        assert!(parse(&argv(&["--at", "2030-01-01T12:00:00", "stray", "--pid", "1"])).is_none());
        assert!(parse(&argv(&["--at", "2030-01-01T12:00:00", "--pid", "x"])).is_none());
    }
}

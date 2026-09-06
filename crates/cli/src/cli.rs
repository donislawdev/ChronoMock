//! Command surface: the product version, the target bitness, and the two usage texts.
//!
//! Split out of `main.rs` so the crate root is a dispatcher and nothing else. The usage strings are
//! CLI text, which is English-only by rule 15 - they are not translation keys and never will be.

pub(crate) const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn print_usage() {
    eprintln!("usage: chrono run <target> [--at <local-moment>] [--preset <id>] [--param id=value]... [--zone <+HH:MM>] [--mode <flow|frozen|xN>] [--scale-duration] [--scale-qpc] [--ticks N] [--timeout <s>] [--set-after T:M] [--jump-after T:moment] [--args \"...\"] [--cwd <dir>] [--report <path>] [--force] [--json]");
    eprintln!("       without --at (or --preset) the session clock starts at the real current time, so `--mode xN` alone just runs the target faster");
    eprintln!("       --scale-qpc also scales the high-resolution counter, which is where Python 3.13+ monotonic, .NET Stopwatch and Java nanoTime read elapsed time");
    eprintln!("       --force runs on even when the opening verdict says the substitution did not take effect (the target is stopped otherwise)");
    eprintln!("       --timeout gives up after N seconds and exits 6, for a pipeline that must not hang; a core that stops answering for 15 s exits 6 on its own");
    eprintln!("       --cwd starts the target in that directory; without it the target inherits ours, and a directory that does not exist stops the session rather than looking like a broken target");
    eprintln!("       (--preset supplies the moment and mode from presets/<id>.json, exclusive of --at/--mode/--scale-duration; --param fills its parameters, a trial start_date defaults to the target's file date)");
    print_calc_usage();
}

pub(crate) fn print_calc_usage() {
    eprintln!("usage: chrono calc [--base <today|now|YYYY-MM-DDTHH:MM:SS>] [--shift <±N<unit>>]... [--set-time <HH:MM:SS>] [--snap <target>] [--nearest <target>] [--to-zone <+HH:MM>] [--zone <+HH:MM>] [--calendar <us-banking|us-federal|pl>] [--format <mask>] [--json]");
    eprintln!("       or: chrono calc --preset <id> [--param id=value]...   (named moment, e.g. month-end, trial-first-day-after)");
    eprintln!("       or: chrono calc --analyze <pasted-date>   (interpret a date, e.g. 04/08/2008; shows both readings when ambiguous)");
    eprintln!("       --json emits machine output (chronomock.calc/1) for any of the above");
    eprintln!("       units: s m h d w mo q y bd (minute=m, month=mo)");
}

pub(crate) fn this_bitness() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

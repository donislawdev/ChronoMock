# Changelog

Notable changes to Chrono Mock, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`chrono run --dry-run`**, which works the session out and starts nothing. The tool's ordinary
  act is to launch your application and inject a library into it, and until now there was no way to
  see what a session would be without performing it. The plan names the resolved moment and the zone
  it is read in, where both came from, the arguments after they are split, the working directory, and
  which of the two mechanisms the target would take. For a preset it also names what each parameter
  was filled with and from where - which is the only way to learn the date a trial preset lands on,
  since it counts from the target's own file date. No process is started and no file is written,
  including the one `--report` names. It exits 0 for a workable plan and 2 for a target that is not
  there, the same code a real run gives for the same fact. A plan carries no verdict and says so.
  With `--json` it is one `chronomock.plan/1` record.
- **`chrono version`**, also spelled `--version` and `-V`. Prints the build, which of the two
  cores that executable is, and the wire protocol it speaks, on stdout so a script can capture
  it. Until now the tool could not answer the first question any bug report asks.
- **`chrono license`**, also spelled `--license`. Prints the licence this build is distributed
  under, the copyright line, the warranty disclaimer the GNU GPL asks a program to state, and the
  third-party components linked into that binary. It also names where the full licence text and
  the third-party notices are on disk, looking beside the executable and in the directories above
  it, since the two packages place them differently. A copy taken out of its package has neither
  file, and the notice says so and points at the licence text online rather than printing a path
  that leads nowhere. The licence itself is read from the build metadata, so what the program says
  and what the dependency audit checks cannot drift apart.
- **The version in the window title and title bar**, so a screenshot says which build it came
  from without anyone having to ask.
- **A start moment relative to now, in the window.** The command line has always taken
  `chrono run --at +30d` - start the application as if it were thirty days from now, which is how a trial
  expiry or a licence renewal gets tested. The panel could only take an absolute date, so the tester had to
  work the date out by hand first. There is now a line under the moment: a direction, an amount and a unit,
  and a button that fills the moment above with the answer. Months, quarters and years land on the calendar
  date rather than on a fixed number of hours, because the same engine works it out as in the calculator.
  Business days are not offered - a session carries no calendar, so they could only be refused.
- **A zone for the calculator's start point.** The command line has always been able to say which zone a
  base is read in - `chrono calc --base today --zone -08:00` asks what "today" is on the US west coast,
  which is a different day either side of midnight. The calculator panel could only re-express an answer
  in another zone, never read the start point in one, so that question had no answer in the window. The
  start point now carries a zone picker, offering this machine's zone first and the same closed list the
  substitution panel uses. The default is this machine, which is what the panel always did, so no existing
  calculation changes - it just says out loud which zone it was using.
- **`timeGetTime` now follows the session clock.** That winmm clock answers "milliseconds since the
  system started" - the same question `GetTickCount` answers, and Chrono Mock has always scaled that one
  under "scale duration too". Leaving the winmm one alone meant an application could see its own elapsed
  time disagree with itself by the whole multiplier. Seven runtimes were measured before this changed:
  it is never an application's main clock, but two game engines read it about 150 times a second, which
  is once or twice per frame. It is scaled only when you ask for a scaled duration, and the report says
  when a session leaned on it, because winmm is also the audio path and a media application that
  positions sound from that clock can drift. The multimedia timer itself is untouched - reading a clock
  schedules nothing.

### Fixed

- **Adding or removing a step in the date calculator left the date unchanged**, and the window
  reported "The CancellationTokenSource has been disposed." Those were one fault seen from two
  sides. The calculator waits out a quarter of a second before turning an edit into a
  calculation, so a burst of typing costs one run instead of one per keystroke - and a run that
  finished normally released its cancellation source while the calculator was still holding a
  reference to it. The next edit then cancelled a released source and threw, before any
  recalculation had been queued: the step appeared in the list, the date stayed as it was, and
  the error box reported the throw. Typing into the reverse-analysis field, pausing, and typing
  again went the same way.

- **Two calculator fields launched the engine once per keystroke.** The custom output format
  mask and a preset's parameter inputs both recalculated on every character typed rather than
  waiting for the quarter-second pause the rest of the builder uses - measured at ten launches
  for a ten-character mask. Both were call sites that predated the pause and were never moved
  onto it. On a test machine with an antivirus scanner watching process creation, that was
  visible as the field stuttering while you typed in it.

- **Five United States holidays answered with today's rule for every year in history.** Both
  calendars now carry the changes that actually happened, so a date before 1986 - or before 1971 -
  comes back correct:
  - **Martin Luther King Jr. Day** did not exist before 1986 (Public Law 98-144 took effect on the
    first 1 January after a two-year period). The third Monday of January 1985 is an ordinary
    business day again.
  - **Washington's Birthday** was 22 February, and **Memorial Day** was 30 May, until the Uniform
    Monday Holiday Act moved both to a Monday with effect from 1971.
  - **Columbus Day** did not exist as a federal holiday before that Act created it in 1971.
  - **Veterans Day** spent 1971 to 1977 on the fourth Monday in October, so 11 November was an
    ordinary working day for seven years, before Public Law 94-97 moved it back from 1978.

  Each of these is a separate calendar entry with its own validity window rather than a single
  rule with a start year, because three of the four holidays that Act touched existed before it
  and only moved. The first year of the pre-1971 entries is deliberately left open: it is known
  from secondary sources and this project puts only primary sources into calendar data.
- Four Polish holiday sources used a semicolon in prose, and the Polish strings file described
  itself as the English one.

### Security

- **The promise that this tool never reaches the network is now guarded rather than only
  stated.** A test scans every Rust and C# source for network APIs and permits only the ones
  registered by file with a reason, refuses a dependency that could speak to a network, and
  refuses a networking feature of the `windows` crate. What it cannot prove is written in its
  own header, and `SECURITY.md` says the same in the section on what this tool does.
- **A second layer that reads the built binaries rather than the source.** Every Windows
  executable carries an import table - the DLLs the loader resolves before the program runs,
  written by the linker from what the code actually calls. A test now pins that list for all six
  binaries this workspace builds, on both bitnesses, with a reason for every entry, so a
  dependency that reached the network would fail the build even if nothing in our source
  spelled it. The core links Winsock and nothing else that touches a network, for the loopback
  debug port Chromium mode uses. The library injected into the target links no networking DLL at
  all, `ws2_32` included, although intercepting `connect` is one of its jobs - it resolves that
  module only when the target has already loaded it.

## [0.1.0] - 2026-09-04

First public release. There is no previous version to compare it against, so rather than
listing everything as "added", here is where to find out what it does:

- **[README](README.md)** - what it is, how to run it, and the honest limits.
- **[chronomock.donislawdev.com](https://chronomock.donislawdev.com/)** - the same in longer
  form, including how the time source audit works and how this compares with RunAsDate and
  libfaketime.

From the next release onwards this file records what changed.

[0.1.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.1.0

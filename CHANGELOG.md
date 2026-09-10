# Changelog

Notable changes to Chrono Mock, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-09-10

### Added

- **The binaries this project builds are now Authenticode-signed**, with an RFC 3161 timestamp so
  the signature outlives the certificate. Windows names the publisher instead of warning about an
  unknown one. The signing key is on a hardware card that cannot be exported, so no workflow can
  reach it: the build happens in CI, the signature happens on a machine with the card, and each step
  refuses to continue on something it has not checked - the signing step verifies the build's
  attestation before touching it, and reads the certificate back out of every file it signs. The
  around 240 Microsoft assemblies inside the window package are left exactly as they arrived, because
  re-signing somebody else's binary would destroy their signature and claim we made it.
- **A bill of materials, hashes and build provenance for every release.** Each release from now on
  carries `SHA256SUMS`, an SPDX 2.3 document per package listing every third-party component with its
  version and licence, and a build-provenance attestation over both archives and every binary inside
  them - including `chrono_hook.dll`, the library the tool injects into other processes. Verify it
  with `gh attestation verify <file> --repo donislawdev/ChronoMock`, which answers what a checksum
  cannot: which workflow, in which repository, at which commit, produced those exact bytes. The
  release is now built by that workflow rather than on a developer machine, because provenance for
  bytes nobody downloads is worth nothing. The archives are still not Authenticode-signed.
- **`chrono license --components`**, which prints every third-party component in the tool with its
  version, licence and copyright holder, from a register compiled into the binary - so a copy taken
  out of its folder, on a machine with no internet, still answers the question.
- **The .NET runtime inside the window package is now declared.** That package ships around two
  hundred Microsoft assemblies, and until now `THIRD-PARTY-NOTICES.md` did not mention them. Two of the
  three runtime packs are MIT, and Microsoft's own `LICENSE.TXT` and `THIRD-PARTY-NOTICES.TXT` now
  ship alongside them under `dotnet/`. One of the three is not MIT and is recorded as such.
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
- **An About window, and a way to support the project.** The command line has always answered
  `chrono license`. The window had none of it, and the window is the default way into this product.
  The title bar now carries a support button and an About dialog, and the dialog states the build,
  the copyright line, the SPDX identifier, the warranty disclaimer the GNU GPL asks a program to
  state, where the full licence and the third-party notices sit on disk, and a button that opens the
  licence with whatever the machine reads text in. Every bundled component is one expander away. The
  support button is the first control here that leads anywhere off this machine, so it is worth being
  exact about the promise it sits next to: nothing in this tool opens a socket, the address is handed
  to the shell, and the browser the user already chose is what connects.
- **The date calculator now says when a shift clamped the day.** A month-folding shift that lands in
  a shorter month clamps: 31 January plus one month is 28 February, and 29 February plus one year is
  28 February. Every one of those is correct and documented, and every one was invisible in the
  result - so a reader who saw a day change with no reason given could not tell the rule from a
  defect, on a date they were about to act on. The engine now reports which step clamped and from
  what day. The human output adds a line under the step, JSON carries `clamped_steps`, and the window
  shows a sentence under the result, which is where it was most invisible, because that panel
  displays no intermediate steps at all.
- **The custom output format understands the 12-hour clock, and names the letters it does not know.**
  `h:mm tt` used to render as `h:05 tt` - raw mask letters inside something shaped like a formatted
  time - because the vocabulary knew only `y M d H m s`. The United States is one of the two markets
  this tool targets and 12-hour with a designator is its rule, so the commonest US time format was
  unreachable and nothing said so, which sends a reader looking for a typo of their own. `hh`/`h` and
  `tt`/`t` are now understood, and every letter run the vocabulary does not recognise is reported: a
  line under the result, `custom_format_unknown` in JSON, a warning under the row in the window. The
  text still renders, because a partly matching mask is useful when the point is to mirror another
  application's output. Quoting arrives with it and is not a separate nicety - adding a token changes
  what every unquoted mask containing that letter means - so `'text'` is literal and `''` is an
  apostrophe, as in Java's SimpleDateFormat. Punctuation is never reported, since a mask is made of it.
- **A bare number is read as an epoch**, in seconds and in milliseconds, in the session zone. Epoch
  has been listed among the recognised formats all along and the analyser refused it, which made
  pasting a number out of a log - the commonest thing a tester has in hand - an error. Both units are
  offered rather than one guessed from the magnitude: `1000000000` is September 2001 read as seconds
  and January 1970 read as milliseconds, and both are things people paste.
- **`chrono calc --base-utc`, and `base.absolute_utc` in a preset**, which read the start point in
  UTC instead of the session zone, for a base that names one instant rather than a wall-clock
  reading. The conversion lives in the core, so the window and the command line cannot drift apart on
  one moment. Adding a field does not change the preset schema version. Why this was needed is under
  Fixed, with the two presets that were wrong without it.

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

- **A 30-day trial preset pointed one day past the trial.** `trial-last-day` worked out
  `start + 30 days` at 23:59:59, so a trial installed on 1 January landed on 31 January - past the
  boundary for both usual implementations at once, since an application checking
  `now - install > 30 days` sees thirty days and fourteen hours, and one counting calendar days is on
  day 31. The preset whose whole question is "does it still work in the last moment" landed on a
  moment where the application already refuses, and the tester could not tell an application bug from
  a preset that misses. The install day is day one, so the last day is `start + length - 1` and the
  first day after is `start + length`. **Both presets move by a day**, and they have to move together
  or they leave a gap. The rule is written into what each preset says about itself and not only into
  the documentation, because that sentence is what the reader has in front of them while looking at
  the date.

- **`year-2038` and `epoch-zero` missed their own instant by the zone offset.** Each names one
  instant - the signed 32-bit `time_t` limit and epoch second zero - and each expressed it as a
  wall-clock reading in the session zone. In UTC+02:00 the 2038 preset produced epoch 2,147,476,447
  against a limit of 2,147,483,647 and reported no significance marker at all, so the tester ran
  "2038 boundary", never crossed it, and read green. Both now use the UTC base described under Added.
  Naming both kinds of base at once, putting an explicit offset on the UTC one, or passing the flag
  twice are refusals rather than silent picks, because "last one wins" there would move the moment by
  a zone offset without a word.

- **A calendar could name a holiday on a date that does not exist.** A `fixed` rule went straight to
  the civil-date arithmetic, which does not validate, so a rule naming 29 February produced 1 March
  in a common year - and the report named that day as the holiday. 31 April, a day in no year at all,
  produced 1 May every year. The engine now answers with nothing when the year's month is too short,
  exactly as the "nth weekday" rule already did for the fifth Monday of February. The loader bounds
  the day by the month's longest length in any year rather than by a flat 1 to 31, so 29 February
  still loads as the legitimate rule it is while 31 April is refused as the typo it is - it would
  otherwise be a holiday silently absent forever. A wrong day off is one error, a wrong day off
  carrying a holiday's name is two, and calendars are the part of this tool outsiders are invited to
  write.

- **A calendar with a Friday-Saturday weekend loaded cleanly and was then wrong twice, in silence.**
  The observance rules match Saturday and Sunday by name - the variant names say so - while the
  weekend is a free list of days, so pairing the two meant a Friday holiday never shifted because the
  rule did not see it, and a Sunday holiday moved to Monday although Sunday is a working day there.
  Both surface as a wrong payment date with no message anywhere. The loader now refuses that
  combination and says what to use instead. It is deliberately not generalised in the engine, because
  generalising needs new variant names, and reusing one that promises Monday in order to produce a
  Sunday would mislead, which is worse than missing.

- **A four-digit year check counted characters**, so `12/25/+999` was read as the year 999 while the
  message beside it promised `N/N/YYYY`. Three more messages in the same pass said something other
  than what the code did: the zone bound now says which part of its range is the real map and which
  is deliberate extra coverage, a year the engine can compute on but the moment field cannot hold
  gets its own message instead of sending the reader to fix a shape that was already correct, and the
  "ambiguous" line names which ambiguity it means.

- **The duration clock could rewind.** The three duration axes multiplied elapsed real time by the
  multiplier with wrapping arithmetic, so about 10.6 days into a session at the maximum multiplier
  the product overflows and the axis jumps backwards by centuries in a single step. A duration clock
  that never goes backwards is one of the few things this tool promises outright. The axes now hold
  at the end of their range instead, and a session that reaches it says so rather than carrying on
  quietly.

- **A scaled wait could turn a sleep into a spin.** Integer division truncates, so under a large
  multiplier every timeout below the multiplier in milliseconds came out as zero - and a zero-length
  sleep is not a short sleep, it gives up the rest of the time slice and returns at once. An
  application polling in a loop went from sleeping to burning a core. Scaled waits now have a floor
  of one millisecond, and a session says when a wait it was asked to scale could not be scaled any
  further.

- **Recording a session after a downgrade wiped the whole history.** Loading a history written by a
  newer build refuses it and keeps the file, which was the promise. Appending broke that promise one
  step later: it built its new list from the refusal's empty answer and wrote over the file, so the
  first session recorded after going back to an older build destroyed the log. Appending now refuses
  in the same case, and writes are serialised so two of them cannot interleave.

- **Repeating a session did not repeat the session.** Five of the inputs that decide what a run does
  were never recorded - the target's arguments, its working folder, and the three switches - so
  repeating filled four fields, left the rest holding whatever the form happened to have, and Start
  produced a third setup that was never run and never recorded. Nothing said so.

- **The summary read the target and the zone live**, so a tester who lined the next run up before
  copying got a report describing the form rather than the session that ran. That summary is the one
  artifact that leaves this tool and lands in somebody else's ticket.

- **A calculator step's remove button was laid out off the panel.** A step row was a horizontal
  stack, and such a panel hands every child its full desired width and lets the overflow run past its
  container. Measured at the default window size, the row wanted 439 logical pixels where the column
  had 331, so the unit list ended mid word and the remove button was drawn 108 pixels beyond the
  column, underneath the opaque result panel. That button is the only control that takes a step off,
  so a step could be added and then not removed. The row is a grid now, with the editor under the
  kind picker, measured after the change against the column edge at the default size and at the
  window's minimum width.

- **One scroller wrapped all three calculator columns**, so the tallest decided for everybody:
  measured at 1560 by 1200, the preset list ran 938 pixels below the bottom of the window, and
  reaching the last preset pushed the result and the builder off the screen. Each column now scrolls
  inside its own card.

- **The primary action turned grey on hover, and the control that ends a session sat three groups
  away from the one that starts it.** The button carried its accent as a local value while the stock
  template repaints a named part of itself on hover, and a template part is not the control, so the
  one findable control on the screen dropped to grey the moment the pointer arrived - which reads as
  disabled rather than as hovered. It has its own template now, painting state as a veil over
  whatever fill the usage sets, measured by pixel on a live window at the accent colour at rest and
  brighter on hover. Stop is the same button, told apart by its label and by the fill turning to the
  failure red, never by colour alone.

- **The confirmation in front of clearing the history answered in the system language.** It was a
  native message box, and Windows labels those buttons, so an English interface offered Tak and Nie
  with no string of ours involved anywhere. The application asks with its own dialog now, names each
  answer after what it does rather than offering a Yes that says nothing about the question, starts
  the focus on the answer that changes nothing so a stray Enter cannot confirm what has not been read
  yet, and dresses the destructive answer in the same red as the session Stop control, with the
  wording carrying the meaning either way. The three dialogs that report that the strings, the
  startup path or the dispatcher has failed stay native on purpose - they report the failure of the
  very things a themed window needs in order to draw.

- **The two modules sat on different backgrounds**, with the seam visible under the module switch.
  The window had carried a background setting the whole time and it never painted a single pixel,
  because that property on this window type is accepted by the parser, reads exactly like a working
  declaration, and does nothing at all. The background is set where it paints now, and two tests
  check the rendered result rather than the presence of the attribute.

- **Three controls on the substitution screen did not look like controls.** The scenarios were eight
  loose phrases with no edges, nothing said they could be pressed, and the stock template revealed
  the chosen one only on hover - they are chips now, with the choice carried by weight and border as
  well as by colour, so it survives a colour-blind reader. The moment row was a centred horizontal
  stack of nine controls whose labels did not line up and which overflowed on both sides, and it is a
  form grid now.

- **The interface blamed the core for its own stalls, and three of its failures stayed quiet.** The
  idle watchdog armed its timer before each wait and let it run through the handling loop, which
  happens on the interface thread by design, so anything that parked that thread reported a core that
  had stopped while events were arriving perfectly well. Separately, the marker that says diagnostics
  were dropped fell out of the very queue it was trimming, and two other failures had no way of
  reaching the screen at all.

- **A calculator refusal answered in English inside a translated window.** The calculator runs the
  engine as a child process and passed its refusals straight through, so exactly one of the nine was
  translated and the rest arrived as a command-line sentence with a key in brackets. All nine answer
  in the language of the interface now.

- **An application with two Chromium pages produced two identical coverage rows.** The core names a
  channel by the kind of context plus the API and emits one row per context, so the rows differed
  only in their counts and nothing on the panel said which was which.

- **The core reported that it had stopped when the client had sent something it could not read.** A
  transport failure ended the command stream in exactly the silence of an ordinary end of input, so a
  client that sent an over-long line, or a byte that is not UTF-8, was told the core had stopped -
  a diagnosis pointing away from the mistake. The stream says why it ended now.

- **Three command-line refusals dumped a structure instead of saying what went wrong.** A Win32
  failure was rendered with Rust's debug formatting, so a missing path answered with a struct dump
  where a sentence belonged.

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

[0.2.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.2.0
[0.1.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.1.0

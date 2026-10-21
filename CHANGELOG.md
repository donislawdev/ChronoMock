# Changelog

Notable changes to Chrono Mock, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A screen reader hears what changes where it is not looking.** The session's status and its verdict,
  a command that failed during a session, the result's verdict as the result screen opens, a history that
  could not be saved, the calculator's answer and its error, the line under the application filled in from
  a shortcut, and what a copy button did ("Copied", "Clipboard busy") are announced as they change. A
  failure cuts in, the rest waits for the reader to finish. The calculator's answer is announced once a
  number being typed rests for half a second, not for every key, and coming back to a screen announces
  nothing that has not changed. The session clocks, which change every second, are not announced.
- **A shortcut (.lnk) can be the application.** Picked with Choose… or dropped on the window, a shortcut
  fills the form with the program it starts, the arguments it hands it and the folder it starts it in,
  and a line under the application says so, because the arguments and the folder stand under Speed and
  options. A shortcut that leads to no program file - one an installer made, or one to a document or a
  script - is refused with the reason, and so is one whose program is gone. Nothing starts until Start.
  The file picker used to swap a picked shortcut for its program in silence and drop its arguments and
  folder.
- **"Copy as command" under the calculator's result** copies the question it answers as a PowerShell
  command line: the engine by its full path, so it works from a portable copy that is not on PATH, and
  the same arguments, so a terminal gives the same answer. Windows PowerShell 5.1 passes three kinds of
  value on wrongly - one with a double quote in it, an empty one, and one with a space that ends in a
  backslash - so a command holding one of them starts with a comment that says to run it in PowerShell
  7.3 or later.
- **`chrono calendars` lists the calendars the calculator can count in**, the way `chrono presets`
  lists the presets: every calendar file `--calendar` reads, with its country and its own name, and
  every file it would refuse, with the reason. `--dir` reads another folder, `--json` answers in
  `chronomock.calendars/1`. The window's calculator now offers the calendars this command lists,
  so a calendar added as a data file can be picked there too.
- **A Chromium session says what its pages did that the fake clock does not reach.** Three new
  warnings, each said only when it happened, in a Chromium session and for the pages of a web engine
  inside an application alike. `chromium.engine_stamps_real`: a page's animation frame got its stamp, or
  the page read `Event.timeStamp` or `document.timeline.currentTime`, while its timers ran faster - the times the browser
  stamps on frames and events run at real speed, so they disagree with `performance.now`.
  `chromium.animations_real`: a page played CSS or Web Animations while its timers ran faster, and
  they played at real speed. `chromium.contexts_unread`: pages or workers ended before the audit,
  which reads once a second, could read them, so what they called is not in the counts. The verdict
  does not change. The shim notices the stamps without changing what the page gets.
- **A GitHub button in the window's title bar**, beside Donate. It hands
  `https://github.com/donislawdev/ChronoMock` (the source, the releases and the issue tracker) to your
  browser, and its tooltip says so before you press it, the same way Donate does. Chrono Mock itself
  still opens no connection. If Windows has no browser for the address, the window says so and shows
  the address to open by hand.
- **The report says when timers could not keep the session speed** (`timer.interval_floored`). A
  window timer (`SetTimer`) never fires more often than every 10 ms, a limit Windows sets, and a
  periodic waitable or thread-pool timer is never given a period under 1 ms, so at a high speed such
  a timer runs slower than the rest of the session. Waits held at their floor were already reported
  (`wait.timeout_collapsed`), timers were not.
- **The evidence file names the run, not only the time it ran at.** Under the `requested:` line,
  `chrono run --report` now writes the version and the core's bitness, when the run started (UTC, from
  the real clock), the target, the folder it started in, how many arguments it got, and the switches
  the run was given. The arguments themselves are left out unless the new `--report-args` asks for
  them, because an argument can carry a password and the file is made to be handed on.
  `--report-args` without `--report` is refused.
- **The date analyser reads the dates Chrono Mock prints.** `chrono calc --analyze` now reads an ISO
  date and time with a fraction of a second and a UTC offset, such as `2026-01-01T10:00:00+02:00` or
  `2026-01-01T10:00:00Z` - the form the calculator prints itself, which it used to refuse as a bad
  second. A date with an offset is a point in time, so it is shown in the session zone, like a pasted
  Unix timestamp, as the new reading `iso_offset`. A fraction is cut to the second. A date whose
  instant falls outside the years the calculator computes on once it is shown in the session zone is
  refused.
- **Three more time functions are covered: `QueryInterruptTime`, `QueryInterruptTimePrecise` and
  `QueryUnbiasedInterruptTimePrecise`.** Under `--scale-duration` they run at the session speed like the
  tick count and `QueryUnbiasedInterruptTime`, and the audit lists them. The two interrupt-time clocks
  keep the real time the computer has slept since it started, as Windows does - sleep is never
  multiplied by the speed. This changes what an embedded Chromium engine in WebView2 reads under
  `--scale-duration`: it imports the third one, which used to stay real beside a scaled tick count. The
  audit now has 44 channels.
- **The session log says when the computer slept.** The session clock stands still while the computer
  sleeps, as during a pause. A new `host_slept` event says how long it slept, at the first heartbeat after
  waking, `session_verdict` carries the total as `host_slept_ms`, and the report and the window warn
  `time.host_slept`.
- **The report says when a timer for a set date and time may fire at the wrong moment of the session.**
  Such a deadline is turned into a delay when it is set, at the speed of that moment, so one still
  pending when the session jumped or changed speed fires where the old clock would have reached it.
  The new warning `timer.absolute_due_converted` appears only when that can have happened.
- **The report says which browser a Chromium session drove.** `session_verdict` carries what the
  browser calls itself (`browser`, such as `Chrome/152.0.7977.149`), and the report and the evidence file
  have a `browser:` line under the target. A native session leaves it out.
- **The report says when a page or worker was already running when the session reached it**
  (`chromium.context_reached_running`): a page whose own scripts were in it before the time shim, or a
  worker that was not paused on start. Its scripts may have read the real clock first, and a timer it
  set before keeps its real interval. Said in a Chromium session and for the web engines inside an
  application, whose pages usually load before the session finds them.
- **The report says when an application left running will open a debugging port later**
  (`embedded.variables_outlive_session`). With the web engine channel on, the application keeps the
  setting that makes its engine open a debugging port, so a web view it opens after the session opens
  one too, reachable by other programs on the computer, until the application is restarted.
- **The report says when a web page's timers could not keep the session speed**
  (`chromium.timer_rate_capped`). A browser runs a timer set from inside another timer no more often
  than every 4 ms, so at a high speed a timer that would have to run more often than that runs slower
  than the session - the clock itself keeps the speed. Measured on Chromium: from the seventh timer of
  a chain, and from the seventh tick of a repeating one.
- **The report says when a frame of a page was out of the session's reach** (`chromium.frames_unreached`):
  a frame of another origin of the same site, a sandboxed frame, or one that was in the page before the
  session reached it. Such a frame keeps the clock it started with (the real one, if it was there
  before the session), does not follow a jump or a change of speed, and its calls are not counted.

### Changed

- **Every copy button says on itself what its copy did** - "Copied", or "Clipboard busy" when another
  program held the clipboard - for a moment, then reads as itself again, and it never changes width while
  it does. The result's summary and diagnostics shared one line beside the buttons that never cleared,
  so a second copy changed nothing on the screen, and the calculator said nothing at all when the
  clipboard was busy. In the calculator a format's Copy now stands beside the format's name, and the
  value under them has the whole width of the column.
- **A portable copy keeps its session history and diagnostics logs in `%LOCALAPPDATA%\ChronoMock`,**
  as an installed copy already did. Beside the program, on a shared folder or a stick passed round,
  they held everybody's sessions - the applications tested and the arguments they were started with -
  where anyone with the folder could read them. The history an older version kept beside the program is
  read in once, when this version first starts, and left where it is. Older diagnostics logs beside
  the program stay there.
- **Both scenario lists search the same way.** A word typed into the session's scenario search now
  finds a scenario by the line that says what it tests, as the calculator's search always did - both
  lines are shown on every row. The calculator's search box is the session's: it has the clear button,
  Enter chooses the first scenario left, and a search that leaves nothing says so in the list's place.
  The scenario chosen in the calculator stays marked while it still matches, and is marked again when
  a search that hid it is cleared, with its parameters as they were typed.
- **A `jump` to an absolute moment that names no moment gets the same answer on both mechanisms.** A
  protocol client that sent `{"kind":"absolute"}` without `local` was told `moment.invalid` by a
  Chromium session, while a native session read the missing moment as an empty one and, when the same
  command named a zone outside the range, answered `time.bad_zone`. Both answer `moment.invalid` now.
  The window and `chrono run` always send the moment, so neither of them could get here.
- **A preset file that puts the `target_file_creation` hint on a parameter that is not a date is
  refused when it is read.** The hint fills a date, so such a file is wrong whatever values are given.
  It used to be refused only when the hint was needed, and then as a bad value, so `chrono calc` blamed
  `--param` for a fault in the file.
- **A number on the `chrono run` command line is digits and nothing else, as it is in `chrono calc`.**
  The multiplier of `--mode xN`, `--ticks`, `--timeout`, both numbers of `--set-after` and the tick of
  `--jump-after` took a leading `+`, so `--ticks +5` ran five heartbeats and `--mode x+60` sixty times
  faster, while `calc` refused the same sign. They are refused now with "(digits only)" at the end of
  the message, and so are `x-5` and `--set-after 3:-3`, which used to be told the multiplier must be at
  least 1. A number that is only too large is refused without that note.
- **A Chromium session covers every page and worker, however many there are.** The audit gives the
  first 256 a row of their own and counts the rest together in the last row, and every one of them runs
  on the session clock. Past 256 they used to be refused the time shim and ran on the real clock, which
  an application that starts a worker per task reached within minutes, and the verdict said `partial`.
  The warning `chromium.context_ceiling_reached` is replaced by `chromium.contexts_pooled`, and
  `context_count` counts every context.
- **The frames of a web page follow a jump and a change of speed.** A frame of the page's own origin
  started on the session clock but kept it as it was when the session jumped or changed speed, and its
  calls were not counted. It now moves with the page, and its calls count in the page's row. A frame of
  another site, which the browser runs as a page of its own, read the real clock: the time shim was
  tried before the frame had a document, failed, and was taken back. The frame now gets the shim with
  its first document and has a row of its own, counted as covered once its shim answers.
- **A repeating timer in a web page takes a change of speed from its next tick, and keeps its
  cadence.** It used to keep the cadence it was armed with, and a delay was cut down to whole
  milliseconds the browser's way: at x60 a one-second timer ran 4% fast (960 ms of session time
  between ticks, measured). A tick missed whole is not made up. The warning
  `chromium.rate_change_affects_running_timers` now says what is left: a one-time timer already waiting
  finishes its wait at the old speed. At speed 1 a one-time timer goes to the browser as the page set it.
- **`performance.timeOrigin` follows the session clock in a web page,** so `performance.timeOrigin +
  performance.now()` equals `Date.now()` as it does without a session. `performance.toJSON()` keeps the
  browser's own value.
- **The time shim keeps its clock to itself.** The `__chronomock` object in a page is no longer an
  enumerable global with the clock in its fields: a page's own scripts do not see it among their globals,
  cannot replace it, and reach only the methods the session uses. A page still holding the shim of an
  older version is still moved, released and read.
- **A relative jump in a Chromium session keeps the milliseconds of the clock,** as in a native
  session - it used to step through the clock's text and lose up to 999 ms each time. A jump of a kind
  neither mechanism knows is refused with `moment.unsupported_kind` in both, where a Chromium session
  read it as absolute. A change of speed is no longer reported as a caveat about running timers when no
  page or worker was there to have one.
- **Every answer to `start` carries the `id` the client gave it,** the verdict and an error before the
  session runs, where it used to say `1` whatever was sent. Both built-in clients send 1. The protocol's
  last-resort error line carries the protocol version rather than a fixed `1`.
- **A Chromium session's clock stands still while the computer sleeps, and a correction of the system
  clock no longer moves it,** as in a native session. It used to run on through a sleep at the session
  speed: one hour corrected on the system clock moved the session clock sixty hours at x60.
  The pages are given the clock again when they stand more than a second of session time from it, the
  session logs `host_slept`, and the report warns `time.host_slept`. A page that runs a script in the
  moment between waking and that correction reads the moved clock.

- **A call count stops at 1 048 576 and is then shown as "at least".** Every clock call used to write
  one shared counter, which cost a target with many threads several times the price of the call itself
  (86.6 ns against 14.1 ns at 8 threads, measured). `coverage` marks such a count with `capped`, and the
  report, the window and the copied summary say "at least". The audit table's count column now takes the
  width its numbers need.
- **`GetSystemTimePreciseAsFileTime` moves at the resolution of the timer hardware.** It used to move in
  steps of one timer interrupt multiplied by the speed - 47 ms on average at x60 on the measuring
  machine, 0.94 s at the default timer. The session's anchors are now read from the precise clock
  (`QueryUnbiasedInterruptTimePrecise`, which also leaves sleep out), while the plain clocks keep reading
  the plain one, so the precise wall clock is never behind the plain one, as in Windows. It costs about
  26 ns more per call on 64-bit and 38 ns on 32-bit.
- **A timer or a wait set for a date and time follows the fake clock in every session.**
  `SetWaitableTimer`, `SetWaitableTimerEx`, `SetThreadpoolTimer`, `SetThreadpoolTimerEx` and an absolute
  `NtDelayExecution` are converted without `--scale-duration` too: a timer set for "tomorrow 03:00" on a
  clock at 2038 used to wait twelve real years. Relative delays and periods change only under
  `--scale-duration`, as before.

- **Another running session ends a start with exit code 7.** It used to be exit code 3, the code of
  an internal error, with a line saying the core ended without a verdict - so a pipeline could not
  tell a busy machine from a crash. It is the one start failure that waiting fixes. The `error` event
  carries the same 7.
- **A run whose evidence file was not written ends with exit code 8.** When `chrono run --report`
  reached a verdict but could not put the evidence at the path it was given - the disk was full, or
  another program held the old file open, which then stays as it was - the exit code was still the
  verdict, so a pipeline keeping that file as the proof of the run could keep an older one under exit
  code 0. It now ends with 8, and says which verdict it replaces. The verdict is still in the report on
  standard output and in the temporary file the run names. A code that is no verdict (1, 2, 3, 5, 6, 7)
  stays as it was.
- **A works session can still be unreliable evidence.** The evidence file and the copied report now
  lead with the unreliable-evidence banner also when the application read none of the clocks the
  session replaces (`coverage.session_clock_never_read`), or is a Go program whose wall clock no hook
  reaches (`runtime.go_wall_clock_unreachable`). The verdict and the exit code stay as they were. The
  window's result screen shows those warnings under the verdict's reason, where it used to keep them
  folded inside the audit.
- **The web engines line claims pages only when some were put on the session clock.** It said that
  the engines' pages ran on the session clock whenever an engine was reached. When the session
  reached an engine but put none of its pages on its clock that way, the line, the window's note and
  the copied report now say so, and the window's process table no longer marks that engine's renderer
  as one whose pages were reached. Each engine in `session_verdict.engines` now carries its own
  `context_count`, and the process table and the warning about a renderer read that one, so with two
  engines reached and pages in only one of them, the other engine's renderer is no longer said to have
  had its pages reached. A core older than this field reads as zero, which claims no pages.
- **A refused session names the way to run anyway.** Under the refusal the report now says that
  `--force` starts the application regardless, and that the report then marks the session unreliable.
- **The audit no longer claims a time function that part of the application reads past.** Most time
  functions live in kernelbase, and kernel32 only passes the call on, so Chrono Mock places its hook
  where the two meet. When it cannot - another program hooked kernelbase first, or a Windows build
  where kernel32 has a copy of its own - the hook stays on kernel32: a program that calls kernel32
  still reads the session clock, but the C runtime and anything else that goes to kernelbase directly
  read the real one. The report used to list such a function as covered. It now lists it as not
  covered, so the session works partially (exit code 10). On the Windows builds measured so far,
  kernel32 leads into kernelbase for every one of these functions.
- **The audit follows 4096 processes in a session instead of 256.** A session counts every process it
  ever ran, not the ones running at once, so an application that starts a short helper every ten
  seconds went past 256 in about 43 minutes and the rest of the session's processes were left out of
  the audit. 4096 takes that to about eleven hours. The processes past the limit are still put on the
  session clock, and the note about them says the new number. Measured on both bitnesses, the larger
  limit costs nothing visible: the session starts in the same time and the core uses the same CPU and
  memory.
- **The note about more processes than the audit can follow says what happened to them.** It said
  that some of them ran with no fake clock at all. They were put on the session clock, and only the
  audit could not check them, which is what the note now says, on the command line and in the window.
- **A session waiting for a library that never loads wakes the application less often.** The hook
  looks for a window, sound or network library that the application loads after the start: often in
  the first two seconds, then every 250 ms for as long as the session lasts. It now backs off to once
  a second, so a console program that never loads those libraries is woken once a second instead of
  four times. A library loaded later than that is hooked up to a second after it arrives.
- **`chrono run` refuses a flag given twice.** A flag that takes one value (`--at`, `--preset`,
  `--zone`, `--mode`, `--args`, `--cwd`, `--ticks`, `--timeout`, `--set-after`, `--jump-after`,
  `--report`) used to let the last one win in silence, and so did `--param` with the same id. Both are
  now refused with exit code 1, the way `chrono calc` already refused them.
- **The business-day limit counts the whole calculation.** No more than 1,000,000 business days are
  counted in one calculation, all its steps together, where the limit used to stand on each step
  alone - so two hundred steps of a million business days each held the processor for seconds. The
  refusal names the step that goes past it, before that step is counted, and says to lower the counts.
- **A preset has at most 64 steps and 16 parameters.** The shipped presets have at most three and
  two. A longer file is refused by its count, like a calendar with more than 2,000 holidays, and the
  refusal says what to remove.
- **"Not found" for a calendar or a preset names the folders and the ids on offer.** It printed
  `looked in <exe>/calendars` as written and no id at all. It now names the folder by its path and the
  ids it holds, or says that neither folder exists.
- **An unexpected exit code of the core is named.** When the session closed but the core ended with a
  code that is not a verdict, such as 101 for a crash in the tool itself, `chrono run` still exits with
  3 and now says which code the core ended with, as it already did for a core that stopped before the
  session closed.
- **Calculator failures carry a key the window translates.** A calendar that is missing or cannot be
  used, a scenario that is missing, broken or uses something not built yet, a value for a scenario's
  parameter that is missing or cannot be read, and a market calendar that cannot be loaded each end in
  a key of their own (`calc.calendar_not_found`, `calc.calendar_invalid`, `calc.preset_not_found`,
  `calc.preset_invalid`, `calc.preset_not_built`, `calc.preset_parameter`,
  `calc.market_calendar_unavailable`), so the window says them in its own language instead of in
  English. A preset step that cannot be used is named by its number.
- **An unknown unit is refused with the units that work** (`s, m, h, d, w, mo, q, y, bd`), as an
  unknown `snap` or `nearest` target already was.
- **The analyser's refusals quote what was pasted.** They quoted the text built from it, such as a date
  with `T00:00:00` added, and a month out of range is now named by its number.
- **`chrono calc` lists the calendars it can find** in its usage, from the folder `--calendar` reads,
  where it listed three names written by hand.
- **The window opens sooner.** Its own code ships compiled ahead, so a start no longer waits for it to
  be compiled, and the session and result screens are built the first time a session reaches them
  rather than before the window first shows. Measured on the published window, to its first frame:
  0.96 s to 0.80 s with the files already read once, and 1.53 s to 1.31 s on the first start after
  unzipping. The package grows by about 1 MB.
- **The window package is 23 MB smaller.** It no longer carries the Windows Runtime projection
  (`Microsoft.Windows.SDK.NET.dll` and `WinRT.Runtime.dll`), which nothing in it used: 170 MB to 147 MB
  unpacked, and about 6 MB less to download. It is still built for Windows 10 1809 and Windows Server
  2019 and later. The projection was the one component in the package without an SPDX licence, and it
  leaves the SBOM and the third-party notices with it.

### Fixed

- **The calendar beside a date works from the keyboard.** Opening it puts the focus on the day of the
  date in the field, in that date's month - it used to open on the current month whatever the field held,
  with the focus left on the button that opened it. The arrows move the day, Enter or Space takes it,
  Escape closes the calendar and puts back what the field held, and the focus returns to the button. The
  first arrow used to close the calendar and write a date the reader had not chosen, counted from today.
- **A history that could not be saved is on the screen.** When the history file could not be written - a
  session's record, a removal or a clear - the history section now opens and shows the reason. The section
  is folded, and its summary says only how many sessions it holds, so the reason stood behind the fold, and
  a clear or the removal of the last session that failed took the whole section, reason and all, off the
  screen.
- **The focus stays on the screen when the control holding it goes away.** Removing a calculator step
  with its own button puts the focus on the step now in its place, else the one before it, else Add step.
  Format rows an error takes off, and a Copy as command or Use this date an error switches off, hand it
  to the custom format. Each of them used to drop the focus on the window or the column, where the next
  Tab started again from the top.
- **A screen reader names every control the keyboard reaches.** The calculator's start point, time zone,
  step type, jump and nearest-day pickers, both search fields, the header of every folding section, and
  the time zone, speed, arguments and working folder fields were read as an unnamed "combo box" or
  "edit". The calculator's copy buttons are named after their format ("Copy ISO date"), its steps by
  number ("Step 2", "Remove step 2"), and the rows of the session history by everything they show, where
  two sessions of one program used to read the same.
- **Tab no longer stops on nothing.** Form rows, the session clocks, section summaries and lists of text
  each took a Tab stop with nothing to do there, which a screen reader could only call the window - four
  extra stops on the setup screen and five on the calculator.
- **The window no longer waits while the engine or the core starts.** The calculator starts the engine
  for every answer, and the window's own thread used to do the starting, so every edit paused it for as
  long as Windows took to start a program - longer on a machine whose antivirus scans the engine first.
  The engine and, at Start, the core are now started off the window's thread. Arrowing down the
  session's scenario list started the engine for every row it passed. A row chosen while the one
  before it is still being worked out now waits a fifth of a second, whether it was clicked or reached
  with the arrow keys, so a run of rows asks about the row it stops on. A row chosen when nothing is
  being worked out, such as the first click on the list, still asks at once.
- **A large process family no longer holds the window as its reports come in.** Every change on the
  session panel told all nineteen of its commands to check themselves again, and every coverage report
  rebuilt the audit lists even when it said nothing new. A family of 256 processes reporting the same
  channels took 275 ms of the window's time and now takes 37 ms. A heartbeat no longer re-announces the
  whole result either.
- **The copied session summary says what the CLI report says about errors, sleep and the browser.**
  Every error the core sent is listed under the verdict - a speed it refused while the session ran was
  on the screen only while the session lasted. How long the computer slept is in the session's block,
  and the result screen shows it beside the elapsed times. The browser a Chromium session drove is
  named under the application. The sleep warning is on the session's warning list as soon as the core
  notices the sleep, not only once the session ends.
- **The diagnostics block says which window, system and core a session ran with, and what failed.** It
  names the window's version, Windows, the core's version, bitness and path, whether the session drove
  an application or a Chromium browser, and that browser, and it carries the whole exception the window
  caught, with its stack. Every line of the core's output has its time since the session began. A core
  that could not be started at all now leaves a block too - it used to leave none.
- **A failed start writes the whole fault to the diagnostics folder and its box says where.** The box
  showed the exception's message and nothing was written. Two different unexpected faults thrown from
  the same place inside .NET are no longer taken for one, and faults past the eighth, which are no
  longer shown, are still written - up to 32 different ones a run, the last saying so.
- **A history this version cannot read is set aside with a word, and a killed write leaves no file
  for good.** The history panel says where the earlier history was moved when this version could not
  read it, instead of starting again with one row and saying nothing. A scratch file left by a write
  that was killed half-way is removed by the next write once it is ten minutes old. A history that was
  set aside is never removed.
- **"New session" comes back at the mode the last session started with.** Changing the speed while a
  session ran moved the Mode list with it, so a session started in flow left the next form at x60.
- **The "Jump to" field of a new session is empty.** It kept the moment typed in the last session,
  ready to jump the new one there.
- **A calculator answer counted from "Today" or "Now" is brought up to date before it is used.** It
  was computed once, so a "Today + 30 days" left open overnight went to the substitution panel and
  the clipboard as yesterday's answer. Showing the calculator again, "Use this date" and Copy now ask
  again when the day (Today) or the second (Now) has moved on, and wait for the new answer.
- **A calculator scenario that counts in a calendar this installation does not have is no longer
  computed in another one.** The window kept the calendar picked before, so a scenario for a market
  with no calendar here counted its business days in another country's, without a word. It now says
  which calendar the scenario needs and computes nothing. When the list of calendars cannot be read,
  the calendar field says why and offers no calendar.
- **A timer or a delay whose absolute due the session's clock has already passed fires at once.**
  `SetWaitableTimer`, `SetWaitableTimerEx`, `SetThreadpoolTimer` and `SetThreadpoolTimerEx` got the
  shortest relative due for it, which Windows fires only at the next tick of the system timer - up to
  15.6 ms of real time at the default timer resolution, 15.6 s of session time at x1000. An absolute
  `NtDelayExecution` deadline already reached waited for the same tick. Each now answers as it does
  without a session: the timers fire at once, and `NtDelayExecution` returns at once with the status of
  a deadline already past rather than the one for giving up the processor.
- **The WebView2 policy value of `--elevated-embedded` is always its own session's, also when the
  `*` value holds a session marker.** A session copies the `*` value into its own and puts its marker
  last, but the first marker was read. So a `*` value carrying one - a tester who copied a session's
  value there - made the session's value look like another core's: the session left it in the
  machine registry while saying it was removed, and another session's look for leftovers removed it
  while its session ran. A broken marker in `*` hid a dead session's value from every look after it.
  The last marker is read now.
- **A look for leftover policy values never removes `*` or the key's default value.** No session
  writes under either name, but a `*` value carrying the marker of a session that is gone was taken
  for that session's and removed.
- **A preset or calendar file is refused in the words of the file, not of the program reading it.**
  A rule `type` or a step name the reader does not know was an "unknown variant", and a rule `type`
  that is not text "expected variant identifier" - the names those things have in Rust. They are now
  an "unknown name" and a "name, written as text". A preset step that is not an object (`"steps": [5]`)
  or names two steps at once was refused with "expected value", the sentence for a file that is not
  JSON, so its author looked for a missing comma. It is now refused as what it is, with the step names
  it may use, and a second key is named.
- **A date that is not one is quoted as it was written.** A preset parameter's `default`, or a
  `--param` value, that is not a date (`2026-13-01`) was refused quoting `'2026-13-01T00:00:00'`, a
  text nobody wrote. The refusal quotes the date as given.
- **`chrono presets` names the step of a preset that does not resolve.** `--preset` has said "step 2:"
  for a step it cannot use, and the catalogue - the list the window shows its skipped files from -
  gave the same reason without the number. Both say it now.
- **A protocol line past the size limit is called too long, whatever character the limit cuts.** The
  core reads a command line of at most 1 MiB. A longer line whose limit fell inside a character of two
  or more bytes was refused as `protocol.stream_unreadable` ("not UTF-8 text"), which sent a client
  looking at its encoding when the length was what it got wrong. It is now `protocol.line_too_long`, as
  for any other line past the limit.
- **A pasted date with a sign in a number is refused.** `chrono calc --analyze "+1/1/2026"` read the
  date as January 1st, because the integer reader takes a leading `+`. A US or Polish date is now
  digits only in all three numbers, the rule an ISO date's fields have had since the analyser refused
  `2026-+1-05`, and the refusal says so ("bad number ... (digits only)").
- **A Chromium session that could not start says what it left behind.** When the browser never opened
  its debugging port, the session ended only the process it had started, tried once to remove the
  profile and reported `clean` whatever happened. It now ends everything the browser started and removes
  the profile the way the end of a session does, and a profile it cannot remove is named
  (`cleanup.chromium_profile_left`).
- **A process that refuses to be opened counts as running.** One that runs as administrator or as
  another user used to read as ended: the clean-up of old Chromium profiles could remove the profile of
  a live session, and a session took such a member of its application for closed. It now counts as
  running, and the session says `session.left_running` for it.
- **Looking for web engines inside an application goes on after a failed read.** One table of
  listening sockets that could not be read ended the search for the whole session, and took the other
  table with it. The two are read apart now and a failed read is tried again. The search gives up, and
  says so (`embedded.discovery_unavailable`), only when the IPv4 table, the one web engines open their
  debugging port on, could not be read several times in a row.
- **A Chromium session reaches the first window sooner.** It looks for the browser's debugging port
  every 10 ms instead of every 150 ms and connects by the address the browser writes beside the port,
  so the first window of an application is paused for the time shim more often.
- **Pages and workers the session could not reach are counted and named.** One the session could not
  attach to was skipped without a word, and why a time shim failed was dropped - both now count against
  the verdict, and the first three are named on the diagnostic stream. A connection that closes while the
  browser still runs says why.
- **A Chromium session lets go of what it does not drive.** A second debugging session on a page, a page
  the native mechanism covers and a target with no JavaScript are detached from instead of being kept
  for the whole session, and the session remembers a bounded number of the workers that came and went.
- **A process of the session that cannot be watched is named.** The session lasts while a process on
  its clock runs, and one that would not let Chrono Mock wait on it (access denied) used to count as
  ended, so the session could end while it still ran, without a word. The report now says so
  (`session.member_unwatchable`): from the end of the session such a process runs on the real clock.
- **Start-up refusals say what happened.** A program that asks to run as administrator, a path or
  command line that is too long, a program built for another processor, a file antivirus blocked or
  removed and a program a system policy blocks are named instead of shown as a bare number. A session
  block left by an application that another version of Chrono Mock started is told from one that does
  not belong to Chrono Mock at all, and says to close that version's applications. When the hook library
  will not load in the application, the report says whether Chrono Mock itself cannot open it or it is
  built for another machine. The error numbers stay in every message, and none of them is the
  system's own text, which came in the system's language.
- **A program the application starts under its own debugger no longer stalls the start for ten
  seconds or reads the real clock.** When the application under test started a program with
  `DEBUG_PROCESS` or `DEBUG_ONLY_THIS_PROCESS` (a launcher that debugs what it starts, say), Chrono Mock
  waited in the application's own thread for the hook to load in that program, while the program
  waited for that very thread to answer its first debug events. The start stood ten seconds, the
  program then began before the hook was in place and read the real clock, and the report named it
  uncovered although the hook joined it a moment later. The wait now happens in a thread of its own,
  the call returns at once, and the program starts once the hook is in place. A debugger that asks
  for the program suspended and resumes it itself can still start it before the hook is in.
- **A child process that Windows could not start is no longer reported as running on the real
  clock.** When the application under test started a program that Windows ended while loading it,
  before the program ran (a library it needs is missing, say), the report listed that program among the
  processes the hook could not follow, said it ran on the real clock and gave the session a `partial`
  verdict. It never ran, so it is left out now, the way Chrono Mock already read its own target.
- **On 32-bit Windows with `--scale-qpc`, the precise system clocks no longer follow the scaled
  counter.** Windows computes them through the exported `QueryPerformanceCounter` there, so after the
  session the application's precise wall clock stayed ahead of the real one by everything the counter had
  gained (200 s after a 3 s session at x60, measured), and its precise interrupt time ran off during the
  session too. 64-bit Windows was not affected.
- **An absolute `NtDelayExecution` under `--scale-duration` waits for the fake date and time.** It was
  passed to the kernel as it was, which compares it with the real clock.
- **A delay shorter than a millisecond is no longer lengthened.** `NtDelayExecution` with 100 ns, a common
  way to give up the processor, was held at 1 ms at every speed, x1 included, and reported as a wait the
  speed could not divide.
- **The fake elapsed time no longer turns negative** after about 10.7 days at the highest speed. It stands
  at its limit and `time.duration_axis_clamped` says so, which it now also does when the limit is reached
  over several speed changes.
- **A core stopped in the middle of a speed change costs a target one wait, not one per clock call.**
- **The 2038 marker no longer disappears after the year 30828.** It was counted through FILETIME,
  which ends there, while the calculator reaches the year 262143.
- **Business days on a calendar with many holidays, and long format masks, take less time.** A day off
  is now found by binary search: `+200000bd` on a calendar of 2,000 holidays went from 1.3 s to 0.86 s,
  measured. A format mask full of letters that are not tokens was checked in quadratic time: 30,000 of
  them went from 0.85 s to 0.06 s.
- **A flag that takes a value no longer swallows the flag after it.** `chrono run app.exe --report
  --dry-run` ran the application and wrote the evidence into a file named `--dry-run` instead of
  printing the plan, and `--report --no-embedded` dropped the opt-out. A word that starts with two
  dashes is now refused as the value of a flag, naming both. `--args` still takes flags, because they
  are the application's own, and a path that starts with two dashes can be written `.\--name`.
- **A window, sound or network library present at the start can no longer take the hook with it.**
  The hook keeps such a library loaded for as long as the application runs, as it already did for one
  loaded later, so an application that frees it cannot leave the report claiming a function whose
  hook went away with the library.

### Security

- **A calendar or preset file cannot hide text from the terminal.** A text or key in such a file that
  holds a control character, a format character (the ones that reverse the direction of text or have
  no width) or a line separator is now refused when the file is read, naming the place in the file,
  the position and the character. A preset whose description held a carriage return and an escape
  sequence printed a fake result line over the real one. These files are made to be shared, and none
  of the shipped ones holds such a character. A refusal that quotes such a file where the check cannot
  read all of it - a file that is not valid JSON, or one with a key given twice - shows the character
  as its code. The same characters in what an application or a browser says about itself - a process
  name, an error, a refused connection - are now shown escaped.
- **`--report` cannot overwrite a program, and writes its file whole.** The path is checked before the
  session starts, also in a dry run: a folder that does not exist, a folder, the application itself and
  any other program file are refused with exit code 1. The evidence is written to a temporary file
  beside the target name and then renamed over it, so a failure leaves no half-written file, and when
  the old file cannot be replaced the message names the temporary one that holds the evidence. The
  temporary file is always created new, never written through one that is already there - a
  temporary file an earlier run left with its evidence, or a link.
- **A core in the window's package reads only the package's calendars and presets.** Run on its own
  from `core\x64` or `core\x86`, it found no catalogue beside it and read the one in the current folder,
  so a calendar placed in a shared folder could answer business-day questions in a report. It now reads
  the package's own folders and never the current folder's. The window was not affected.
- **A catalogue file is read with a bound.** Its size and its contents come through one open file, and
  the read stops past the 4 MiB limit even when the file grows while it is read.
- **The window package carries .NET 10.0.13.** It is the .NET security release of 2026-10-13, and
  the window, which brings its own runtime, bundles it in place of 10.0.12. The command line does not
  run on .NET and is unchanged.

## [0.4.0] - 2026-10-07

### Added

- **A Windows installer.** Each release now also carries `ChronoMock-app-win-x64.msi`, signed like the
  programs inside it. It installs the window and the command line for every account on the machine,
  into `Program Files\Chrono Mock`: the window in the Start menu, and the 64-bit `chrono` on the
  machine's PATH (the 32-bit one is `core\x86\chrono.exe` in the same folder). Installing needs
  administrator rights, running does not. An installed window keeps its session history and
  diagnostics logs in `%LOCALAPPDATA%\ChronoMock`, also when it runs as administrator, and
  uninstalling (Settings, Apps) removes the program, the shortcut and the PATH entry and leaves that
  folder alone. A newer version installs over an older one, and an older one is refused over a newer.
  The installer closes nothing that is running - a file still held by a running program is set aside,
  the new one is in place at once, the old copy goes at the next restart, and the installer says so
  with exit code 3010. The two zips stay as they were, and a release candidate gets no installer.
- **A preset names its parameters for the window.** A parameter in a preset file can carry a
  `label`, one text per language like `name`, with `en` required when it is there. The window shows
  it above the parameter's input, in its own language or else in English, and shows the id only for a
  parameter the file does not name. The command line keeps the id, which is what `--param` takes, and
  `chrono presets --json` hands each parameter's `label` on, empty when the file names none. Every
  parameter of the shipped presets now has an English and a Polish label - the window used to show
  `trial length` above the field, in every language.
- **`chrono presets` lists the preset catalogue as `--preset` reads it.** Every preset file in the
  folder comes back either as a preset, written in the words the calculator's grammar reads, or as
  a file left out, with the reason `--preset` would give for it. `--json` writes the schema
  `chronomock.presets/1`, and `--dir` names another folder. A file is listed exactly when
  `chrono calc --preset` and `chrono run --preset` accept it. The window now takes its scenarios and
  its calculator presets from this command instead of reading the files itself.
- **Timers keep the session speed in a hidden window, when asked.** A Chromium engine runs the timers
  of a minimized or hidden window about once a second, so with time sped up a hidden window lost the
  speed-up - measured at x60, a page's timer ran about 16 times in 15 seconds once its window was
  minimized, against 938 times with the slowdown switched off. `chrono run --keep-background-timers`
  and the option "Keep timers at speed in hidden windows" in the window start the web engine with
  that slowdown switched off: the browser of a Chromium or Electron application, and the engine
  inside an application (WebView2 and Qt WebEngine, through their own variables). Off by default,
  because the application then also works at full pace while it is hidden. Without it, a page that
  was hidden while time ran faster is reported (`chromium.background_timers_slowed`) - also for an
  application that switched the slowdown off itself, which the session cannot tell, so the report
  says the timers may have run slower. An application started as administrator through
  `--elevated-embedded` does not get the switches, and its hidden pages are reported the same way.
  The wire carries the option as `start.target.keep_background_timers`, and `--dry-run --json` as
  `session.keep_background_timers`.
- **A caution for the part of an Electron application outside its windows.** Its windows run on the
  session clock, but its main process runs JavaScript in Node, which the session does not reach, and
  reads the real date and time. The report now says so for every Electron application
  (`chromium.main_process_uncovered`). The verdict still speaks of the pages the audit sees.

- **The .NET timing caution now reaches .NET applications that leave no runtime files beside
  them.** An application published as NativeAOT or as a single file (self-contained or not), a
  .NET Framework application, whose runtime lives in the Windows directory, and one started as
  `dotnet app.dll` have no .NET runtime files beside the executable. Those files were how the
  session recognised .NET, so none of these got the caution that a `Stopwatch` timer does not follow
  the session speed. The session now also reads the marks .NET leaves inside the executable itself -
  the runtime's own exports, the signature every .NET launcher carries, the header of a .NET
  Framework program and the module header every NativeAOT program starts from - and recognises the
  `dotnet` host by name. One build is still not recognised: a NativeAOT application published with
  debugger support turned off that also exports functions of its own.
- **A caution when the application was built with Go.** The Go runtime reads the date straight
  from shared system memory rather than asking Windows for it, so a Go application sees the real
  date and nothing this tool can do will change that. The session time zone does reach it, which
  makes the result confusing rather than merely incomplete - the real moment, shown in the
  session's zone. The session now says so instead of reporting a clean success over it. Of the
  runtimes this tool supports, Go is the only one the wall clock cannot reach.
- **A caution when the application never read the clock the session replaces.** The verdict
  answers whether the substitution took effect, and it can say it did over an application that
  read the real date from start to finish - because the channels were hooked, which is a
  different thing from the application asking for them. A session where no process ever read a
  substituted clock now says so in as many words, beside the verdict rather than instead of it.
  It does not change the verdict or the exit code: what the session achieved and what the
  application used are two answers, and a tester needs both.
- **The total beside the channel list.** The list of covered channels now carries how many times
  the whole family read them, so the one number worth seeing first does not have to be counted
  off forty-one rows by eye.
- **A caution about network timeouts under sped-up timers.** A consequence of the sped-up timers
  fix below: a network library that measures its own timeout from the tick count, WinHTTP for one,
  now counts that timeout in session time, while the network wait underneath stays real. At x60 a
  60 second timeout runs out after one real second, so a server slower than that makes the request
  fail. A session with timers sped up now says so whenever the application opened a network
  connection, right under the line saying that waits on system objects stay on the real clock.
- **Reaching the web pages of an application that runs as administrator (`--elevated-embedded`, off
  by default).** WebView2 ignores the setting the session reaches its engine through when the
  application is elevated, so such pages stayed on the real clock. With this option, started from a
  Chrono Mock that runs as administrator too, the session writes one WebView2 value under
  `HKLM\SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments`, named after the
  application's file, for the length of the session and removes it before the verdict. It is the one
  thing in this tool that writes the registry, so the cost is spelled out beside the option: while the
  application runs, any program on this computer can reach its pages through the debugging port, and
  the application has administrator rights. A value of yours under the same name is never
  overwritten, and a `*` value of yours is carried into the session's. A session that ends abruptly
  leaves a value carrying the marker of its dead owner, which the next session reports and the next
  one started with this option removes. The report names every step - removed, left behind,
  recovered, leftover, foreign, not written - and puts a value that could not be removed first among
  the warnings. The window has the option as a checkbox under "Reach the web pages inside the
  application", greyed out with the reason unless the window runs as administrator. A script as the
  target is not covered, and a launcher whose WebView2 application is another program is reported
  with the name of the program to start. With or without the option, the session now follows the
  process tree under an elevated application: the engine's processes are named among those that ran
  on the real clock, so the verdict is as careful as for an ordinary application, and an engine port
  you opened yourself in the registry is reached. The wire carries the option as
  `start.target.elevated_embedded`, and `--dry-run --json` as `session.elevated_embedded` (false for
  a Chromium target, where it does not apply) and `session.elevated_embedded_value`, the file name the
  value would be named after when the option applies to an `.exe` target, null otherwise, as for a
  script. A name there is not a promise of a write: the session writes the value only when Chrono Mock
  itself runs as administrator.

### Changed

- **A preset or a calendar file that cannot be read says what is wrong in the file's own words.** A
  value of the wrong JSON type used to be refused with the name of a type inside the tool
  (`expected struct PresetTextDto`, `untagged enum BaseDto`, `expected u32`), which told whoever wrote
  the file nothing. The refusal now names the shape the file needs, and the place comes first:
  `bad preset JSON at line 7, column 67: invalid type: string "Start", expected a text in each
  language - an object like {"en": "...", "pl": "..."} with en required`. The same holds for
  `chrono calc`, `chrono run --preset`, `chrono presets` and the window's list of files it left out.
- **The window speaks of the application, the fake clock and Chrono Mock.** Its texts no longer use
  the words the code and the command line use for the parts of the tool: "the core" became Chrono
  Mock, "the target" the application, "the substitution" the fake clock, and "the moment" the date
  and time. A message about a fault inside Chrono Mock now says that it is one and how to report it,
  and a message about a mixed-up package says to extract it again. The command line, its report and
  the JSON are unchanged.
- **A holiday calendar says which years it answers for, and counts no business day outside them.**
  A calendar file now has three more required fields: `valid_from`, the first year every rule in it
  has a source for (holidays, weekend and observance), `law_as_of`, the day its holidays were checked
  against the law, and `source`, where the range, the weekend and the observance come from. The
  shipped calendars answer from 2002 (`pl`, the first full year of the five-day working week in the
  Polish Labour Code), from 1960 (`us-federal`, the first full year of both observance rules) and
  from 2008 (`us-banking`, the oldest Federal Reserve holiday schedule read). A business-day step or
  a `--nearest` business day that reaches a day before that year is refused with exit 1
  (`calc.before_calendar`). A date before it keeps its formats and its other marks, and its business
  day and holiday are not judged: the text says so, the JSON has `business_day` and `holiday` null,
  and the marks hold `before_calendar`. The JSON gives `metadata.calendar_valid_from` and
  `metadata.calendar_law_as_of`. A calendar file written for an earlier version is refused with the
  missing field named, and an earlier version refuses a calendar file from this one.
- **A calendar older than a year says that a holiday may be missing.** When a calendar was checked
  against the law more than a year before today, and it judged a later day - the result, or a day a
  business-day step walked over - the result carries the mark `calendar_outdated`, and `chrono run
  --preset` says it on stderr. A package updated in one year and used two years later would
  otherwise count on as if no holiday had been added since. The text output names the day of the
  check under the marks of every calendar read from a file.
- **`chrono calc --preset` counts business days in the calendar of the preset's market**, as
  `chrono run --preset` does, and names that calendar above the result. A market preset
  such as `payment-due-business-days` used to stop with exit 5 and ask for `--calendar`, while the
  calculator window counted the same preset in the market's calendar. A `--calendar` given on the
  command line still wins.
- **A preset file whose `applies_to` is not `calculator`, `substitution` or `both` is refused when
  it is read**, with exit 1 and the path of the file. Each command used to refuse it on its own, in
  its own words, and only when it was used.
- **The pages of a Chromium or Electron application, and the web pages inside an application, read
  the session's time zone.** They read this machine's zone before, so a local time they showed could
  be an hour or more away from the session's - also when the session ran in this machine's own zone,
  because the session's zone is a fixed offset and the machine's moves with daylight saving: a January
  date showed an hour off in summer, and nothing said so. Each page and worker is now put on the
  session's offset through the engine's own zone override, and checks the offset it reads. A page that
  read another zone at some point is reported (`chromium.zone_is_host`, and `embedded.zone_is_host`,
  which used to compare today's offsets and is now said only when a page read another zone). When a
  session ends and the application lives on, its pages are put back on this machine's own time zone,
  as the system names it, so a date on either side of daylight saving shows this machine's offset
  again. Each page is then asked what it shows, because an engine that does not know the name keeps
  the page where it was: a page that still shows the session's zone, or does not answer, is reported
  (`embedded.zone_kept`) - also when the system names no zone, and only then.
- **A console application's output goes to `chrono run`'s standard error.** The application writes
  its output and errors there, and reads the terminal's input when `chrono run` runs in a terminal
  with a window, or empty input otherwise, as in CI. Standard output carries the report and nothing
  else, and with `--json` it is a clean stream of protocol events. The application's output used to
  go into the channel between `chrono run` and the session, where the report lost it and `--json`
  mixed it in with the events (see Fixed). An application still running when the session ends
  keeps writing there, so a script or a CI step that reads `chrono run`'s standard error to its end
  waits until the application exits, as it did before this change. `chrono run` now says so when it
  happens, where the wait used to look like the tool hanging. An application with a window of its
  own still starts without standard handles, as it does from a terminal.
- **Started from the window, a console application opens in a console window of its own**, with
  its output and its input there, as when it is started by hand. A short-lived one closes the
  window as it exits. An application with a window of its own is not affected. The machine
  protocol's `start` carries this as `target.console`: `new` for a console of its own, or `shared`,
  the default, for the console and standard error of the process running the session.
- **An idle `chrono run` now says the core "sent no event" for 15 seconds**, where it said "sent
  nothing", because a line that is not an event no longer counts as a sign of life.
- **A calendar file with a field Chrono Mock does not know is refused instead of read around.** A
  misspelt optional field, such as `valid_form` for `valid_from`, used to be skipped, so the holiday
  it was meant to limit counted in every year. The file is now refused with exit 1, and the message
  names the field and the fields that can stand there. This holds at every level of the file: the
  calendar itself, a holiday, its name and its rule. Calendars therefore no longer follow the rule
  that a reader ignores fields it does not know, and a field added to the calendar format later will
  need a Chrono Mock that knows it. A file written to a later schema version is still refused as
  that, not over its new fields. Presets keep ignoring fields they do not read.
- **An Easter-based holiday has to fall in the year of its Easter.** A calendar's `easter_offset` is
  accepted from -80 to 250 days, the range that keeps the holiday between 1 January and 31 December
  of its own Easter's year in every year. The old range of a year either side let a holiday fall into
  the year before or after, where one date was reported both as not a holiday and as a holiday moved
  there from a weekend. The shipped calendars use 1, 49 and 60.
- **A calendar or preset whose `id` is not its file name is refused.** `other.json` declaring the id
  `pl` used to load as `--calendar other` and sign its answers "(pl)". `chrono calc` and
  `chrono run` refuse such a file with exit 1 and name both, and the window leaves such a preset out
  of its list, as it leaves out any preset file it cannot use. Case does not count, as it does not
  for the file name on Windows.
- **The core checks a Chromium or Electron start the way it checks a native one.** A mistyped time
  mode used to run as flow at real speed, a speed outside 0 to 1,000,000 was accepted or turned into
  x1, and a speed of 0 ran at x1 where a native session froze. The same start is now refused with the
  same key as a native one, and 0 freezes the clock in both. A protocol `start` without
  `tz_bias_min` is refused with `time.bad_zone` instead of being read as UTC, and an absolute `jump`
  without one is read in the session zone rather than in UTC. Chrono Mock's own window and command
  line always send the zone, so only other clients of the protocol see this.
- **`chrono run` refuses a change it would never make, and a jump the core would refuse.** A
  `--set-after` or `--jump-after` at heartbeat 0, or past the last heartbeat `--ticks` allows, never
  happened, while `--dry-run` promised it. Both are refused now with exit 1. The moment of a
  `--jump-after` is checked before anything starts: a mistyped moment, a step in business days (a
  session has no calendar) and an absolute moment outside the years 1601 to 30828 used to run the
  session and have the jump refused part-way through. They now stop the run with exit 1, and
  `--dry-run` says the same.
- **`chrono run --preset` counts business days in the calendar of the preset's market**, as the
  scenario list in the window does (`us`: US banking, `pl`: Poland). It used to refuse such a preset
  and point to a `--calendar` option that `run` does not have. A preset with no market still has no
  calendar to count in and is refused with exit 5, with a message that no longer mentions
  `--calendar`.
- **`chrono calc` refuses arguments it used to drop without a word.** A flag given twice
  (`--zone`, `--calendar`, `--preset`, `--format`, `--analyze`, or `--param` with the same name)
  kept the last value and forgot the first. `--analyze` answered as if a step flag or `--format`
  beside it were not there. A flag standing where a value belongs was taken as the value, so
  `--format --json` printed text with `--json` as the mask. Each is now refused with exit 1. A mask
  that has to begin with two dashes quotes them, as in `'--'yyyy`.
- **A refused `chrono calc` argument ends in a key, `calc.bad_argument`**, like the calculator's
  other refusals, and with `--json` the usage no longer follows it. The calculator window showed
  that whole usage under its message, and now says which step values it could not read.

### Fixed

- **The readme inside each zip tells a tester to exclude the library file from Defender, not the
  folder.** The repository README and the installer's readme already said to add a File exclusion
  for `chrono_hook.dll`, because a Folder exclusion leaves unscanned the very folder the tool
  injects from. The readmes in `ChronoMock-app-win-x64.zip` and `ChronoMock-cli-win.zip` still said
  to exclude the folder, so the same product gave opposite advice depending on the download. Both now
  name the file (`core\x64\chrono_hook.dll` in the window's package, `chrono_hook.dll` in the
  command-line one, plus the `x86` copy) and keep the folder as the fallback.
- **"90 business days" reads in full.** The unit list had a fixed width that cut the last letter
  of its longest unit, so the builder and a scenario's parameter showed "90 business day" at every
  window size. The list is now as wide as its longest unit in the window's language, whichever unit
  is chosen.
- **A value under "All formats" stays on one line.** At the window's default size ISO datetime,
  FILETIME and RFC 1123 broke in the middle of the value - "2026-01-01T00" over ":00:00+00:00" - so
  a value meant to be copied read as two. Each format's name now stands over its value.
- **The list of recent applications says again which file is gone, and where each one is.** A file
  that no longer exists is marked "(missing)" in the open list and in the closed box, and each entry
  shows its folder after its name in the open list, because two builds of one application share a
  name. Both were lost when the window moved to three phases. A screen reader hears the mark too.
- **A long scenario name wraps instead of being cut.** At the window's smallest width "Payment due,
  +N business days" lost its end, and its tooltip gave the description, not the name.
- **The Polish name of UTC fits the zone box.** "Uniwersalny czas koordynowany" was cut in the
  closed zone box, in the run panel at every window size. It reads "Czas uniwersalny" now.
- **Every part of an amount has a name for a screen reader.** The sign, the amount and the unit of a
  shift step in the calculator had none, and the unit of a scenario's parameter had none.
- **A scenario's parameters fit the calculator at the window's smallest size.** At the minimum width
  the parameter row ran past the edge of the result card: the unit list lost its arrow and the date
  its calendar button. A parameter's label now stands above its input, as in the builder beside it,
  and the amount and the unit are as wide there as in the builder and the run panel.
- **The calculator no longer asks to fill in the parameters "below"** while they stand above the
  note.
- **Dropping a script on the window no longer says that it cannot be run.** The window still takes
  only `.exe` files, and its message now says so and that a script (`.bat`, `.cmd`) runs with the
  `chrono` command line. It used to say that a clock cannot be attached to a script, which stopped
  being true when the command line learned to run scripts.
- **Old dates are no longer counted with today's holidays.** The Polish calendar answered for every
  year with today's list: 3 May came out a day off before 1990, 11 November and 15 August before
  1989, 22 July (a day off until 1989) never, and Saturday a weekend day in years when it was a
  working day. The American calendars put Thanksgiving on the fourth Thursday also before 1942. Those
  years are now outside the calendars, so they are refused or not judged instead of answered wrongly.
- **12 November 2018 is a day off in the Polish calendar.** It was made one by its own act, and the
  calendar counted it as a working day.
- **The window reads every preset exactly as the command line does.** It read the preset files
  with a grammar of its own, and where the command line refused, the window made a value up: a
  default of one month counted as one day, a duration or a choice with no default was filled in
  with one day or "day before", a date default was left out, a step the command line refuses was
  computed, and a preset with no parameters asked for them. A file saved as UTF-16, or named with a
  space, was listed while `--preset` refused it. The window now gets the list from the engine, so
  it lists what `--preset` accepts and nothing else, an input with no default stays empty until it
  is filled in, and a preset the window cannot show says so instead of asking for parameters. The
  files the engine left out are counted under the list, each with its reason in the tooltip, and a
  list that could not be read says why, where it used to stay empty without a word.
- **"Analyze a pasted date" shows the time of a pasted number, and the zone it is shown in.** A
  number of seconds or milliseconds since 1970 is a moment, and the window showed its date alone,
  under a label that was a raw key, read in this machine's zone without saying so. The reading now
  carries its time, a line above it names the zone, and that zone is the one chosen for the
  calculator, as `--zone` is for `chrono calc --analyze`. Changing the zone reads the number again.
  Each reading in the JSON analysis says whether it is such a moment (`instant`) and which time it
  shows (`time`).
- **The session sends far fewer HTTP requests to the application's own servers.** While looking
  for a web engine inside the application, the session asked every local port the application
  listened on whether it was a debugging endpoint - up to four HTTP requests to each, also to a server
  that speaks another protocol and may take such a request badly. Only the port Qt WebEngine was told
  to open and the processes that can be a Chromium engine are asked now. In a process with Qt
  WebEngine loaded only the port its engine was told is asked, and its other ports only once its
  engine runs without that port, because the application chose a port of its own. A process beside
  the Chromium runtime - an application built on CEF among them - still has every local port asked,
  so a server inside it can still get the request, and no process is asked again once its debugging
  endpoint is found. A process whose executable the system will not name is still asked, so a web
  engine is not missed for that.
- **A session no longer says a Qt WebEngine port was taken in an application without Qt WebEngine.**
  Every session sets a port aside for Qt WebEngine, and when another program on the machine started
  listening on it, the report said the application's web pages could not be reached and ran on the
  real clock (`embedded.qt_port_taken`), also for an application that has no web engine at all. It is
  said now only when a process of the application has Qt WebEngine loaded, and about the port its
  engine was actually told, which is the tester's own when they set one.
- **A Chromium or Electron application started through a launcher no longer ends its session at
  once.** A target that starts the browser and exits - a script beside the application, or a
  Chromium browser started from its runtime folder, which hands over to a process of its own - ended
  the session within a second, with no verdict, and the session's cleanup closed the browser it had
  handed the application to. The session now lasts as long as the browser's debugging connection,
  and the report says that the target handed the application over and names the program the session
  went on with (`chromium.followed_browser`, `session_verdict.followed`). `--dry-run` says how a
  Chromium session ends instead of promising the rule of the native mechanism.
- **Closing the window during a session no longer freezes it for three seconds, and the session is
  recorded.** The window waited for the session to end on the same thread the end needed, so every
  close during a session took three seconds and the application quit before the session reached the
  history or its diagnostics were saved. Closing now ends the session the way Stop does - the panel
  says "Stopping" - and the window closes as soon as the session is over, after ten seconds at most.
  A session still starting when the window closes ends before the application is launched. A close
  with no session running is not delayed. A session cut short by Windows logging off or shutting down
  is still not recorded, because the window is not told before it goes.
- **A portable package whose x64 core is missing starts and says what is wrong.** The window decided
  it ran from the package by finding its own core, so a core removed by an antivirus or missing from
  a partial extraction made it look for a source checkout and stop at start with "could not find the
  repo root". The package now knows its layout from the build. A missing core makes Start say that the
  installation may be incomplete, with the path, and the calculator says the same instead of a raw
  error. Start no longer blames the application when the core disappears while the window is open.
- **Dragging an application over the window no longer freezes it when the file is on a network
  drive that has gone away.** The window checked that the file existed on every mouse move of the
  drag, and Start read the application's header on the window's own thread, so each check waited for
  the drive to time out - about 20 seconds per check here. Both now happen in the background, and the
  file is checked once, when it is dropped.
- **The session history survives a file held by another program, and a damaged entry no longer stops
  the window from starting.** A virus scanner or a second Chrono Mock holding the history file when a
  session ended made the window move the whole history aside and start a new one with a single
  session. It now waits briefly and, if the file stays locked, says the session was not recorded. A
  history file with an empty entry (`"sessions": null`, a `null` row, a row with a text set to
  `null`) made the window fail at start or later, and is now treated like a history this version
  cannot read: an empty list, and the file moved aside rather than written over. Clearing the history
  moves such a file aside as well, rather than deleting sessions the window never showed. Removing a
  row or clearing the history no longer runs on the window's thread, and no longer drops a session
  another instance recorded at the same moment. Two instances started together no longer keep their
  history in two different folders, and their diagnostics files no longer overwrite each other.
- **One preset file the current user may not read no longer hides the presets listed after it.**
- **The About window lists the components even when the core writes a lot of diagnostics.** A core
  that wrote more than a few kilobytes of diagnostics stalled until the window gave up and said the
  core could not be asked.
- **"Use this date" and Copy in the calculator act on the result for what the builder shows, not
  the one from before the last edit.** Pressing a button takes focus from the field being edited,
  which commits the edit and only schedules its result, so a press straight after typing sent or
  copied the result from before the edit. A press now waits for the result of the current input -
  normally well under a second - and acts on that, or does nothing when that result is an error.
  While a newer result is being computed the result column fades a little, so the old date is no
  longer passed off as the answer to the new input. The format rows are updated in place, so
  keyboard focus on a Copy button survives a new result.
- **A failed calculation no longer leaves the previous result on screen.** The old date, formats and
  metadata stayed visible under the error, and Copy copied them. A preset that is still waiting for a
  parameter now shows no result either, with "Use this date" off until the parameter is filled. The
  month-length note and the custom-format warning are cleared with the rest of the result.
- **The scenario list and "Relative to now" no longer overwrite a later choice.** A scenario's date is
  computed in the background, and an answer that came late filled the field anyway: arrowing from one
  scenario to the next could leave the first one's date beside the second one's name, and a date typed
  while a scenario was being computed was overwritten. An answer now fills the field only while it
  answers the latest request, and nothing overwrites a date typed by hand. Start pressed straight
  after choosing a scenario waits for its date instead of starting with the one before it, and when
  that date takes longer than 15 seconds to come, Start does not go ahead with the old one: it says
  the date is still being computed and is available again the moment the date arrives.
- **Start refuses when the chosen scenario gave no date.** The field still held the date from before
  the choice, so the session started with a date nobody chose. The reason is shown beside Start, the
  line above it no longer promises that old date to the application, and typing a date or choosing
  another scenario clears both.
- **Changing the session zone recomputes the chosen scenario.** A scenario is computed in the session
  zone, so after a zone change the field kept a wall clock that names a different instant in the new
  zone - the 2038 boundary chosen in UTC and then read at UTC+01:00 was an hour short of it - while the
  scenario still showed as chosen.
- **"Analyze a pasted date" reads a pasted date at once and keeps the engine's explanation.** The field took
  its text only when focus left it, so a pasted date was not analysed until something else was
  clicked. It now follows typing after a short pause. A failed analysis also clears the readings of the
  previous date and shows the engine's own sentence under the message, as the result column does.
- **Copy is off for a format the date falls outside.** Such a row shows that it is out of range, and
  Copy used to put that text on the clipboard as if it were the value.
- **A broken translation file no longer breaks a calculation.** A template naming a value that does
  not exist made the result fail halfway through being shown. Such a template now falls back to the
  English text, with the values in it, and only shows as written when that cannot be filled either.
- **A busy web page no longer stops a Chromium or Electron session.** Once a second the session
  asked every page and worker for its call counts and waited for each answer in turn, up to ten
  seconds each. A page busy with its own work - a long script, an `alert` - does not answer, so every
  busy page or worker added ten seconds of silence, and a client that hears nothing for fifteen
  seconds stops the session. Measured on an Electron page: a busy page kept the session silent for
  10.5 seconds at a time, and with two busy workers beside it the session was stopped with no
  verdict. The pages are now asked all at once and their answers are taken as they come: counts are
  merged on arrival, a page still answering the last question is not asked again, and a speed change
  or a jump waits at most two seconds for the pages before it is acknowledged - a page that takes it
  later still gets it, and whether it missed the change is read from what it answered. The pages of
  an application that embeds a web engine had the same wait, two seconds a page, and it also delayed
  the session's look for new child processes.
- **The connection to a browser no longer reads with a timeout.** It waited for messages by letting a
  read time out every half second, or every 10 ms beside a native session, and a read that times out
  leaves a Windows socket in a state it should not be used in again. Measured on loopback with a 1 ms
  timeout: in ten minutes, 39 timed-out reads ended in an error on the next read and lost the bytes
  they had taken, and the session took such an error for the application closing. A thread of its
  own now reads the connection with no timeout, and connecting, `/json/version` and the WebSocket
  upgrade have one deadline for the whole exchange instead of ten seconds for every read.
- **A page that is already loading when a Chromium or Electron session reaches it now gets the
  session clock ahead of its own scripts.** The browser can answer the session only once the
  application's first window is loading, and that page is then reached halfway through, when
  nothing can hold it any more. The session sent it three setup commands one after another, each
  after the answer to the one before, and the page's startup scripts could run in between: a page
  that reads the date once at start kept the real one for the whole session, and the verdict was
  "undetermined". Measured on an Electron application, about one session in ten ended that way. The
  three commands now go out together, and in every measured run the page reached halfway through
  took the clock before its startup scripts ran.
- **A speed change no longer moves a page's clock backwards.** In a Chromium or Electron session, a
  speed change took effect in the window's clock at once and in each page only when the change
  reached it, so the page's clock jumped at that moment by the delay times the change in speed -
  backwards when slowing down. Measured on an Electron page busy in 5 ms slices, x1440 to x1 put the
  page's clock back by 18.7 seconds. A speed change now takes effect a quarter of a second after it is
  asked for, at the same instant in the window and in every page, and each page takes it over from
  where its own clock stands, so neither the date nor `performance.now` steps. The same page now gets
  the change with about 0.24 seconds to spare. A page too busy to get the change within that quarter
  of a second takes it when it gets it: its clock stays continuous, but runs that delay times the
  change in speed away from the window's until the next jump. The report says so with a new warning,
  `chromium.clock_move_missed`, which also names a page that did not take a speed change or a jump
  at all, or did not take the clock its next document starts from. The speed a `state` event reports is the
  one asked for, from the moment it is asked for. The pages inside an application follow the
  application's clock as before, which changes at the moment of the command.
- **A page that reloads after a jump or a speed change keeps the session clock.** In a Chromium or
  Electron session, and in the pages of an embedded web engine, every new document starts with a
  script that carries the clock, and that script still carried the clock from the moment the session
  first reached the page. Measured on an Electron page: after a jump to 2031 and a change to x1, a
  reload brought the page back to 2038 at x60. Every jump and speed change now replaces that script
  as well, so the reloaded page stays in 2031 at x1. The script is also taken away when the session
  lets the pages of an application that outlives it go, so a page that loads a new document in the
  session's last moments starts on the real clock. A script the page did not confirm removed is tried
  again at every later move and at the end, and a page that still has one when the session lets it
  go is named among the pages that did not confirm they were let go.
- **A CDP session survives a message it cannot read, and no page or worker is left paused.** A
  Chromium or Electron session, and the bridge to the pages inside an application, ended the whole
  connection on one message that was not valid JSON - which Chromium sends for a JavaScript string cut
  in the middle of an emoji, such as a window name - and a Chromium session then reported the
  application closed. Such a message is now repaired (the broken character becomes U+FFFD) or, failing
  that, skipped with a single notice, and the session reads on. The events that attach a new page or
  worker are no longer dropped from the queue while anything else is in it: dropping one, which could
  happen only to a target that sent over ten thousand events while a command was waiting, left the
  page or worker paused for good. A queue of nothing but such events keeps twice as many, and if
  even that fills up, the session says it is dropping them. Every message already received is now handled in the same turn, where one per turn was
  handled before, so a worker started during a busy page load is released sooner. A debug port that
  answers on IPv6 (`::1`) is reached as well - the request named the host in a form the browser and
  the resolver both rejected.
- **A page's own `Date` behaves like the browser's, and its reads are counted.** In a Chromium or
  Electron session, and in the pages inside an application, the replacement for `Date` broke a class
  extending it: `new X()` came back as a plain `Date`, so a library built on such a class lost its
  methods and its own `instanceof`. `Date()` without `new`, `new date.constructor()` and
  `Intl.DateTimeFormat` formatting "now" still read the real clock, and `Date.name` read as something
  else than `Date`. Measured on an Electron page, all of them now read the session clock, a subclass
  stays itself, and the name and arity are the native ones. A worker started by another worker gets
  the session clock too (it read the real one), and `performance.now` in a page that was already
  running when the session reached it goes on from where it stood instead of starting again from 0.
  The report counts the time read through `new Date` or `Date()` and through `Intl.DateTimeFormat` on
  rows of their own: an application that read the time only that way was reported as having called no
  time API at all. A page or a worker that refuses to hand over the workers it starts no longer counts
  as fully covered: such a worker would run on the real clock unseen, so the verdict says some
  contexts could not be reached.
- **A child started past kernel32 now runs on the session clock.** The session followed children
  started through kernel32's `CreateProcessW` and `CreateProcessA`. A child started through
  kernelbase's own `CreateProcessW` or `CreateProcessA` - which is what code importing through the
  Windows API sets calls - through `WinExec` or through `CreateProcessInternalW` ran on the real clock,
  and the session reported it as a child it did not cover. Measured on x64 and x86, all of them are
  now followed, and the ways that already were (kernel32, the C runtime's `system`, `_wspawnv` and
  `_popen`, `ShellExecuteEx`) still are, each child once. A child started under another user token
  (`CreateProcessAsUserW`) is left as before: named in the report, not followed.
- **A `Sleep` made inside an APC is scaled, and every wait made there is counted.** Windows runs an asynchronous procedure call -
  the completion routine of overlapped I/O or of a waitable timer, or one queued with `QueueUserAPC` -
  while the thread sits in an alertable wait such as `SleepEx(..., TRUE)`. A `Sleep` made inside one
  ran at its real length under `--scale-duration` and was left out of the audit, because the session
  took it for Windows' own work inside the outer wait: measured at x60, a `Sleep(1200)` there took 1.2 s,
  on x64 and x86 alike. An object wait made there, such as `WaitForSingleObject`, went uncounted too.
  And an exception that left the APC for a handler outside the wait left every later `Sleep` of that
  thread at its real length and uncounted, for good. All three now behave like any other call: the same
  `Sleep` takes 20-35 ms at x60, each wait is counted once (an object wait keeps its real timeout, as
  everywhere else), and a wait Windows makes inside another on its own is still not counted twice.
- **The duration clocks no longer step back when the speed changes or the core stops.** Under
  `--scale-duration` and `--scale-qpc`, a speed change could answer one read from the old speed and the
  next from the new one for an overlapping moment, and the later read came back lower. Measured over
  20 s of changes between x1440 and x1, `QueryPerformanceCounter` stepped back 1,627 times on x64 and
  2,149 times on x86, by up to 0.18 s, and an earlier run caught the tick count stepping back 1.4 s. A
  core that died at x1440 sent `QueryPerformanceCounter` back by up to about a millisecond in every
  thread that was reading it as the application was let go. Both are gone, 0 in the same runs: a speed
  change reads its instant inside its own write, every clock read takes the real clock inside the same
  window as the anchor it projects from, and an application a dead core left behind keeps the session's
  speed for 100 ms more before it carries on at the real speed from where its clocks stood. The tick
  count is now derived from `QueryUnbiasedInterruptTime`, so it no longer falls behind by up to a
  millisecond at every speed change. The cost, measured: about 7 ns more per `QueryPerformanceCounter`
  call under `--scale-qpc` on x64 and 11 ns on x86, nothing measurable on the wall clock.
- **An application that cannot load is named for what it lacks, at once.** An application whose
  library was missing, built for the other bitness, without a function it needs, or refusing to
  initialise ended before it ran, and the session reported a single-instance application that
  vanished (exit 12) - or, started from the window or from Explorer, waited ten seconds on Windows'
  error window, reported a loader lock and left the window on the desktop. It now ends with a key that
  says which (`target.loader_dll_not_found`, `target.loader_entry_missing`, `target.loader_bad_image`,
  `target.loader_init_failed`, or `target.died_while_loading` with the code it ended with), exit 2.
  Windows' error window is kept off while the application loads and switched back before its entry
  point, so the application runs with the error mode it inherited, and the message says how to see the
  name that window would have shown: start the application once without Chrono Mock.
- **The session's account of its processes holds in rare cases.** An application that ended with exit
  code 259 was reported with no exit code. A launcher that started the application and ended at the
  same moment could end the session under the running application, or leave the report silent that the
  session went on for it. A child the hook could not enter was named after whatever process took its
  pid next, and a child its parent did not finish recording held back the children recorded after it.
  An environment variable holding a lone UTF-16 surrogate reached the application with U+FFFD in its
  place.
- **An application that runs as administrator and uses WebView2 is no longer reported as working
  over pages that ran on the real clock.** WebView2 ignores the environment variable the session
  reaches its web engine through when the application is elevated, so the session never reached the
  pages of such an application and still said `works`. The session now looks for the WebView2 client
  library in the application, and when it is there and no web engine was reached, the verdict is
  `partial` (exit 10) with an explanation: the session never reached the engine and its pages may
  have run on the real clock. When the application's token is elevated it adds the reason, that Chrono
  Mock runs as administrator and so does the application. The same explanation now also covers an
  application whose engine was not reached for any other reason that no other line names. A session
  started with `--no-embedded` gets the same explanation, without the advice about administrator
  rights: the pages stay on the real clock by request, and the report no longer calls that `works`
  for an application whose pages it knows about.
- **An application the tool was still starting no longer stays behind, suspended, when the tool is
  stopped.** It is started suspended, the time library is put into it, and only then does it run. If
  the tool ended in that stretch - the window stopping it after two seconds without an answer, Ctrl+C,
  a crash - the application stayed in memory for good, invisible, holding its program file and the
  library open, so the folder they sit in could not be deleted. It now starts in a job that ends it
  with the tool until it runs, and is let out of it at once, so an application the tool leaves running
  after a session keeps running as before. Where Windows will not start a program in a job, it starts
  without one and the tool says so on its error stream.
- **A browser started in the Chromium mode now ends with everything it started.** It was put in its
  job only after it had started, so processes it opened in between stayed out and could outlive the
  session. It is in the job from its first instruction now.
- **A refused session now ends everything the application had started, not only the application.**
  When the opening check finds that nothing the application read came from the session clock, the
  tool ends it rather than let it run on the real date. Processes it had started in its first moments
  were left running, on the real clock, and nothing said so. They are ended now, each one checked
  against the time it was created, so a process that only shares a number with one of them is never
  touched. The report says the application was ended, and any process the tool could not end - one
  running with more rights than the tool, for example - is named there under `refused:` and in the new
  `left_running` field of the `verdict` event. When the tool could not look for all of them - the
  process list would not be read, or the application was still starting processes after the last of
  three passes - the report says some may still run, and the `verdict` event carries
  `family_search_incomplete`. The window says the session ended the application instead of that it
  was stopped, and lists the same processes under that line and in the copied summary, with the same
  caveat when the search was not complete.
- **A new session opens its live view and its result at the top.** The window keeps one of each for
  as long as it is open, and each kept its scroll position, so a second session showed its result
  wherever the first one had been read to - with the verdict out of sight above the edge. Both now
  start at the top every time a session enters them. Switching to the calculator and back still keeps
  the place, and the setup form still opens where it was left.
- **The injected library no longer takes down an application that does nothing wrong.** Handing a
  clock or timer function a buffer at an odd address, which a packed structure does and Windows
  accepts, was undefined behaviour in the hook, and its debug build ended the application on the spot.
  Freeing the hook library, which an application may do with any module it finds loaded in itself,
  unmapped it under its own detours, and the next clock read crashed - the library now stays loaded
  until the application ends. A failure of the hooking library to set itself up ended the application
  from inside its start, and now it declines to join instead: the application runs on the real clock
  and the session reports it as not covered.
- **Detours that could only partly be switched on no longer stay half on.** Switching them on stops at
  the first one that cannot be written, and the ones before it stayed live while the audit reported
  none, so part of the application ran on the session's clock unreported. They are switched off again
  now. The detours for a module that loads later still go on together, and when that stops part-way,
  each one goes on by itself and is counted once it is live.
- **A time function whose detour could not be switched on is reported, also in a library that is not in
  every application.** For `user32`, `winmm` and `ws2_32` a function missing from the report read as a
  library the application never loaded - also when the library was there and only the detour had
  failed, so an application could read the real `timeGetTime` beside a scaled tick count while the
  report looked complete. Such a function is now listed as not covered, and a session with one is
  partial. A function the audit only counts is listed as not watched, as it already was for the
  libraries every application has.
- **A child whose hook failed after it loaded is reported as running on the real clock.** It used to be
  missing from the report altogether. A child of the other bitness is no longer written into before it
  is counted, since the library cannot load there anyway.
- **A core that stopped in the middle of a speed change no longer leaves the timers behind.** The new
  speed was stored before the new starting points, so a slowdown cut short there read the old starting
  points at the new speed - behind where the tick count, the interrupt time and QPC stood - and an
  application that outlived the session kept that. The speed is stored last now.
- **A session whose core stopped before it closed it is no longer reported as working.** The core
  sends a first verdict a fraction of a second into the session and closes the session at its end.
  When it stopped in between - a crash, or a kill from outside - `chrono run` reported that first
  verdict as the result, over the call counts of the session's first moment, and the `--report`
  evidence file carried it without the unreliable banner. The exit code was the one the core died
  with: measured with the core ended by `taskkill /F`, `chrono run` printed WORKS, said nothing, and
  exited 1, the code for a usage error. Such a run now says CUT SHORT above its numbers, the evidence
  file leads with the unreliable banner, a line names the code the core stopped with, and the exit
  code is 3 whatever that code was.
- **The window no longer shows a session whose core stopped as working.** The result led with the first
  verdict in green - "Works" - over the counts of the session's first moment, beside "the application's
  clock is frozen", which has not been true since the application goes back to the real clock when the
  core is gone. The summary it copied had no unreliable banner, no diagnostics were kept, and the history
  recorded "works". Such a session, and a Stop the core did not close, now reads "Cut short", says there
  is no verdict for it and that the counts are a floor, and says the application is back on the real
  clock. The copied summary leads with the banner and a CUT SHORT line, the diagnostics are kept with the
  code the core stopped with - after a Stop too, which used to read them before the core had finished
  shutting down - and the history records it as undetermined. A session that failed with an
  error is no longer copied without the banner either. New session now waits until the previous session
  has been closed down, which could otherwise overwrite the new form, and a failure while recording a
  session no longer leaves Start disabled for good. A background task that failed with nobody waiting on
  it is now reported like any other unexpected fault, instead of being lost. An unexpected fault no
  longer shows the bare exception message: the box says in the interface's language what happened and
  what to do, and names the file its full details were saved to, which is written even when the window
  can no longer show the box.
- **The window names the programs a session went on for after the application closed.** When the
  program you start hands the work to another one and closes - a launcher - the session goes on for
  the programs it started, and the CLI report names them. The window said only that the application
  had exited on its own, with its exit code, and the one sentence explaining why the session lasted
  longer sat among the warnings, below the fold. The result now lists those programs under the exit
  code, by name and pid, and the copied summary lists them in the same place.
- **After a run cut short, `chrono run` no longer says the application may still run on the session
  clock.** Once the core is gone, what the application runs natively goes back to the real clock, and
  only pages inside a web engine it embeds stay on the session clock, so the line now says only that
  the application may still be running.
- **An application its session let go starts its children on the real clock.** A session often ends
  with the application still running - `--ticks`, Stop, a core that stopped - and the application
  then goes back to the real clock. Its later children did not: each one was followed into the
  session, found the session's control data that the application still held, and stayed on the
  session's date for good, after an ordered end and after a stopped core alike. Once a second session
  had started, such a child joined that one instead, read its date and was counted in its audit as if
  the second session's application had started it. Measured on an application of our own, x64 and
  x86: a child started five seconds after the end read 2030, the session's year, and with a second
  session running it read that session's 2040. Such a child is now started as it would be without
  Chrono Mock and reads the real date, and a hook that finds a session it cannot confirm as alive and
  its own refuses to join it rather than joining whatever is there. The next session's first line
  also said "a previous core had died" after every ordered end - it now says the previous session
  ended while its application kept running, and that application is on the real clock.
- **A session keeps a process it follows whoever the system names as its parent.** A session lasts
  while any process on its clock runs, and it recognised such a process by the parent the system lists
  for it. A process started with a parent chosen for it was not recognised, so the session could end
  under it. It is now recognised by when it was created, and the parent is asked only when that
  cannot be read.
- **The process that lets an application go when the session ends gives up less easily.** One failed
  attempt to start it left the application on the session's clock after the end, and one failed wait
  on the session let the application go while the session was still running. Both are now tried
  again.
- **An application that read no clock during its session reads the real one after it.** The part of
  Chrono Mock inside an application begins to watch for the end of its session at the first clock
  read, not when it starts. An application that read no clock while the session ran made that first
  read after the end, and it came back at the session's date, as did the reads right after it.
  Measured on an application of our own, x64 and x86: all five reads in a row after the end read
  2077, the session's year. They now read the real date.
- **A console application no longer reads or writes the session's own channel.** `chrono run` and
  the window talk to the session over the standard input and output of the process that runs it,
  and a console application the session started was handed both. Measured on a console application
  of our own: a line written in a Polish console code page ended the reading, so the run printed no
  verdict and exited 0, and `--json` carried no events at all. A progress bar written without a line
  break glued the heartbeat after it to its text, and every heartbeat was lost. An application
  reading its input took the command `--ticks 3` sends, and the run took 8.4 seconds instead of 3.
  With the core stopped two seconds in, `--timeout 5` still waited 20 seconds, for as long as the
  application kept writing. And once the session was over, the application's writes failed. The
  application never gets those handles now, and the same runs give the verdict, every heartbeat, 3.4
  seconds, 2.5 seconds and writes that succeed. Where its output goes instead is under Changed.
- **`chrono run` reads past a line it cannot use.** One byte that was not UTF-8, or one line over
  the 1 MiB protocol limit, ended the read and took the rest of the session with it, verdict
  included. Such a line is now skipped, and the run says at the end how many it skipped, that the
  report may be missing what they said, and how the first one began. The idle limit counts from the
  last event rather than from the last line of any kind. The window reads the session the same way.
  It splits lines only at a line feed, skips a line that is not UTF-8 or too long instead of
  patching or growing it, ignores an event of another protocol version, and keeps such lines apart
  in its diagnostics, where they used to push out the session's own messages.
- **Two holidays observed on the same day are two days off.** Under `weekend_to_mon`, Christmas on a
  Saturday and Boxing Day on a Sunday both moved to the Monday, so the Tuesday counted as a business
  day. An observed holiday that lands on a day already off, another holiday or a weekend day, now
  moves on to the next free day in the same direction, under every observance rule. None of the
  shipped calendars has such a collision, and their business days are unchanged, compared day by day
  from 1583 to 4000.
- **A calendar with fewer than one business day a week no longer runs out of room.** A business-day
  shift gave up after seven calendar days for each business day asked for, so a legal calendar with a
  long weekend and many holidays answered `+100bd` and reported no business days at all for
  `+1000bd`. It now gives up only after 400 days in a row without a business day, and the nearest
  business day is looked for as far, where it used to stop after a month.
- **A preset that says two things at once is refused instead of half read.** A base naming both a
  date parameter and an absolute date took the parameter in `chrono calc` and the absolute date in
  the window. A parametric shift dropped the `amount` and `unit` beside it, a variant shift dropped
  its `sign`, and of two parameters with one id the second won. Each is refused with exit 1 and a
  message naming what the file says twice, and the window leaves such a preset out of its list. So
  is a base or a shift naming a parameter the preset does not declare: `chrono calc` used to report
  that parameter as having no value, and then refused the value passed with `--param` as an unknown
  parameter. The message now names the parameter and the ones the preset declares. No shipped
  preset does any of this.
- **A calendar or preset saved with a UTF-8 byte order mark loads.** Windows PowerShell 5.1 writes
  one with `-Encoding UTF8`. The command line refused such a file with "expected value at line 1
  column 1", while the window read it.
- **A preset file that cannot be used names its path**, as a calendar's error always did.
- **A batch script as the target lost its arguments, or never started.** Windows starts the command
  interpreter for a `.bat` or `.cmd` itself and strips quotes from the line it hands over. So an
  argument with a space, or a script in a folder with `&` in its name, kept the script from starting,
  and the session said the application vanished right after injection. An empty argument disappeared
  and moved the others up one place, and `a&b` gave the script `a` and ran `b` as a separate command.
  The session now starts the interpreter from the system folder itself. Arguments with spaces, `&`,
  `%` or nothing in them arrive as they were given, and `%OS%` stays those four characters, as it
  does for a program, in an argument and in the script's own path. Two things arrive doubled, as the
  interpreter cannot take them otherwise: a quote inside an argument, and the trailing backslash of
  an argument that needs quotes. AutoRun commands from the registry still run, as they do when the
  script is started any other way. An argument holding a line break, and a command line longer
  than the interpreter's 8191 characters, are refused before anything starts, with exit 2, and
  `--dry-run` refuses them too.
- **A launcher, or a script that starts a program and ends, took the session down with it.** The
  session lasted only as long as the program it launched. A script that runs `start app.exe`, a
  launcher, or an application that restarts itself ends that program on purpose and leaves the real
  application running, and the session ended at that moment and put the application back on the
  real clock within a second. Ending at once, it was reported as a single-instance application that
  did not take effect. Ending a moment later, it was reported as working, over an application that
  saw the real date almost the whole time. The session now lasts until the last program on the
  session clock closes, and the report names the programs it went on for (`followed:` in the report,
  `followed` in `session_verdict` under `--json`). A helper that keeps running after the application
  closes keeps the session open too, and is named the same way. A target that hands off to a program
  the session could not enter, usually one of the other bitness, is no longer called a
  single-instance application.
- **`--dry-run` approved sessions the real run refuses.** An impossible `--at` (month 13, 30 February,
  25:61) was printed as the plan and exited 0, while the real run was refused by the core with
  exit 1. A file Windows will not start (a text file, an empty `.exe`, one cut short inside its
  header, a library) was planned as "native injection" and exited 0, while the real run failed to
  launch it with exit 2. The plan now refuses both with the code the real run gives, the moment in
  the core's own words. It reads the header where the file says it is, so a program whose header
  sits far into the file is still planned. What it leaves to the real run is whether the rest of the
  file is intact. A batch script is still planned, because Windows starts it through the command
  interpreter. `chronomock.plan/1` names that target state `not_a_program`.
- **A preset that dates from the application's file carried the time of day the file was written.**
  `chrono run --preset date-before-install` without `--param` set the session to 23:00:35 the day
  before installation, where the same preset given the date by hand sets midnight. A date parameter
  is a date, so the file's creation date now arrives as midnight too. The trial presets were not
  affected, because they set their own time.
- **Asking for help was answered as a mistake.** `chrono help` said "unknown command", and
  `chrono calc --help` said "unknown flag" and exited 1, while `chrono --help` exited 0. `help`,
  `run --help` and `calc --help` (and `-h`) now print the usage and exit 0. `--help` further along the
  command line is still passed to the application. `--at` followed by another flag, or by `-h`, now
  says that `--at` is missing its moment, instead of a sentence about a "shift".
- **Ending a session sent the application's timers back, and left its web pages on the session
  date.** A session often ends while the application keeps running: `--ticks` ran out, or the
  session was stopped from the window. The application was then handed back the real value of every
  clock. For the date that is the point, but with timers sped up (`--scale-duration`, or "Also speed
  up timers and countdowns inside the application", and `--scale-qpc`) the tick count, the
  interrupt-time counter, `timeGetTime` and the high-resolution counter went back in one step by all
  the time the session had added - 316 seconds after a five-second session at x60, on both 32 and
  64 bit. Web pages inside the application (WebView2, Qt WebEngine) stayed on the session date and
  kept running at the session speed for as long as they were open, even with no option set. Now the
  application is let go properly: the date and the time zone return to the real ones, its tick
  counts and elapsed-time counters carry on at normal speed from where the session left them, and
  its pages are handed back to the real clock as well. A repeating timer set while timers were sped
  up keeps its shorter interval until it is set again. The session says that it left the
  application running and what that means, and warns when a page did not confirm it was handed
  back.
- **A screen reader read the result's rows as code.** Every row of the audit's tables, every
  warning, the cleanup list, the speed and jump buttons and the calculator's lists told assistive
  technology what the row was built from rather than what it showed: a warning announced itself as
  `runtime.dotnet_stopwatch_qpc`, and an audit row as a dump of its fields, while the screen showed
  a sentence and three cells. Each row now reads out exactly what is on screen - "GetTickCount64,
  fake clock, 44910" - and a row made of inputs, such as a calculator step, reads out what they hold.
- **Java applications kept the machine's time zone.** A Java application under a session read the
  session date but showed it in the machine's own time zone, on every Java version from 8 on and on
  both 32 and 64 bit, while the session reported success. The zone the session hands out did not say
  that it has no daylight saving time, so Java took it for a named zone, looked the name up, found
  nothing, and fell back to the machine's current offset from the registry. The zone now says so,
  and Java builds it from the session offset, under a name like `GMT+05:30`. Node.js and Deno name a
  whole-hour session zone the same way now (`Etc/GMT-5`, and `UTC` for +00:00), where until now they
  gave it no name at all. The offset they use is unchanged.
- **The network caution missed most applications that go online.** The audit is meant to say when
  an application opened a network connection, because it may then take the time from a server,
  which no local substitution reaches. It watched one Windows function for that, and two of the
  three ways to connect never call it - among them the one WinHTTP and WinINet use, and the ones
  .NET, Node.js and Go connect through. Those applications got no caution and a clean result. The
  audit now counts every connection attempt at the point all of them pass through, whichever
  function made it, on both 32 and 64 bit. A datagram sent without a connection is still not
  counted, because it is not one.
- **The .NET timing caution said `Environment.TickCount` follows the session speed.** It does only
  when timers are sped up too (`--scale-duration`, or "Also speed up timers and countdowns inside
  the application" in the window). With that off, it runs at real speed, so a tester reading the
  caution could expect a countdown built on it to finish early when it would not. The caution now
  says when.
- **Programs on the dynamic C runtime saw the real date.** A C or C++ program that reads the time
  through the C runtime's own functions (`time()`, `localtime()`, `strftime`) and links that runtime
  as a DLL, which is the default for a Release build in Visual Studio, got the real date and the
  machine's real time zone under a session, while the session reported success. The same was true
  of anything else that reaches the clock through the system's newer entry points instead of the
  classic ones, the command interpreter's `%DATE%` among them. Windows keeps the time functions in
  one system library and has an older one pass the call on, and the substitution sat on the older
  one. It now sits where the code lives, so both routes are covered, and every such read shows in
  the channel counts instead of being invisible. Python and Ruby get the session time zone from
  the same change, where until now they kept the machine's.
- **Sped-up timers missed code that reaches them the way Windows' own libraries do.** With timers
  sped up (`--scale-duration`, or "Also speed up timers and countdowns inside the application"),
  the millisecond tick count, the interrupt-time counter and waitable timers followed the session
  only for code that asks for them through the classic entry points. Code that takes the newer
  route, which is how the older `msvcrt` runtime and the libraries inside Windows itself ask, read
  them at real speed, and with `--scale-qpc` the same was true of the high-resolution counter. Its
  waits on system objects never showed in the audit either, and a sleep taken that way was
  shortened but counted under another function's name. All of these now follow the session and
  are counted where they are called, on both 32 and 64 bit.
- **The report of `chrono run` printed seven error keys raw.** A jump the session refused (one
  counting business days, or of a kind the core does not know), a command the core could not use
  (two keys that leave the session running, two that end it) and the key sent in place of a message
  the core could not write showed up as bare keys such as `moment.needs_calendar`. Each now says
  what happened and whether the session went on. A target that vanished for a reason this version does
  not know was described as a suspected single-instance application, whatever the reason was, and
  is now shown by its key alone. The key sent in place of a message the core could not write had no
  text in the application either, and has one now.
- **Edge values in the date calculator and in time zones.** `chrono calc --analyze` with the largest
  64-bit number, a common "never expires" value, crashed with exit 101 on a computer east of UTC.
  It is now refused like any other number outside the supported years, with exit 1. A Chromium or
  Electron session whose fake clock stood at the end of the supported range, year 30828, crashed the
  same way east of UTC and left the application behind with its debugging port open. It no longer
  crashes. A time zone such as `++05:00` or `+05:+30`, and a date or time with a sign inside one of
  its fields (`2026-+1-05`, or `+23:59:59` in a set-time step), were read as valid and are refused
  now. A business-day step past 1,000,000 days was reported as a number too large to compute, and
  now names the limit instead (`calc.business_days_limit`).
- **Session dates outside the supported range, and time zones outside it.** A session could start
  past the last date the fake clock can hold, 30828-09-13 11:48:05 UTC. Once the clock stood there
  at a high speed, the next speed change or relative jump sent the application back to 1601-01-01.
  A jump by a fixed amount had no limit at all, so `-300000d` parked the clock on 1601 without a
  word. A moment just inside 1601 in UTC but before it in the session zone was accepted, and the
  application's local time then showed the real date. Each of these is now refused before it takes
  effect, with `moment.out_of_range`, and a refused jump leaves the session running as it was. A
  time zone outside -14:59 to +14:59 sent over the protocol reached the application unchecked and is
  refused now (`time.bad_zone`). A Chromium or Electron session ran on past the end of the range,
  its pages with it, and now stops there like a native one and says so (`time.fake_clock_clamped`).
- **A closed output crashed the tool.** `chrono version`, `chrono license`, `chrono calc`,
  `chrono run --dry-run` and `chrono run` itself ended with exit 101 when nobody read what they
  wrote - a pipe into a program that stops reading early, such as `| head -n 3`, or a closed error
  stream. The core writes its diagnostics to that same error stream, so it could crash too,
  part-way through ending a session and before it set an application that outlived the session back
  to normal speed. An output that cannot be written is now said once on standard error, where that
  is still possible, and the exit code stays what the command found. `chrono run` goes on to the end of the session and
  still writes `--report`. `chrono run --dry-run --json` printed a `--set-after` heartbeat above
  9223372036854775807 as a negative number and prints it as given now, and a page in an
  application's built-in web engine whose own clock read a value at either end of the 64-bit range
  crashed the core and is now judged like any other reading.
- **A preset with a zone step started the session at the wrong moment**, in `chrono run` and in
  the scenario list in the window. The moment was computed in the step's zone and then read in the
  session zone, off by the difference between the two. The same instant now reaches the session,
  shown in the session zone. No preset that ships with Chrono Mock has a zone step.
- **`--dry-run` approved dates the run refuses.** A date before 1601 or after 30828, typed, relative
  or from a preset, was planned with exit 0 and then refused by the core with exit 1. The plan and
  the run now refuse it the same way, before anything starts. A preset asking for a speed above
  x1,000,000 is refused when it is read, and a message about a broken preset file now names the
  preset.
- **A target named without its folder missed the runtime cautions.** `chrono run game.exe` from the
  game's folder did not look beside the program for Python, .NET, Java or Unity files, so the
  cautions about them never appeared, in the plan or in the report. It now looks in the current
  folder, where the session starts the program.
- **A zero character in a path, an argument or a working folder cut the program's command line
  short** without a word, when a client of the protocol sent one (a batch script already refused
  it). It is now refused before anything starts, with exit 2.
- **"Days from now" was a day out after a zone step.** `chrono calc --zone -12:00 --base now
  --to-zone +14:00` called the same instant a day away, because today was read in the session zone
  and the result in the zone after the step. Today is now read in the result's zone, on the command
  line and in the calculator window.
- **`chrono calc --analyze` misread and misprinted dates.** `31-12-25` was read as the year 31 and
  `-9-05-05` as the year -9. Both are refused now, because an ISO date starts with a four-digit year.
  A year before 1 CE was printed as `-009` and is now printed as `-0009`, the way it is read back. A
  number read as seconds or milliseconds since 1970 was shown without its time of day and without
  the zone that time is in, and now shows both. `0` and `-1`, the same second in both units, were
  listed twice as if ambiguous. The JSON analysis names its zone in a new `zone_bias_min` field.

## [0.3.0] - 2026-09-22

The window was rebuilt so it can be changed one place at a time. Most of the work is a new
structure a user never sees, but a good deal of it is visible the moment the tool opens, and
the notes below say what a user sees rather than how the parts were rearranged.

### Added

- **A search field over the scenario list.** The ready-made scenarios are meant to grow into
  a long list, and a drop-down or a wall of chips stops working well before that. The filter
  narrows the list as you type, clears with one button, and Enter takes the first match.
- **A jump to a date you type, while a session runs.** The running session could already jump
  by a relative amount. It can now jump to an absolute date, next to the relative jump and told
  apart from it by name - "Jump" moves by an amount, "Jump to" moves to a moment. The duration
  clock still never goes backwards, so a jump moves the wall clock and leaves elapsed time alone.
- **The elapsed time on each session clock.** A session at sixty times speed shows 1:01:00
  against 0:01:01 - the multiplier as something you can see rather than a number to trust. It
  used to reach only the copied report. It counts the time that passed, so changing the speed
  mid-session does not distort it and a jump does not inflate it.
- **A hidden component catalogue, opened with `--catalogue`.** It shows every interface part in
  every state, with its longest text and its extreme values, so a change to a part can be seen
  in one place. It is for whoever works on the window and never opens on its own.
- **Type-to-select in the drop-downs.** Typing a letter jumps to the matching entry - the offset,
  the unit, the zone - the way a native list does, so a long list is reached from the keyboard and
  not only the mouse.
- **The web pages inside an application follow the session clock.** An application with an
  embedded web engine (WebView2, Qt WebEngine) runs its pages in a process the hook cannot enter,
  and until now those pages showed the real date while the rest of the application showed the
  session's. A native session now asks the engine to open a local debugging port (two environment
  variables the application inherits, appended to its own switches), keeps looking for that port
  among the session's own processes, and puts every page and worker behind it on the same clock as
  the application - one start, one speed, and a speed change or a jump reaches both. The pages'
  timers run at the application's own pace: scaled only when timers are scaled for the rest of it.
  On by default. `--no-embedded`, or the "Reach the web pages inside the application" box in the
  window, leaves the pages on the real clock and opens no port. Measured by hand on a WebView2
  host and a Qt WebEngine host: the pages read the session's year at the session's rate and moved
  with a jump.
  - The report shows those pages as rows of their own - `context 1: page Date.now (12 calls)` on
    the command line, "page Date.now - fake clock" in the window's audit table - counted per page,
    never summed, and names the engine reached with its port, so a tester can attach their own
    DevTools to the same pages. The headline says "processes: N, contexts: M".
  - The port stays open, to any program on this computer, for as long as the engine runs, and a
    warning says so every time one was opened. Further warnings, each only when it happened: an
    engine that answered as DevTools but could not be attached to, a session that could not look
    for engines at all, the port reserved for a Qt engine taken by something else, pages that read
    this machine's time zone while the session's differs, and a WebView2 browser-arguments policy
    in the registry that the session's variable hid for its duration (the engine reads the
    variable first, so a debugging port set up there was not applied).
  - The machine protocol carries `coverage.kind` (`process` or `context` - pid 8 and context 8 are
    different units), `session_verdict.context_count` and `session_verdict.engines`, and
    `start.target.embedded`, all additive. A dry run says whether the pages would be reached.
  - Out of reach, and said so: the engine's helper processes and the renderer's own native reads
    (the verdict stays PARTIAL and the warning that used to claim the pages read the real clock
    now says they were reached instead), a timer already scheduled before the session reached
    the page, the pages' time zone, and an application running elevated, where the engine ignores
    the variables.
- **The window names the processes that ran on the real clock.** When an application starts a
  process the hook never got into, the verdict already said so and the report on the command line
  listed them - the window only said that some existed. "What the application read" now carries a
  table of them under the functions: one row per executable and role, counted, with a renderer
  (the process web pages run in) marked in the failure colour, because that row means the pages
  read the real clock. The heading carries the true total, the folded section's header counts them
  in a chip, a process gone before it could be named is one row in words, and a total above the
  named list is said under the table. The copied summary lists them one per process id, the way
  the command line does.
- **A renderer whose pages were reached no longer reads as a failure.** The process table marks a
  renderer in the failure colour, because a renderer is the process web pages run in, so one the hook
  never entered used to mean the application's pages read the real clock. Since the session learned to
  reach those pages that is no longer true of every renderer: its pages run on the session clock and
  only its own native reads do not. Such a row now reads as partial - the same word the verdict beside
  it uses - and says why, so the screen no longer contradicts its own verdict. Two renderers of one
  executable, one reached and one not, stay two rows rather than being counted into one.
- **The window names the web engine it reached, and the port it reached it on.** A session that
  put an application's pages on the session clock warns that a local debugging port stands open to
  other programs on this computer for as long as the engine runs. The command line named the port
  beside that warning and the window did not, so the warning named something a reader had no way
  to identify. "What the application read" now carries a second table under the processes: the
  engine as it names itself, and its port. The two tables answer one question between them - what
  inside this application ran on which clock - and the port is where a tester points their own
  DevTools at the same pages. The copied summary keeps the process id beside the port, the way the
  command line does.

### Changed

- **The window now moves through three phases instead of one panel.** Setup, then the running
  Session, then the Result, each replacing the last and showing only what that step needs. The
  single panel stacked the form, the clocks, the verdict and the audit on one screen, so during
  a session the clocks sat below the fold and the verdict lower still. Each screen is now in the
  order of the questions it answers.
- **Setup was rewritten for somebody opening the tool for the first time.** A line at the top
  says what the tool does and that it does not change the system clock. There is one path to the
  moment - the date and time field is the answer, and the scenario list and the "starts at"
  offset only fill it in - so the field comes first and the two fillers follow it. The labels
  say what they mean to a reader rather than to the mechanism: Application, Date and time,
  Starts at, Time zone, Speed. Start gives a reason for every refusal rather than the first one
  it meets, the footer that carries it is pinned so it cannot fall below the fold, and the accent
  leads to the next thing to do rather than sitting on a button that cannot be pressed yet.
- **A session starts at real speed.** The first run used to start sixty times faster than real
  time, shown as `×60`. The tool's first promise is the date it starts an application on, and
  speeding time up is the second thing, so an unasked-for sixty times on a first run was a
  surprise that every first run paid. It starts at real speed now, shown as `×1` beside `×10`,
  `×60` and `×1440`.
- **The default time zone is UTC.** The panel used to start in a local summer-time offset, which
  reads like somebody's own clock without saying whose, from a zone list that covers only two
  markets - so an arbitrary local default is worse than a neutral one. The default moment is the
  signed 32-bit time limit, which is a moment in UTC, so the default date and its own reason now
  agree instead of being two hours apart.
- **The audit reads in the reader's words.** The block is called "What the application read" now,
  and its lines say "Read from the fake clock", "Read from the real clock", "Left on the real
  clock on purpose" and "Could not be watched" in place of the mechanism's terms. On a result
  worse than a clean pass it opens on its own, because a reader with a bad verdict has one
  question - which clocks read the real time - and the audit is the answer.
- **The result is led by the verdict.** The result screen puts the verdict at the top in the
  colour of its meaning, with the reason the core gives even when the answer is a clean pass, how
  the session ended, the application's exit code, buttons to copy the summary or the diagnostics,
  what the application saw, the audit, and the history of past sessions. History rows read as
  dates now rather than as raw machine time with a T and a Z.
- **The calculator was repainted to match.** The two modules sat on slightly different
  backgrounds, with the seam visible under the module switch. They are drawn from the same parts
  now, so the two look like one program.
- **The controls and the explaining text were made legible.** Field and list borders were below
  the contrast at which an edge is visible, so a control read as a shapeless patch - the borders
  sit at the visible threshold now, on cards. The prose that explains the tool was taking two
  reductions at once, smaller and dimmer, and is now at reading size in the quieter ink, so the
  text that teaches the tool is no longer the hardest thing on the screen to read.
- **The substitution panel is called "Run an app".** The screen's own name spoke of the mechanism
  rather than the task - a reader wants to run an application on another date, and the tab says so
  now. "Substitution" and the other mechanism words stay in the command line and the reports.
- **Keyboard focus is a tight border rather than a ring floating outside the control.** The ring
  stood off the edge with a gap and read as a heavy halo - focus recolours the control's own edge
  now, and a section no longer draws a box around itself when a field inside it is being edited.
- **The calculator carries both ways of naming a moment in one column.** The reverse analysis moved
  up beside the builder, the three columns fill the height, the scenario descriptions read in full
  rather than trailing off into an ellipsis, and the copy buttons confirm with "Copied".
- **The footer's primary action is sized to its word, and Stop is red.** Start and New session were
  a banner far wider than their text and are sized to it now. Stop wears the failure red, because it
  ends the session, and red is not the accent the rest of the tool keeps for the next step.
- **The reason Start is disabled stands beside the button, in red.** It used to be a grey line
  above the button, one more line of help to skim past. When Start does not respond, that
  sentence is the one thing to read, so it took the button's row and the colour that says why.
- **"What the application read" is one table.** Every function the application asked the time
  from is a row: its name, where it read from - fake clock, real clock, not watched, or hooked
  late - as a word in the colour that says so, and how many times. It was five separate lists
  with five headings, and finding one function meant finding the heading it sat under. Warnings
  sit under the table, each in its own box.
- **Every date on the screen is typed and picked the same way.** A scenario that takes a date -
  a trial's start, a birth date - offered a bare text box where the rest of the window offers a
  calendar. It has the calendar now.
- **The calculator's builder has one left edge.** Its fields sat at different indents, some under
  a label and some beside one, and the time field wrapped under the date. Every field now stands
  under its own label on the column's edge, and the date and the time share a line.
- **The time zone reads the same way in both pickers.** Setup showed the offset alone while the
  calculator showed the offset and the name, so "UTC+00:00" and "UTC+02:00 · this machine" looked
  like two different facts. Both show the offset and the name now, which is what tells the neutral
  default apart from this computer's clock.
- **"Specific instant (UTC)" is gone from the calculator's start points.** It was a time zone
  hidden inside a kind of date, from before the calculator had a zone picker. A scenario that
  starts from a UTC instant, such as the 2038 boundary, now arrives as a specific date with the
  zone picker on UTC, so the one control that names zones is the one that says it.
- **"Use this date" stands out as the calculator's main action**, in the same style as Start.
- **A process the hook never got into now changes the verdict, and is named.** An application
  that starts a process the native mechanism cannot follow into - the renderer of an embedded
  Chromium web engine such as WebView2 or Qt WebEngine, spawned through the Chromium sandbox, or a
  child of the other bitness - used to be reported as WORKS with "every process that read time
  saw the session clock", while the pages inside that application showed the real date. The
  verdict is now PARTIAL (or DID NOT TAKE EFFECT when nothing read the fake clock at all), the
  exit code 10 or 11 instead of 0, and the report lists each such process by image name and
  parent, under "processes this app spawned that the hook could not follow". When they belong to
  a web engine, a warning says so in one sentence: every page inside this application read the
  real clock. The machine protocol carries the list on `session_verdict` as `uncovered_children`
  with a total, both additive. A pipeline that branched on exit code 0 for such an application was
  branching on a claim the tool could not back.

### Fixed

- **A second session in the same window kept the first session's channels in the audit.** The
  list of what could not be watched is a running union, and the reset before a new session did
  not clear it, so the second session listed channels from the first.
- **The warning for a channel hooked late named a count it did not have.** It read "One time
  function" whether one function or several came under the fake clock late, and a module loaded
  after start usually brings several. It names the channels now.
- **The release signing check compares the signature states against the list it signed.** The
  step that reads the certificate back out of every signed file checked it against the wrong
  list, so a mismatch could have passed unnoticed.
- **The date calculator's scrollbar sat over the values and the copy buttons.** Each column keeps a
  lane for it now, so it no longer covers what it scrolls past.
- **The start point's time field was clipped in the calculator.** The moment shared one row with the
  kind picker in the narrow middle column and ran off the edge - it has its own full-width row now.
- **Dialog buttons clipped their own labels.** A uniform padding on a fixed-height button ate the
  vertical room the label needed.
- **The RFC 1123 output is labelled GMT.** Its value has always ended in GMT, and the row says so
  now, so a reader is not left guessing which zone it is in.
- **"Set up again" on a finished session did nothing visible.** It filled the setup form behind
  the result screen and left the result on top. It returns to the filled form now, with a note
  when a zone or speed from the old session is no longer offered. Nothing starts until Start.
- **"Use this date" did nothing visible once a session had run.** After a session ended it filled
  the hidden form the same way, and while a session ran it only switched screens. It now returns
  a finished session to the setup form with the date in it, and during a running session it
  fills the "Jump to" field with the date in the session's zone, ready for Jump.
- **The copied report left out the channels that could not be watched.** The window listed them,
  the report did not, so a report could claim a watch that was not running.

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

[0.4.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.4.0
[0.3.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.3.0
[0.2.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.2.0
[0.1.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.1.0

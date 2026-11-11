# Chrono Mock ⏱ - run any Windows application at a different date (without touching the system clock) 
<p align="center">
  <b>Follow me:</b>
  &nbsp
  <a href="https://discord.gg/bDTu8HX"><img src="https://cdn.simpleicons.org/discord/5865F2" alt="Discord" height="26" align="absmiddle"></a>
  &nbsp
  <a href="https://facebook.com/donislawdev"><img src="https://cdn.simpleicons.org/facebook/0866FF" alt="Facebook" height="26" align="absmiddle"></a>
  &nbsp
  <a href="https://x.com/donislawdev"><picture><source media="(prefers-color-scheme: dark)" srcset="https://cdn.simpleicons.org/x/FFFFFF"><img src="https://cdn.simpleicons.org/x/000000" alt="X (Twitter)" height="26" align="absmiddle"></picture></a>
  &nbsp
  <a href="https://bsky.app/profile/donislawdev.bsky.social"><img src="https://cdn.simpleicons.org/bluesky/0285FF" alt="Bluesky" height="26" align="absmiddle"></a>
  &nbsp
 <a href="https://store.steampowered.com/developer/DonislawDev"><picture><source media="(prefers-color-scheme: dark)" srcset="https://cdn.simpleicons.org/steam/FFFFFF"><img src="https://cdn.simpleicons.org/steam/000000" alt="Steam" height="26" align="absmiddle"></picture></a>
</p>

[![CI](https://github.com/donislawdev/ChronoMock/actions/workflows/ci.yml/badge.svg)](https://github.com/donislawdev/ChronoMock/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/donislawdev/ChronoMock?sort=semver)](https://github.com/donislawdev/ChronoMock/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/donislawdev/ChronoMock/total)](https://github.com/donislawdev/ChronoMock/releases)
[![License: GPLv3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078D6)
[![Website](https://img.shields.io/badge/website-chronomock.donislawdev.com-e4573f)](https://chronomock.donislawdev.com/)

**Chrono Mock** is a tool for testers and developers: run one application as if it were a different
date, and find out whether it actually worked. Set the clock to next year, freeze it, or speed it up
so a 30-day trial expires in half an hour. The rest of the machine keeps the real time - your domain
logon, your certificates and every other application carry on as normal. Like **RunAsDate**, with the
parts that were missing: acceleration, an independent time zone, child processes, and a verdict that
tells you which time channels the substitution actually covered. On Linux
**[libfaketime](https://github.com/wolfcw/libfaketime)** has done this for years. On Windows there has
been nothing comparable.

⭐ **If it saved you a 30-day wait, leave a star.** That is how the next tester who needs it finds out
it exists.

**What it can do**

- **Any moment, absolute or relative** - a specific date and time, or `+30 days`, `-1 year`.
- **Flowing, frozen, or accelerated** - run from the shifted point at normal speed, hold the clock
  still, or multiply it by any whole number up to a million (x1440 turns a day into a minute).
- **Jump mid-session** - forward or backward, including the backward jump a laptop makes after
  resyncing with a time server, which breaks anything measuring elapsed time without a guard.
- **Its own time zone** - a fixed offset for that process only, while the system zone stays put.
- **Child processes come along** - installers and launchers spawn children, and without that the test
  covers only half of what ran. A launcher or a script that starts the application and ends does not
  end the session: it lasts until the last program on the session clock closes.
- **Tells you if it worked** - a verdict, the channels covered with call counts, the channels missed,
  and warnings with consequences. [See below](#how-do-you-know-it-worked---the-time-source-audit) - this
  is the part no other tool does.
- **A date calculator built for QA** - month-end, quarter close, the 2038 boundary, "the first day
  after a 14-day trial", business days and holidays, every format at once.
- **Electron and Chromium apps too** - through a different mechanism, because their clock lives in a
  sandboxed renderer where injection does not reach.
- **The web pages inside an ordinary application too** - an application with a built-in web engine
  (WebView2, Qt WebEngine) runs its pages in a process the hook cannot enter. The session asks the
  engine to open a local debugging port and puts those pages on the same clock as the rest of the
  application, through it. On by default, off with one switch.
- **A window and a command line** - the same engine behind both, exit codes for CI.
- **Portable, or installed** - the zips need no installer and no administrator rights and run from a USB
  stick, and a signed installer is there for a whole machine.

![Chrono Mock running another Windows application at a fake date. The left window is Chrono Mock: the application's clock reads 19 January 2038 and climbs at sixty times real speed while the real clock beside it moves a few seconds. The right window is the application under test, whose own session record shows it started on 19 January 2038 and has been running for minutes while barely half a minute has passed in the room.](site/assets/chrono-in-action.gif)

*A real session. The window on the right is the program under test - and it recorded its own start as
**19 January 2038**, then ran for minutes of its own time while the clock in the room moved seconds.
That is the whole point.*

![Clicking the Star button at the top of the Chrono Mock repository page: the counter goes from Star 0 to Starred 1.](assets/sta-chono.gif)

---

## Download and run

Grab the latest build from the **[Releases page](https://github.com/donislawdev/ChronoMock/releases/latest)**:

| File | What it is | Size |
|---|---|---|
| `ChronoMock-app-win-x64.zip` | The desktop app, self-contained - no .NET install needed | ~65 MB |
| `ChronoMock-app-win-x64.msi` | The same desktop app and the command line, installed for every account on the machine | ~54 MB |
| `ChronoMock-cli-win.zip` | Just the command-line tool, for CI and scripts | ~2.6 MB |

Unzip anywhere and run `ChronoMock.exe` (or `chrono.exe` for the CLI). The zips need no installer and
no administrator rights, and they write nothing to the registry, with one exception you have to ask
for: reaching the web pages of an application that itself runs as administrator (the paragraph on
elevated applications, further down, says what it writes and what it costs). Windows keeps records of
its own about the programs that run, and these two are no exception. On the Windows 11 we traced, it
noted when each of them last ran, runs started from a script included, and for a window the text
input services and the graphics driver kept entries of their own. Which records there are depends on
the Windows version and its settings. They belong to the system, not to Chrono Mock, which neither
writes nor removes them.

The `.msi` is the other way in. It installs the window and the command line for every account on the
machine, into `Program Files\Chrono Mock`: a Start menu entry for the window and the 64-bit `chrono` on
the machine's PATH (the 32-bit one is `core\x86\chrono.exe` in the same folder). Installing needs
administrator rights, running does not. Besides the program it adds the Start menu entry, the PATH
entry, an entry in Settings, Apps and the key `HKLM\SOFTWARE\DonislawDev\Chrono Mock`. Uninstalling
removes all of that and leaves `%LOCALAPPDATA%\ChronoMock`, where the window keeps its session history
and diagnostics logs, alone. A newer version installs over an older one. The installer closes nothing
that is running: a file still held by a running program is replaced at the next restart, and the
installer says so with exit code 3010.

**Checking what you downloaded.** From v0.2.0 onward, each release also carries `SHA256SUMS`,
a bill of materials per package (`*.spdx.json`, SPDX 2.3), and an attestation of that bill of
materials, signed by GitHub's infrastructure. Two ways to check it, both of which work on either
archive:

<!-- verify-commands -->
```
gh attestation verify ChronoMock-app-win-x64.zip --repo donislawdev/ChronoMock --predicate-type https://spdx.dev/Document/v2.3
gh attestation verify ChronoMock-app-win-x64.zip --repo donislawdev/ChronoMock --predicate-type https://spdx.dev/Document/v2.3 --bundle ChronoMock-app-win-x64.zip.sigstore.json
```
<!-- /verify-commands -->

The first asks GitHub. The second asks nothing and nobody: it checks the `.sigstore.json` published
beside the archive, so it works with no network and no account.

**Both need `--predicate-type`, and leaving it off is the mistake worth naming.** The tool asks for
build provenance by default, and a released archive deliberately has none - it was signed by a person
on a machine with a hardware key, not built by a workflow. Without the flag one spelling reports "no
attestation found" and another returns 404, and both look like a broken release when nothing is wrong.

The binaries inside also carry an Authenticode signature with an RFC 3161 timestamp, so Windows
names the publisher instead of warning about an unknown one. **The v0.1.0 archives, which are still
on the releases list, do not** - they predate all of this, and neither a signature nor an
attestation can be granted after the fact. [SECURITY.md](SECURITY.md#verifying-a-download) says what
is signed, what is not, and why.

> **Early release.** The substitution core is implemented and covered by an automated suite that runs
> on every commit, plus an end-to-end harness exercised against real applications on both 32-bit and
> 64-bit builds before a release. Native Windows applications are the well-tested case. .NET, Java,
> Python and Electron are marked **experimental** in the
> [support matrix](#support-matrix), and that word has a defined meaning there. Whatever the runtime,
> the built-in audit tells you whether your specific application was covered - you never have to take
> this table's word for it.

**One thing to do first:** if the application you are testing stores data, back up its data directory
before your first future-dated session. [Here is why](#before-you-run-an-application-in-the-future) -
it takes one paragraph and it can save you a licence.

Antivirus software may flag the injected library. That is expected, and
[explained below](#antivirus-and-microsoft-defender).

---

## Two minutes with it

**In the window:** pick the application - or drag its `.exe` onto the window, open it with
`ChronoMock.exe <path>`, or reopen one you ran before - then click a scenario (`Year rollover`, `Last day of month`, `2038 boundary`, ...) or
type a date, choose a speed, press Start. Before you start, the panel reads the date back to you in
plain language ("The application will see Friday, 25 December 2026, 09:00:00 (UTC+02:00)"), so a
mistyped year or an off-by-one month is visible while it is still cheap. If the application needs
command-line arguments or a particular working folder, they are under *Arguments and working folder*.

Once it starts, the panel shows the application's clock and the real clock side by side, the
verdict, and which time channels the application is actually reading.

![The Chrono Mock session panel: the application's clock reading 19 January 2038 at 03:14:14 beside the real clock reading 22 September 2026 at 15:50:30, each with its own elapsed time, then the speeds from ×1 to ×1440 with a Freeze, the relative jumps and a jump to a typed date, and on the footer a green Works verdict beside a red Stop session.](site/assets/session-panel.png)

**From the command line:**

```bash
# Run an application as if it were the last second of 2027
chrono run "C:\apps\test.exe" --at 2027-12-31T23:59:59

# Watch a 30-day trial expire while you make coffee: x1440 is a day a minute
chrono run "C:\apps\test.exe" --mode x1440

# Use a named scenario instead of a date, and write the evidence to a file
chrono run "C:\apps\test.exe" --preset year-rollover --report session.txt

# Countdowns and timers are on a separate axis - add --scale-duration to speed those up too
chrono run "C:\apps\Timer.exe" --mode x60 --scale-duration

# In CI, put a ceiling on the run: --timeout ends the session and exits 6, which is not a verdict
chrono run "C:\apps\test.exe" --at 2027-12-31T23:59:59 --timeout 120
```

The calculator answers the other half of date testing - *which* date to use:

```console
$ chrono calc --base today --shift +30d
Chrono Mock - date calculator
  base:    2026-09-04T00:00:00  (today, session zone +02:00, from the host)
  step 1:  shift +30 days  -> 2026-10-04T00:00:00
  result:  2026-10-04T00:00:00
  formats:
    ISO date      2026-10-04
    ISO datetime  2026-10-04T00:00:00+02:00
    US            10/04/2026
    PL            04.10.2026
    epoch (s)     1791064800
    epoch (ms)    1791064800000
    FILETIME      134355384000000000
    RFC 1123      Sat, 03 Oct 2026 22:00:00 GMT
```

The named scenarios both commands take with `--preset` are listed by `chrono presets`, which also
names every preset file it left out and why - the reason `--preset` would give for it. The window's
scenario and preset lists come from the same command, so a file is offered there exactly when the
command line accepts it.

Every command speaks `--json` as well, and exits with a code your pipeline can branch on. Every flag
and every exit code of both commands is listed in the
[CLI reference](https://chronomock.donislawdev.com/cli-reference/).

A console application's own output appears on `chrono run`'s standard error while it runs, so
standard output holds the report, or with `--json` the events, and nothing else. An application
that is still running when the session ends keeps writing there, so a script or a CI step that
reads that stream to its end waits until the application exits, and `chrono run` says so when it
happens. Started from the window, a console application opens in a console window of its own.

<details>
<summary><strong>Table of contents</strong></summary>

- [Why this exists](#why-this-exists)
- [How do you know it worked - the time source audit](#how-do-you-know-it-worked---the-time-source-audit)
- [Built-in date calculator](#built-in-date-calculator)
- [Honest limits](#honest-limits)
- [Before you run an application in the future](#before-you-run-an-application-in-the-future)
- [Antivirus and Microsoft Defender](#antivirus-and-microsoft-defender)
- [Support matrix](#support-matrix)
- [This is not a licence bypass tool](#this-is-not-a-licence-bypass-tool)
- [Contributing](#contributing)
- [License](#license)

</details>

---

## Why this exists

A huge class of application behaviour depends on the date: licence expiry, trial periods,
subscription renewals, certificate validity, token lifetimes, cache expiration, month-end and
year-end reports, interest accrual, reminders.

Testing any of it means moving time. Today there are two options, and both are bad:

**Change the system clock.** It breaks domain logon and Kerberos, invalidates TLS certificates so the
browser stops working, confuses Windows Update, makes every other application on the machine write
wrong timestamps, and leaves a mess behind when you set it back. On a domain-joined machine, group
policy usually blocks the change or reverts it minutes later - so the test is not even repeatable.

**Wait.** Which means: do not test it.

So date-dependent tests get skipped, and "the licence never expires" ships to the customer.

Chrono Mock shifts time **for one process only**. The domain, the certificates and every other
application keep seeing the real date and keep working.

---

## How do you know it worked - the time source audit

**This is the part nobody else does.**

With every other tool, you do not know. The application starts, it shows some date, and you assume.
But an application can read the clock through a channel the tool never intercepted - from a server,
from a database, through a runtime layer - and then "the test passed" means nothing at all.

Chrono Mock answers the question directly:

- A verdict before the real test begins: **works / works partially / does not work**
- Which time channels were covered, and how many times the application queried each
- **Which channels were not covered** - stated plainly, not omitted
- Warnings with consequences attached: *"this application reads time from the network - local
  substitution may not be enough"*

**When the verdict is "does not work", the process is stopped immediately** - before you can interact
with it. The verdict cannot precede the launch, because the only source of truth is a reading from
inside the running process. What Chrono Mock guarantees is that you learn the truth before you start
working, not that a doomed launch never happens. You can override and continue, but the session is
then marked as unreliable everywhere it appears - in the history, and in any evidence you export.

A silent failure is the one outcome this tool refuses to produce. An application that looks
time-shifted but is not is worse than one that never launched.

---

## Built-in date calculator

The other half of date testing is knowing *which* date to use. Chrono Mock includes a calculator that
speaks in QA terms rather than arithmetic:

- **Build the expression from controls, not from a sentence** - a starting point plus steps
  (`- 18 years`, `- 1 day`, `set time 23:59:59`), with the intermediate result shown after each step
- **Presets that explain what they catch** - trial end, month-end and quarter close, year-end
  rollover, epoch 0, the 2038 boundary
- **Every format at once**, one click to copy - ISO 8601, `MM/dd/yyyy`, `dd.MM.yyyy`, epoch seconds
  and milliseconds, `FILETIME`, RFC 1123, and a custom mask to match whatever the application under
  test expects
- **Reverse direction** - paste a date from a log and get *"3 days before quarter end"*, *"Saturday,
  not a business day"*. When the format is ambiguous, both readings are shown rather than one guessed
- **Business days and holidays** - United States and Poland at launch, including the US rule where a
  holiday landing on a Saturday is observed on the preceding Friday. Each calendar answers only for
  the years its sources cover (Poland from 2002, US federal from 1960, US banking from 2008) and
  says when it was last checked against the law

Any calculated date goes straight into a time-shift session with one click.

![The Chrono Mock calculator: a searchable scenario list with its explanations on the left, the date builder in the middle where a pasted 04/08/2008 is reported as ambiguous with both the American and the Polish reading shown, and on the right the result in every format with a copy button against each one and Use this date below them.](site/assets/calculator.png)

---

## Honest limits

This section is longer than most tools' entire documentation, on purpose. A tool that quietly fails to
cover something is worse than one that says what it cannot do.

**Time sources that cannot be intercepted from user mode:**

- Direct reads of the shared user-mode data page - the value sits in memory rather than behind a
  function call, so there is nothing to hook. Rare in ordinary applications, common in licensing and
  anti-tamper code, which is exactly where testing matters most
- Direct system calls that bypass the standard exports - same pattern, same class of application
- Time obtained out-of-process, through WMI, out-of-process COM servers or RPC
- Time from the network: an HTTP `Date` header, NTP, an API response, or a database server's own clock
- Time from kernel drivers

**Other limits:**

- **A jump changes what the application sees, not when it wakes up.** Waits and timers already
  scheduled with the kernel keep the length they were given when they started - divided by the speed
  under `--scale-duration` for the sleeps and timers the session scales, real for the object waits and
  multimedia timers it only counts. That includes a timer set for a date and time: its due time is
  turned into a wait from the moment it is set, so a later jump or speed change does not move it. A
  jump forward past it does not fire it early, and a jump back does not hold it back - Windows itself
  would move such a timer when the system clock changes. Most applications poll the clock, so jumps
  work in practice - but not all of them do
- **The session time zone is a fixed offset with no daylight saving.** A session set to Poland stays
  at the offset you chose whether its clock is in March or in July, so a run that crosses a real DST
  boundary drifts an hour from what that zone would really show. Forcing an application through a DST
  transition is therefore not something this tool does yet, and the date calculator says so rather
  than guessing
- **The zone reports itself as "Chrono Session"**, a name no Windows registry knows, and says it
  has no daylight saving time. Anything that maps that name back to a zone falls back to the offset
  instead, which is the right answer: .NET's `TimeZoneInfo.Local` keeps the name, Java names the zone
  after its offset (`GMT+05:30`), and so do Node.js and Deno for a whole hour (`Etc/GMT-5`). The Rust
  `jiff` crate is the exception: it finds a zone by its name alone and would fall back to UTC, so a
  session gives a program built on it the zone in the `TZ` variable instead (`CMS-5:30` for +05:30).
  A target that insists on a registry name will not find one
- **A `TZ` variable in your environment wins over the session zone** for the programs that read it:
  the C runtime, and so Python, MSYS2 and Node.js. Measured under a session at +05:30 with `TZ` set
  to UTC+2: Python showed +02:00 and Node.js +00:00, and the report did not say so, while .NET, which
  does not read the variable, showed the session's zone. Leave `TZ` unset for a session in another zone
- **A windowed program that ends within its first 300 ms is reported as a suspected hand-off** (exit
  code 12), because that is what the second instance of a single-instance application does: it passes
  its work to the copy already running, which the session never reached. A console program that ends
  that quickly has simply done its work, and its session ends with a verdict like any other. A windowed
  program that ends that fast on purpose, without showing a window, is suspected all the same
- **A hand-off that comes later is recognised by the copy it went to.** A windowed program that closes
  on its own after its first 300 ms, without ever showing a window, while a copy of the same program file
  runs outside the session, most likely passed its work to that copy: the verdict is at most `partial`,
  and the report names the copy (`session.handed_to_copy`). A hand-off to another installation of the
  program - another program file - is not recognised, and neither is one by a program that showed a
  window first. The session looks for a window about once a second, so a window that opens and closes
  again within that second can go unseen, and the program then counts as one that never showed a
  window. When the list of running programs cannot be read, the report says that the copies were not
  looked for (`session.copy_search_failed`)
- **A child started under another user token is not followed.** Children started through
  `CreateProcessW` or `CreateProcessA`, whichever system library the call goes through, `WinExec`,
  the C runtime's `system`, spawn and `_popen` functions or `ShellExecuteEx` join the session. One
  started under another user token, with `CreateProcessAsUserW` - by a launcher that drops its rights,
  or a sandbox broker - runs on the real clock, and the report names it among the processes the
  session did not cover. A program a Windows service starts on the
  application's behalf is outside its process tree altogether
- **What runs while Windows loads the application reads the real clock.** The fake clock is attached
  once Windows has loaded the application, so the start-up code of the libraries it imports
  (`DllMain`) and its TLS callbacks run before it. Everything from the application's entry point on,
  static constructors included, runs after it
- **Chrono Mock cleans up after itself. It cannot clean up after the application you tested.** See the
  warning below
- Windows only. No macOS, no Linux - `libfaketime` already covers Linux well

---

## Before you run an application in the future

> ⚠️ **Read this one before your first future-dated session.**

An application running in 2028 will write 2028 dates wherever it stores its data: its database, its
configuration, its cache, its licence file. Those values stay there after you return to real time.

For applications with licence protection this can be permanent. "Clock rollback detected → licence
invalidated" is a standard industry pattern, and it will do exactly what it was designed to do.

**Back up the application's data directory before your first future-dated session.**

Chrono Mock leaves nothing behind in your system - no persistent hooks, no registry entries (bar the
opt-in described further down for an application that runs as administrator, which writes one value
for the length of the session, removes it before the verdict and names it in the report), and
nothing outside its own folder except three things it will name for you: the session history and the
diagnostics logs, kept in `%LOCALAPPDATA%\ChronoMock\` by every copy, so they hold the applications you
tested and the arguments you gave them where only your account reads them (the environment variable
`CHRONOMOCK_DATA_DIR`, set to a full path, moves both to that folder - for a test run that must leave
your own history alone - and who reads them there is up to that folder), and, for an Electron or
Chromium target, a throwaway browser profile under `%TEMP%` that is deleted when the session ends - and
reported in the session summary if it could not be. That guarantee covers the tool. It does not, and cannot, cover the
application under test.

---

## Antivirus and Microsoft Defender

Chrono Mock shifts time by injecting a small library into the target process. That is a legitimate,
documented Windows technique - and it is also one that malware uses, so antivirus software, Microsoft
Defender included, may flag or quarantine the injected library or block the injection outright. This
is a false positive triggered by the technique, not by anything the tool does to your machine.

If a session fails to start and Defender reports a threat, add an exclusion in Windows Security
(Virus and threat protection, Manage settings, Exclusions, Add an exclusion), or run it on a machine
where you are permitted to do so. The tool is open source under GPL-3.0 - the injected library is
`chrono_hook.dll`, built from the code in this repository, and you can rebuild it yourself.

**Exclude the library, not the folder.** Choose **File** and give the full path to the injected
library - `core\x64\chrono_hook.dll` in the GUI package, `chrono_hook.dll` in the CLI one, plus the
`x86` copy if you test 32-bit targets. Microsoft's own guidance is that every exclusion is a
protection gap, so it is worth keeping this one as small as it can be. There is a specific reason
here: a **Folder** exclusion turns the Chrono Mock directory into a place nothing is scanned - and
that is the same directory the tool loads `chrono_hook.dll` from and injects into every application
you point it at. Excluding the one file leaves the rest of the folder watched. If the narrower
exclusion is not enough for your Defender configuration, the folder exclusion is the fallback, not
the starting point.

The Chromium / Electron mode does not inject at all. It launches the target with a debug port and a
clean isolated profile (see the support matrix), so it is not affected by this.

An application with an embedded web engine gets both: the native hook in the application, and the
pages inside it reached over the engine's own debugging port. That port is opened by the engine at the
session's request (two environment variables the target inherits) and stays open, to any program on
this computer, for as long as the engine runs - the report says so, names the port, and `--no-embedded`
(or the checkbox in the window) leaves the pages on the real clock and opens nothing. The engine's
helper processes and the renderer's own native reads stay on the real clock, so such a session is
reported as PARTIAL with the reason spelled out. An application running elevated is out of reach by
default: the engine ignores the variables there, so its pages stay on the real clock. An application
runs elevated when Chrono Mock does, because it inherits that. The session says so instead of
reporting success - it is PARTIAL, with the reason spelled out. Run Chrono Mock without administrator
rights and the application starts without them too, and its pages are reached. An application that has
to run as administrator is reached by the opt-in below, from a Chrono Mock that runs as administrator.

**Reaching the pages of an application that runs as administrator (opt-in, off by default).**
`chrono run app.exe --elevated-embedded`, or the checkbox "Reach the web pages of an application that
runs as administrator" in the window, from a Chrono Mock that runs as administrator too. WebView2
honours the machine registry in such an application, so for the length of the session Chrono Mock
writes ONE value under `HKLM\SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments`,
named after the application's file (it opens a debugging port on loopback and carries a marker naming
the session), and removes it before the verdict. It is the one thing in this tool that writes the
registry, and the cost is worth weighing first. While the application runs, any program on this
computer can reach its pages through that port, and the application has administrator rights. Any
program started under the same file name, by anybody, gets the port too for as long as the session
lasts, and the port of an application that outlives the session stays open until it exits.

Chrono Mock never overwrites a value it did not write: a value of yours under the same name is left
as it is, and a `*` value of yours is carried into its own, because a value under the file name
replaces `*` for that application. It removes only the keys it created, and only when they are empty.
If a session ends abruptly, the value it left carries the marker of its dead owner: the next session
says it is there, and the next one started with this option removes it and says so. Every step is
named in the report, and a value that could not be removed is its first warning. Some security
products flag a write under `Policies\Microsoft\Edge`. A script (`.bat`, `.cmd`) as the target is not
covered, because the value is named after the `.exe` that is started, and a launcher whose WebView2
application is another program is reported with the name of the program to start instead.

**Timers in a hidden window (opt-in, off by default).** A Chromium engine runs the timers of a
minimized or hidden window about once a second, so with time sped up a hidden window loses the
speed-up. `chrono run app.exe --keep-background-timers`, or the option "Keep timers at speed in hidden
windows" in the window, starts the web engine with that slowdown switched off - the browser of a
Chromium or Electron application and the engine inside an application alike. The application then
also works at full pace while it is hidden. Without the option, a page that was hidden while time ran
faster is named in the report. An application started as administrator through `--elevated-embedded`
does not get the switches.

---

## Support matrix

_A row "measured on" a platform comes from the automated tests run before every release, on targets
of our own: the probe harness, last run in full on 2026-10-26 (x64 and x86), the runtime maps and the
embedded-engine stand. A row measured "by hand" comes from a person running the case. The Basis column
says which is which, and some rows have both._

| Environment | Status | Basis | Notes |
|---|---|---|---|
| Native Win32 / Win64 (C, C++, Delphi) | supported | measured on x64, x86 | The cleanest case |
| .NET (Framework and modern) | experimental | measured on x64, x86 | Time calls go through Win32 exports and are covered, including the session time zone. Stopwatch stays on the real high-resolution counter unless you opt in with Scale QPC (`--scale-qpc`), which accelerates it too |
| Java (JVM) | experimental | measured on x64, x86 | The wall clock, at the session's speed too, and the session time zone are covered. The zone arrives as a fixed offset named after it, like `GMT+05:30`. nanoTime stays on the real high-resolution counter unless you opt in with Scale QPC (`--scale-qpc`) |
| Python (CPython, incl. PyInstaller) | experimental | measured on x64 by the runtime maps (wall clock, time zone), by hand on x64 (the high-resolution clocks) | Wall clock (time.time, datetime) and the session time zone (time.localtime) are covered. perf_counter, and monotonic on Python 3.13+, are on the high-resolution counter - real by default, accelerated when you opt in with Scale QPC (`--scale-qpc`) |
| JavaScript runtimes (Node.js, Deno) | experimental | measured on x64 by the runtime maps | `Date` and the session time zone are covered. A zone of whole hours is named like `Etc/GMT-5`, and one with a half-hour offset has no name in `Intl`, though its offset is right |
| Ruby | experimental | measured on x64 by the runtime maps | The wall clock and the session time zone are covered |
| Rust | experimental | measured on x64 by the runtime maps (the standard library), by hand on x64 and x86 (the `chrono` crate) and on x64 (the `jiff` crate) | The wall clock (`SystemTime`) is covered, and so is the session time zone read through the `chrono` crate. The `jiff` crate finds a zone by its name, which the session zone does not have, so a session gives a program built on it the zone in the `TZ` variable. A jiff program started by another program of the session is not recognised and shows UTC. `Instant` stays on the real high-resolution counter unless you opt in with Scale QPC (`--scale-qpc`) |
| Go | not supported | measured on x64, x86 by the runtime maps | Go reads the wall clock straight from a page Windows shares with every process, which no hook reaches, so the application keeps the real date. The report recognises a Go program and says so. The session time zone does reach it |
| Applications reading time from the network | out of scope by definition | measured on x64, x86 | The audit detects it - every connection attempt made through Windows' own socket layer is observed and warned about, whichever function made it (Winsock, WinHTTP, WinINet, and the .NET, Node.js, Go, Java and Python runtimes). A datagram sent without a connection is not a connection and is not counted. A third-party Winsock provider, rare on current Windows, is not watched |
| Embedded web engine inside a native app (WebView2, Qt WebEngine) | experimental | measured on x64 (WebView2 and Qt WebEngine) and x86 (WebView2) by the embedded-engine stand, on hosts of our own | The native hook covers the application and the pages inside it are reached over the engine's debugging port, opened for the session through two environment variables the application inherits. The pages read the session clock at the session rate and follow a speed change and a jump. The engine's helper processes and the renderer's native reads stay on the real clock, so the verdict is PARTIAL and says why. The pages read the session's time zone - a fixed offset, set through the engine's own zone override and checked in every page and worker. An elevated application is out of reach unless you opt in with `--elevated-embedded`, which writes one registry value for the session and removes it (see above) |
| Batch script (`.bat`, `.cmd`) as the target | experimental (command line) | measured on x64 by the test suite, on x86 by hand | Started through the command interpreter from the system folder, which is the process the session covers, together with everything the script starts. Arguments arrive as given, including spaces, `&`, an empty argument and `%` (`%OS%` stays those four characters). A quote inside an argument arrives doubled, and so does the trailing backslash of an argument that needs quotes, which keeps a script that passes `%*` on to a program correct. A command line longer than the interpreter's 8191 characters is refused before anything starts. The window takes a program (`.exe`) or a shortcut to one, not a script |
| Electron / Chromium | experimental (Chromium mode) | measured by hand on an Electron app and on two installed browsers (x64), not by the suite | A separate mechanism, not injection: the app is launched with a debug port and a clean isolated profile, and its own JS time APIs are put on the session clock over the DevTools protocol - reaching the sandboxed renderer and its Web Workers, where the timer often lives. A browser built on Chromium and installed on Windows, which keeps the engine in a folder named after its version, is started in this mode too, without its first-run windows. The pages read the session's time zone - a fixed offset, set through the engine's own zone override and checked in every page and worker. A launcher that hands the app over to another process and exits does not end the session. The main process of an Electron app runs JavaScript in Node outside its windows and reads the real clock, which the report says. Chromium slows the timers of a minimized window to about once a second, which the report says unless the session starts with `--keep-background-timers` |
| UWP / MSIX (Store apps) | not supported | declared (not exercised) | Packaging and launch model |
| ARM64 | out of scope through v1.0 | declared (not exercised) | To be revisited |

**"Experimental" has a defined meaning:** it works on our test targets, it has not been tested broadly,
and the verifier is your only source of truth. It does not mean "should work".

---

## This is not a licence bypass tool

Chrono Mock is built for testing software you are responsible for. Every example, every preset and
every piece of documentation reflects that, and none of them will ever demonstrate extending someone
else's trial period.

A feature pointed in the opposite direction is planned for a later version: a mode that runs a series
of time manipulations against your own application and reports which ones your protection caught. If
you build licensing systems, that is the reason to watch this project.

---

## Contributing

The substitution layer is not something a configuration file can extend. One catalogue is different:
**business-day and holiday calendars per country.**

Knowing that Corpus Christi is a public holiday in Poland, that German holidays vary by state, or how
UK bank holidays work is knowledge scattered across the world and unobtainable by one author. It is
also required to test "next business day" logic, which sits inside every financial and logistics
system.

The United States and Poland ship as reference implementations, chosen because between them they
exercise every rule type: fixed dates, nth-weekday-of-month, dates calculated from Easter, and weekend
observation rules. Adding a country is a data file with sources cited - no code changes. If code
changes turn out to be needed, that is a bug in the data model.

Interface translations are welcome on the same terms, with one caveat stated up front rather than
discovered afterwards: the window has no language switcher yet, so a translation lands in the
repository before a user can select it.

**[CONTRIBUTING.md](CONTRIBUTING.md)** has the file format, the three rule types and what makes a
calendar acceptable. Behaviour here follows the [Code of Conduct](CODE_OF_CONDUCT.md), and security
problems go through [SECURITY.md](SECURITY.md) rather than the issue tracker.

---

## License

GPL-3.0. See [LICENSE](LICENSE). Free, no account, no telemetry, and it never talks to the internet.

The window has two controls that lead outside it, both in the title bar: **Donate** and **GitHub**.
Pressing one hands its address (`https://donislawdev.com/support/` or
`https://github.com/donislawdev/ChronoMock`) to your browser, and your browser is what connects. Chrono
Mock opens no socket either way, nothing it measured about your application goes anywhere, and each button
says all of this in its tooltip before you press it.

Chrono Mock is built with an AI-assisted workflow.

---

<sub>Keywords: RunAsDate alternative · libfaketime for Windows · test date change Windows application ·
fake system time for one process · change date for a single program · test license expiration without
changing system clock · simulate future date · speed up time for testing · test daylight saving time
change · QA date testing tool Windows</sub>

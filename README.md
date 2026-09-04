# Chrono Mock ⏱ - run any Windows application at a different date (without touching the system clock)

[![CI](https://github.com/donislawdev/ChronoMock/actions/workflows/ci.yml/badge.svg)](https://github.com/donislawdev/ChronoMock/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/donislawdev/ChronoMock?sort=semver)](https://github.com/donislawdev/ChronoMock/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/donislawdev/ChronoMock/total)](https://github.com/donislawdev/ChronoMock/releases)
[![License: GPLv3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078D6)

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

<!-- SCREENSHOT: the session panel - the two clocks side by side (app clock vs real clock, both with
     their zone), the verdict line, and the speed controls. This is the single most convincing image
     of the tool. A short GIF of x1440 running the app's clock forward would be even better. -->

**What it can do**

- **Any moment, absolute or relative** - a specific date and time, or `+30 days`, `-1 year`.
- **Flowing, frozen, or accelerated** - run from the shifted point at normal speed, hold the clock
  still, or multiply it by any whole number up to a million (x1440 turns a day into a minute).
- **Jump mid-session** - forward or backward, including the backward jump a laptop makes after
  resyncing with a time server, which breaks anything measuring elapsed time without a guard.
- **Its own time zone** - a fixed offset for that process only, while the system zone stays put.
- **Child processes come along** - installers and launchers spawn children, and without that the test
  covers only half of what ran.
- **Tells you if it worked** - a verdict, the channels covered with call counts, the channels missed,
  and warnings with consequences. [See below](#how-do-you-know-it-worked---the-time-source-audit) - this
  is the part no other tool does.
- **A date calculator built for QA** - month-end, quarter close, the 2038 boundary, "the first day
  after a 14-day trial", business days and holidays, every format at once.
- **Electron and Chromium apps too** - through a different mechanism, because their clock lives in a
  sandboxed renderer where injection does not reach.
- **A window and a command line** - the same engine behind both, exit codes for CI.
- **Portable** - no installer, no administrator rights, runs from a USB stick.

---

## Download and run

Grab the latest build from the **[Releases page](https://github.com/donislawdev/ChronoMock/releases/latest)**:

| File | What it is | Size |
|---|---|---|
| `ChronoMock-win-x64.zip` | The desktop app, self-contained - no .NET install needed | ~69 MB |
| `chrono-cli-win.zip` | Just the command-line tool, for CI and scripts | ~1.4 MB |

Unzip anywhere and run `ChronoMock.exe` (or `chrono.exe` for the CLI). There is no installer, nothing
is written to the registry, and no administrator rights are needed.

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

**In the window:** pick the application, click a scenario (`Year rollover`, `Last day of month`,
`2038 boundary`, ...) or type a date, choose a speed, press Start. The panel then shows the
application's clock and the real clock side by side, the verdict, and which time channels the
application is actually reading.

<!-- SCREENSHOT: the setup half of the panel - target row, the scenario chips, the date field and the
     speed selector. Shows in one image that you do not have to type a date. -->

**From the command line:**

```bash
# Run an application as if it were the last second of 2027
chrono run "C:\apps\Ledger.exe" --at 2027-12-31T23:59:59

# Watch a 30-day trial expire while you make coffee: x1440 is a day a minute
chrono run "C:\apps\Ledger.exe" --mode x1440

# Use a named scenario instead of a date, and write the evidence to a file
chrono run "C:\apps\Ledger.exe" --preset year-rollover --report session.txt

# Countdowns and timers are on a separate axis - add --scale-duration to speed those up too
chrono run "C:\apps\Timer.exe" --mode x60 --scale-duration
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

Every command speaks `--json` as well, and exits with a code your pipeline can branch on.

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
  holiday landing on a Saturday is observed on the preceding Friday

Any calculated date goes straight into a time-shift session with one click.

<!-- SCREENSHOT: the calculator screen - presets on the left, the step builder in the middle, and the
     formats column with "what this date tests" underneath. -->

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

- **A jump changes what the application sees, not when it wakes up.** Timers already scheduled with
  the kernel run on real time. Most applications poll the clock, so jumps work in practice - but not
  all of them do
- **The session time zone is a fixed offset with no daylight saving.** A session set to Poland stays
  at the offset you chose whether its clock is in March or in July, so a run that crosses a real DST
  boundary drifts an hour from what that zone would really show. Forcing an application through a DST
  transition is therefore not something this tool does yet, and the date calculator says so rather
  than guessing
- **The zone reports itself as "Chrono Session"**, a name no Windows registry knows. Anything that
  maps that name back to a zone (.NET's `TimeZoneInfo.Local` among them) falls back to the offset
  instead, which is the right answer - but a target that insists on a registry name will not find one
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

Chrono Mock leaves nothing behind in your system - no persistent hooks, no registry entries, and
nothing outside its own folder except three things it will name for you: the session history and the
diagnostics log, which move to `%LOCALAPPDATA%\ChronoMock\` when the tool's own folder is not writable
(a USB stick, or Program Files without admin), and, for an Electron or Chromium target, a throwaway
browser profile under `%TEMP%` that is deleted when the session ends - and reported in the session
summary if it could not be. That guarantee covers the tool. It does not, and cannot, cover the
application under test.

---

## Antivirus and Microsoft Defender

Chrono Mock shifts time by injecting a small library into the target process. That is a legitimate,
documented Windows technique - and it is also one that malware uses, so antivirus software, Microsoft
Defender included, may flag or quarantine the injected library or block the injection outright. This
is a false positive triggered by the technique, not by anything the tool does to your machine.

If a session fails to start and Defender reports a threat, add a Defender exclusion for the Chrono
Mock folder (Windows Security, Virus and threat protection, Manage settings, Exclusions), or run it on
a machine where you are permitted to do so. The tool is open source under GPL-3.0 - the injected
library is `chrono_hook.dll`, built from the code in this repository, and you can rebuild it yourself.

The Chromium / Electron mode does not inject at all. It launches the target with a debug port and a
clean isolated profile (see the support matrix), so it is not affected by this.

---

## Support matrix

_Most rows come from the automated test suite, last run on 2026-09-03 (x64 and x86). The Python and
Electron rows were verified by hand instead - the suite does not exercise those two yet, and the Basis
column says which is which._

| Environment | Status | Basis | Notes |
|---|---|---|---|
| Native Win32 / Win64 (C, C++, Delphi) | supported | measured on x64, x86 | The cleanest case |
| .NET (Framework and modern) | experimental | measured on x64, x86 | Time calls go through Win32 exports and are covered, including the session time zone. Stopwatch stays on the real high-resolution counter unless you opt in with Scale QPC (`--scale-qpc`), which accelerates it too |
| Java (JVM) | experimental | measured on x64, x86 | Wall clock and elapsed time are covered. The session time zone is not reached - a known gap. nanoTime stays on the real high-resolution counter unless you opt in with Scale QPC (`--scale-qpc`) |
| Python (CPython, incl. PyInstaller) | experimental | measured by hand on x64, not by the suite | Wall clock (time.time, datetime) is covered. perf_counter, and monotonic on Python 3.13+, are on the high-resolution counter - real by default, accelerated when you opt in with Scale QPC (`--scale-qpc`) |
| Applications reading time from the network | out of scope by definition | the audit detects it | connect observed, warned |
| Electron / Chromium | experimental (Chromium mode) | measured by hand (Pomotroid, x64), not by the suite | A separate mechanism, not injection: the app is launched with a debug port and a clean isolated profile, and its own JS time APIs are put on the session clock over the DevTools protocol - reaching the sandboxed renderer and its Web Workers, where the timer often lives. The session zone follows the host zone (the instant is faked, not the local-time getters) |
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

Interface translations are welcome on the same terms.

---

## License

GPL-3.0. See [LICENSE](LICENSE). Free, no account, no telemetry, and it never talks to the internet.

---

<sub>Keywords: RunAsDate alternative · libfaketime for Windows · test date change Windows application ·
fake system time for one process · change date for a single program · test license expiration without
changing system clock · simulate future date · speed up time for testing · test daylight saving time
change · QA date testing tool Windows</sub>

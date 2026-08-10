# Chrono Mock

**Run any Windows application as if it were a different date — without touching the system clock.**
Speed time up, and a 30-day expiry test takes half an hour instead of 30 days.

> **Status: pre-alpha. Nothing to download yet.**
> This repository currently holds design documents. The core time-substitution layer has not passed its technical feasibility gate. If that gate fails, this project stops — and this README will say so.

---

## Why this exists

A huge class of application behaviour depends on the date: licence expiry, trial periods, subscription renewals, certificate validity, token lifetimes, cache expiration, month-end and year-end reports, interest accrual, reminders.

Testing any of it means moving time. Today there are two options, and both are bad:

**Change the system clock.** It breaks domain logon and Kerberos, invalidates TLS certificates so the browser stops working, confuses Windows Update, makes every other application on the machine write wrong timestamps, and leaves a mess behind when you set it back. On a domain-joined machine, group policy usually blocks the change or reverts it minutes later — so the test isn't even repeatable.

**Wait.** Which means: don't test it.

So date-dependent tests get skipped, and "the licence never expires" ships to the customer.

---

## What Chrono Mock does

Time is shifted **for one process only**. The domain, certificates and every other application keep seeing the real date and keep working.

- **Absolute or relative time** — a specific moment, or `+30 days`, `−1 year`
- **Time modes** — flowing normally from the shifted point, frozen, **accelerated or slowed (×0.1 to ×1440)**
- **Independent time zone** per process, without touching the system zone
- **Jump forward or backward mid-session** — including the backward jump a laptop performs after resyncing with a time server, which breaks anything that measures elapsed time without guarding against negative values
- **Child process inheritance** — installers and launchers spawn children, and without this the test is incomplete
- **Session panel** — the app's clock and the real clock side by side, both with their time zone spelled out
- **Portable** — no installer, no administrator rights, runs from a USB stick

If you have used **RunAsDate**, this is the same idea with the parts that were missing: time acceleration, independent time zones, and — most importantly — a way to find out whether the substitution actually worked.

On Linux, `libfaketime` has done this well for years. On Windows there has been nothing comparable.

---

## The part nobody else does: the time source audit

**How do you know the substitution worked?**

With every other tool, you don't. The application starts, it shows some date, and you assume. But an application can read the clock through a channel the tool never intercepted — from a server, from a database, through a runtime layer — and then "the test passed" means nothing at all.

Chrono Mock answers the question directly:

- A verdict before the real test begins: **works / works partially / does not work**
- Which time channels were covered, and how many times the application queried each
- **Which channels were not covered** — stated plainly, not omitted
- Warnings with consequences attached: *"this application reads time from the network — local substitution may not be enough"*

**When the verdict is "does not work", the process is stopped immediately** — before you can interact with it. The verdict cannot precede the launch, because the only source of truth is a reading from inside the running process; what Chrono Mock guarantees is that you learn the truth before you start working, not that a doomed launch never happens. You can override and continue, but the session is then marked as unreliable everywhere it appears — in history, and in any evidence you export.

A silent failure is the one outcome this tool refuses to produce. An application that looks time-shifted but isn't is worse than one that never launched.

---

## Built-in date calculator

The other half of date testing is knowing *which* date to use. Chrono Mock includes a calculator that speaks in QA terms rather than arithmetic:

- **Build the expression from controls, not from a sentence** — a starting point plus steps (`− 18 years`, `− 1 day`, `set time 23:59:59`), with the intermediate result shown after each step
- **Presets that explain what they catch** — age-of-majority boundary, trial end, quarter close, the nonexistent hour on a DST spring-forward, epoch 0, the 2038 boundary
- **Every format at once**, one click to copy — ISO 8601, `MM/dd/yyyy`, `dd.MM.yyyy`, epoch seconds and milliseconds, `FILETIME`, RFC 1123, and a custom mask to match whatever the application under test expects
- **Reverse direction** — paste a date from a log and get *"3 days before quarter end"*, *"Saturday, not a business day"*. When the format is ambiguous, both readings are shown rather than one guessed
- **Business days and holidays** — United States and Poland at launch, including the US rule where a holiday landing on a Saturday is observed on the preceding Friday

Any calculated date goes straight into a time-shift session with one click.

---

## Honest limits

This section is longer than most tools' entire documentation, on purpose. A tool that quietly fails to cover something is worse than one that says what it cannot do.

**Time sources that cannot be intercepted from user mode:**

- Direct reads of the shared user-mode data page — the value sits in memory rather than behind a function call, so there is nothing to hook. Rare in ordinary applications, common in licensing and anti-tamper code, which is exactly where testing matters most
- Direct system calls that bypass the standard exports — same pattern, same class of application
- Time obtained out-of-process, through WMI, out-of-process COM servers or RPC
- Time from the network: an HTTP `Date` header, NTP, an API response, or a database server's own clock
- Time from kernel drivers

**Other limits:**

- **A jump changes what the application sees, not when it wakes up.** Timers already scheduled with the kernel run on real time. Most applications poll the clock, so jumps work in practice — but not all of them do
- **Chrono Mock cleans up after itself. It cannot clean up after the application you tested.** See the warning below
- Windows only. No macOS, no Linux — `libfaketime` already covers Linux well

---

## ⚠️ Before you run an application in the future

An application running in 2028 will write 2028 dates wherever it stores its data: its database, its configuration, its cache, its licence file. Those values stay there after you return to real time.

For applications with licence protection this can be permanent. "Clock rollback detected → licence invalidated" is a standard industry pattern, and it will do exactly what it was designed to do.

**Back up the application's data directory before your first future-dated session.**

Chrono Mock leaves nothing behind in your system — no persistent hooks, no registry entries, no files outside its own folder. That guarantee covers the tool. It does not, and cannot, cover the application under test.

---

## Support matrix

> ⚠️ **Pre-alpha: this table is a plan, not a result.** Once the tool exists it will be generated from actual test runs, and this note will be removed. Until then every row is an expectation that the feasibility spike may overturn — treat it accordingly.

| Environment | Status | Notes |
|---|---|---|
| Native Win32 / Win64 (C, C++, Delphi) | supported | The cleanest case |
| .NET (Framework and modern) | experimental | Calls go through Win32 exports, but the time zone is read from the registry and hook timing against runtime startup is critical |
| Java (JVM) | experimental | Time zone cached at startup; elapsed time via the high-resolution counter |
| Electron / Chromium | not supported | Multi-process sandboxed architecture blocks injection into renderers |
| UWP / MSIX (Store apps) | not supported | Packaging and launch model |
| Applications reading time from the network | out of scope by definition | The audit detects this and says so |
| ARM64 | out of scope through v1.0 | To be revisited |

**"Experimental" has a defined meaning:** it works on our test targets, it has not been tested broadly, and the verifier is your only source of truth. It does not mean "should work".

---

## This is not a licence bypass tool

Chrono Mock is built for testing software you are responsible for. Every example, every preset and every piece of documentation reflects that, and none of them will ever demonstrate extending someone else's trial period.

A feature pointed in the opposite direction is planned for a later version: a mode that runs a series of time manipulations against your own application and reports which ones your protection caught. If you build licensing systems, that is the reason to watch this project.

---

## Contributing

The substitution layer is not something a configuration file can extend. One catalogue is different: **business-day and holiday calendars per country.**

Knowing that Corpus Christi is a public holiday in Poland, that German holidays vary by state, or how UK bank holidays work is knowledge scattered across the world and unobtainable by one author. It is also required to test "next business day" logic, which sits inside every financial and logistics system.

The United States and Poland ship as reference implementations, chosen because between them they exercise every rule type: fixed dates, nth-weekday-of-month, dates calculated from Easter, and weekend observation rules. Adding a country is a data file with sources cited — no code changes. If code changes turn out to be needed, that is a bug in the data model.

Interface translations are welcome on the same terms.

---

## License

GPL-3.0. See [LICENSE](LICENSE).

---

<sub>Keywords: RunAsDate alternative · libfaketime for Windows · test date change Windows application · fake system time for one process · change date for a single program · test license expiration without changing system clock · simulate future date · speed up time for testing · test daylight saving time change · QA date testing tool Windows</sub>

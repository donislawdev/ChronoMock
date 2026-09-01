# Chrono Mock

Run a Windows application as if it were a different date - without touching the system
clock - and calculate test dates. This is the portable desktop app: no installer, no
administrator rights, runs from this folder (or a USB stick). Licence: GPL-3.0 (see
LICENSE). Third-party notices: THIRD-PARTY-NOTICES.md.

## Getting started

1. Run `ChronoMock.exe`.
2. **Choose…** the application you want to test (a `.exe`).
3. Set the date under **At**, pick a **Zone** and a **Mode** (flowing at real speed, frozen,
   or a speed-up), then press **Start**. The application launches with its clock shifted;
   the panel shows the fake clock and the real clock side by side, each with its zone.
4. Read the **verdict**: works / partial / does not work. It tells you whether the
   substitution actually took effect - that is the point of the tool, not just that the app
   launched. The covered and uncovered time channels are listed below it.
5. **Copy summary** puts a paste-ready report on the clipboard for a ticket. A session that
   is anything other than a clean "works" is marked as unreliable evidence there.

The **Calculator** tab (top of the window) computes test dates - trial ends, month-end,
business days, and more - and can send a date straight into a session.

## Antivirus / Microsoft Defender

Time substitution injects a small library into the target process. That is a legitimate,
documented Windows technique, and it is also one that malware uses, so antivirus software -
Microsoft Defender included - may flag the injected library or block the injection. This is
a false positive triggered by the technique, not by anything the tool does to your machine.

If a session fails to start and Defender reports a threat, add a Defender exclusion for this
folder (Windows Security, Virus and threat protection, Manage settings, Exclusions), or run
it on a machine where you are permitted to do so. The Chromium / Electron mode does not
inject and is not affected.

## ⚠️ Before you run an application in a future date

An application running in the future writes future dates into its own data - its database,
configuration, cache, and licence file - and those values stay there after you return to
real time. For licence-protected applications this can be permanent ("clock rollback
detected" is a standard protection pattern).

**Back up the application's data directory before your first future-dated session.** Chrono
Mock leaves nothing behind in your system, but it cannot clean up after the application you
tested.

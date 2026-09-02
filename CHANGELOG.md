# Changelog

All notable changes to Chrono Mock are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Internal preview - a portable build is distributed to QA teams; there is no public release yet.

### Added

- **Time substitution for one process.** Run a Windows application as if it were a different date and
  time, without touching the system clock. Absolute or relative moments; a flowing, frozen, or
  integer-accelerated clock (e.g. ×60, ×1440); an independent session time zone per process (a fixed
  offset, no daylight saving); and forward or backward jumps mid-session.
- **The time source audit.** A verdict - works / partial / does not work - reporting whether the
  substitution actually took effect: which time channels were covered and how often, which were not, and
  warnings with consequences (for example, an application reading time from the network). A session that
  is anything other than a clean "works" is marked as unreliable evidence wherever it appears.
- **Child process inheritance.** Spawned children join the session, so installers and launchers are
  tested whole.
- **Chromium / Electron mode.** For an application whose clock lives in a sandboxed renderer, the target
  is driven over the Chrome DevTools Protocol instead of by injection - launched with a debug port and a
  clean isolated profile.
- **Date calculator.** Build a test date from a starting point plus steps (shift, snap to a period
  boundary, nearest business day, set time, change zone), with presets, reverse analysis of a pasted date
  (both readings when it is ambiguous), business-day and holiday calendars (United States and Poland),
  and every common format at once. Any calculated date can be sent straight into a substitution session.
- **Two surfaces.** A desktop app (WPF, dark theme) and a command-line tool (`chrono`), both portable -
  no installer, no administrator rights.

### Notes

- Windows only. Native substitution injects a small library, which antivirus (Microsoft Defender
  included) may flag as a false positive - add an exclusion for the Chrono Mock folder. The
  Chromium/Electron mode does not inject and is unaffected.

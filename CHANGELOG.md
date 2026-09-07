# Changelog

Notable changes to Chrono Mock, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`chrono version`**, also spelled `--version` and `-V`. Prints the build, which of the two
  cores that executable is, and the wire protocol it speaks, on stdout so a script can capture
  it. Until now the tool could not answer the first question any bug report asks.
- **The version in the window title and title bar**, so a screenshot says which build it came
  from without anyone having to ask.

### Fixed

- **Martin Luther King Jr. Day was treated as a holiday in every year**, in both United States
  calendars, so any date calculation reaching back before 1986 was wrong. The holiday was created
  by Public Law 98-144, which took effect on the first 1 January after a two-year period, so it
  now carries `valid_from: 1986` and the third Monday of January 1985 is an ordinary business day
  again. The entry previously cited a source document that is not part of this repository, which
  no reader could follow.
- Four Polish holiday sources used a semicolon in prose, and the Polish strings file described
  itself as the English one.

## [0.1.0] - 2026-09-04

First public release. There is no previous version to compare it against, so rather than
listing everything as "added", here is where to find out what it does:

- **[README](README.md)** - what it is, how to run it, and the honest limits.
- **[chronomock.donislawdev.com](https://chronomock.donislawdev.com/)** - the same in longer
  form, including how the time source audit works and how this compares with RunAsDate and
  libfaketime.

From the next release onwards this file records what changed.

[0.1.0]: https://github.com/donislawdev/ChronoMock/releases/tag/v0.1.0

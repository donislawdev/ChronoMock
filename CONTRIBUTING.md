# Contributing to Chrono Mock

Most of this tool is not something an outside contribution can usefully extend.
The substitution layer is hooks into Windows time APIs, written against measured
behaviour of specific runtimes, and a change there is a change to the one thing
the product promises.

Two things are different, and for both of them the bottleneck is knowledge rather
than code.

## Holiday and business-day calendars

**This is the contribution the project actually needs.**

Knowing that Corpus Christi is a public holiday in Poland, that German holidays
vary by state, or how UK bank holidays work is knowledge scattered across the
world and unobtainable by one author. It is also required to test "next business
day" logic, which sits inside every financial and logistics system.

The United States and Poland ship as reference implementations, chosen because
between them they exercise every rule type. Adding a country is **a data file with
sources cited, and no code changes**. If your country needs a code change, that is
a bug in the data model and worth an issue on its own.

### What a calendar file looks like

Drop a new file into `calendars/`, named for its identifier. Start from
[`calendars/pl.json`](calendars/pl.json) or
[`calendars/us-federal.json`](calendars/us-federal.json) rather than from this
description, because they are the tested examples.

```json
{
  "schema": "chronomock.calendar/1",
  "stability": "unstable",
  "id": "pl",
  "country": "PL",
  "subregion": null,
  "name": { "en": "Poland", "local": "Polska" },
  "weekend": ["saturday", "sunday"],
  "observed": "none",
  "holidays": [
    {
      "id": "new_years_day",
      "name": { "en": "New Year's Day", "local": "Nowy Rok" },
      "rule": { "type": "fixed", "month": 1, "day": 1 },
      "valid_from": null,
      "valid_to": null,
      "source": "Non-working Days Act of 18 January 1951 (Poland)"
    }
  ]
}
```

Three rule types cover everything shipped so far:

| Type | Shape | Example |
|---|---|---|
| `fixed` | `month`, `day` | New Year's Day |
| `nth_weekday` | `month`, `weekday`, `order` - `order` is `-1` for the last one in the month | Memorial Day, the last Monday in May |
| `easter_offset` | `offset` in days from Easter Sunday, positive or negative | Easter Monday is `1` |

The `observed` field says what happens when a holiday lands on a weekend, and it
applies to the whole calendar: `none`, `sun_to_mon`, or `sat_to_fri_sun_to_mon`
- the United States rule where a Saturday holiday is observed on the preceding
Friday.

`valid_from` and `valid_to` are years, and they matter more than they look. Poland
restored Epiphany as a non-working day in 2011, and a calendar that ignores that
gives a wrong answer for every date before it.

### What makes a calendar acceptable

- **Every holiday carries a `source`.** A statute, an official schedule, or a
  government page. "Everyone knows it is a holiday" is the one thing this file
  cannot say, because the reader is a test that will be trusted.
- **Say what you are unsure about** in the pull request rather than rounding it
  off. A holiday whose first observance year you could not confirm is fine when it
  is labelled as such - a confidently wrong `valid_from` is not.
- **Bank holidays and public holidays are often different lists.** The two United
  States files exist for exactly that reason. If your country has both, two files
  is the right answer.
- Run `cargo test --workspace` before opening the pull request. The calendar is
  parsed and validated by the suite, so a malformed file fails there rather than
  in somebody's test run.

## Interface translations

Translations are welcome, and there is one thing you should know before you spend
an evening on one.

**The application does not have a language switcher yet.** It loads
`gui/ChronoMock.App/Localization/Strings.en.json` and applies English on start.
A Polish file already sits beside it and is complete, and nothing in the interface
can currently select it. So a translation you contribute today lands in the
repository, is kept correct, and is not reachable by a user until the switcher is
built.

That is stated up front rather than discovered afterwards. If you would rather wait
for the switcher, that is a reasonable choice, and an issue asking for it is a
useful contribution in itself.

Two things worth knowing if you translate anyway:

- **The command line is English only, on purpose.** Only the window is translated.
- **The locale of the data is not the language of the interface.** A date shown in
  Polish format is a formatting choice for the application under test, not a
  statement about the language of the window.

## Something else that helps, and costs nothing

The support matrix in the README marks .NET, Java, Python and Electron as
**experimental**, which means measured on our targets rather than broadly. If you
run Chrono Mock against a real application and the audit reports something the
matrix does not predict - a channel missed on a runtime we list as covered, or
covered on one we do not - open an issue with the audit output. That is direct
evidence about the one claim this tool is built to make honestly, and no amount of
work here substitutes for it.

## What is not accepted

- **Anything that changes the system clock**, in any mode or as a fallback.
- **Anything aimed at defeating licence protection in software somebody else
  wrote.** This tool is for testing software you are responsible for.
- **Support for another operating system.** The mechanism is Windows API hooking,
  and `libfaketime` already covers Linux well.
- **A capability that exists in only one surface.** The window and the command line
  are two clients of one engine, and a feature belongs in the engine.

## Building and checking

You need the Rust toolchain with both Windows targets, and the .NET SDK. The
workspace is on Rust edition 2024, so a recent stable toolchain is required, and
the .NET side targets `net10.0-windows`. There is no `rust-toolchain` file pinning
an exact Rust version - if a clippy release ever moves a measurement, that is
handled by adjusting the ceiling in a commit that says so rather than by pinning.

```bash
rustup target add i686-pc-windows-msvc
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For a calendar or a translation those three are enough. The full set the CI
workflow runs, if you touched code:

```bash
dotnet format gui/ChronoMock.slnx --verify-no-changes
dotnet test --solution gui/ChronoMock.slnx --filter-not-trait "Category=Integration"
cargo run -p chrono-site -- --strict
cargo build --release --target x86_64-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
```

There is also an end-to-end harness that injects into live processes and needs a
JDK, the .NET Framework and hand-built probe programs. It does not run in CI and it
is not something a contributor is expected to set up.

### Conventions the test suite enforces

These are guards rather than preferences, so a pull request that breaks one fails
before anyone reviews it:

- **Everything inside the repository is English**, including comments. The
  criterion is the place rather than the reader.
- **A flat hyphen, never an em or en dash**, and no semicolons in prose.
- **User-visible text in the window is a translation key**, never a literal.
- **Nothing in the repository sets the system clock.**
- **Nothing reaches the network.** Every network API in the workspace is registered
  by file with the reason it is there, and the register is short: a local socket for
  the Chromium debug port, and launching the process under test. A use anywhere else
  fails, and so does a dependency that could speak to a network.
- **The crate dependency direction is fixed** and checked, so a new dependency
  edge fails the suite until it is declared.

## Licence and conduct

Contributions are licensed under **GPL-3.0**, the same as the project. By opening a
pull request you agree that your contribution ships under it.

Behaviour here is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security
problems go through [SECURITY.md](SECURITY.md) rather than the issue tracker.

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

Drop a new file into `crates/dates/catalogue/calendars/`, named for its identifier: a file whose
`id` is not its file name is refused. Start from
[`pl.json`](crates/dates/catalogue/calendars/pl.json) or
[`us-federal.json`](crates/dates/catalogue/calendars/us-federal.json) rather than from this
description, because they are the tested examples. The calendars live with the date library, so the
program and the library packages ship the same files.

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
  "valid_from": 2002,
  "law_as_of": "2026-10-06",
  "source": "Range from 2002, the first full year of the five-day working week ...",
  "holidays": [
    {
      "id": "new_years_day",
      "name": { "en": "New Year's Day", "local": "Nowy Rok" },
      "rule": { "type": "fixed", "month": 1, "day": 1 },
      "valid_from": null,
      "valid_to": null,
      "source": "Non-working Days Act of 18 January 1951 (Dz.U. 1951 nr 4 poz. 28), art. 1"
    }
  ]
}
```

Three fields say which years the calendar answers for, and all three are required:

- **`valid_from`** - the first year for which every rule in the file has a source: the holidays, and
  also the weekend and the `observed` rule. Poland's calendar starts in 2002, the first full year of
  the five-day working week in its Labour Code, because before it Saturday was a working day, and a
  holiday list alone cannot say that. A business-day step that reaches a day before this year is
  refused, and a date before it gets no business-day or holiday judgement. Go back as far as your
  sources go, and no further.
- **`law_as_of`** - the day you checked the holiday list against the law, `YYYY-MM-DD`. A result
  after that day, computed more than a year after it, is marked as possibly missing a holiday added
  since. Update it whenever you check the list again, even if nothing changed.
- **`source`** - where the range, the weekend and the observance rule come from. Each holiday keeps
  its own `source` too.

A holiday whose `valid_to` falls before the calendar's `valid_from` is refused: it could never apply.

Three rule types cover everything shipped so far:

| Type | Shape | Example |
|---|---|---|
| `fixed` | `month`, `day` | New Year's Day |
| `nth_weekday` | `month`, `weekday`, `order` - `order` is `-1` for the last one in the month | Memorial Day, the last Monday in May |
| `easter_offset` | `offset` in days from Easter Sunday, from `-80` to `250` - the range that keeps the holiday in its Easter's year | Easter Monday is `1` |

The file has exactly the fields shown above. A field the reader does not know is refused rather than
skipped, at every level, so a misspelt `valid_form` stops the file instead of quietly making a
holiday count in every year.

The `observed` field says what happens when a holiday lands on a weekend, and it
applies to the whole calendar: `none`, `sun_to_mon`, `sat_to_fri_sun_to_mon`
- the United States rule where a Saturday holiday is observed on the preceding
Friday - or `weekend_to_mon`. When the day a holiday would be observed on is
already off, because of another holiday or a weekend day, it moves on to the next
free day in the same direction.

A holiday's own `valid_from` and `valid_to` are years, and they matter more than they look. Poland
restored Epiphany as a non-working day in 2011, and a calendar that ignores that
gives a wrong answer for every date before it. A single day off made by its own act is a `fixed`
rule with the same year in both, like Poland's 12 November 2018.

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
- Run `cargo test --workspace` before opening the pull request. Every file in
  `crates/dates/catalogue/calendars/` is parsed and validated by the suite, so a malformed file fails
  there rather than in somebody's test run.

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

## Website translations

The website in `site/` is built by `crates/site` and is published in 22 languages.
Unlike the window, every one of them is reachable by a visitor today, so a correction
to a page is worth more than a new language: read yours as a native speaker would and
open a pull request for anything that sounds machine-made. That includes the
marketing wording, which is the part most likely to read wrongly.

To add a language, copy a finished one and keep its structure:

- `site/i18n/<code>.json` - the interface strings, with the `html_lang`, `dir`
  and `og_locale` values.
- `site/pages/<page>/<code>.html` for every page, plus a matching entry under
  `languages` in each `page.json` (slug, link text, title, description).
- The code in the `languages` list of `site/site.json`, which is also the order of
  the language menu.

Translate the prose and the `#` comments inside the terminal blocks, and leave
commands, flags, code and program output exactly as they are - the program is
English only. Internal links name their own language, so a page links to
`/<code>/<slug>/`, never to the English one.

**Slugs are permanent.** GitHub Pages cannot redirect, so a published address that
is renamed later is a dead link for everybody who saved it. The existing languages
use a translated ASCII slug where the language is written in the Latin alphabet and
the English slug everywhere else - follow that rather than choosing again.

`cargo run -p chrono-site -- --strict` fails on an untranslated page, an unresolved
`{{token}}`, a dead link or a page nothing links to, and `cargo test -p chrono-site`
checks the hreflang pairs, the sitemap and the language menu.

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

You need the Rust toolchain with both Windows targets, the .NET SDK, and Node.js
on `PATH`. The workspace is on Rust edition 2024, so a recent stable toolchain is
required, and the .NET side targets `net10.0-windows`. `cargo test` runs the
Chromium time shim in Node (`crates/cli/src/cdp/shim_exec.rs`), and a machine
without `node` fails those tests rather than skipping them. They are run with
Node 22, the version on the CI image, and Node 26. There is no `rust-toolchain` file pinning
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

### Mutation testing

A green suite says the code does what the tests check, not that the tests check
anything. So every guard here has been broken on purpose to see that it notices,
and those breaks are kept in `tests/mutation/entries.json`: one entry per break,
with the text it changes, the file it changes it in and the test that must go red.

A pull request runs the entries its change reaches - a file an entry mutates, the
file declaring an entry's test, or a new or changed entry - and the `mutation`
workflow reports the result in its summary. That report never fails a pull request.
Once a week every entry runs, in sixteen parts side by side, and that run is the
one that goes red.

If you add a guard, add the entry that proves it can fail. A local run names its
entries or compares with a base - a run of every entry takes hours and is refused
unless asked for with `-All`:

```bash
pwsh -File tests/mutation/mutate.ps1 -Only "r4-18 sign reversed"
pwsh -File tests/mutation/mutate.ps1 -Base origin/main
```

## Licence and conduct

Contributions are licensed under **GPL-3.0**, the same as the project. By opening a
pull request you agree that your contribution ships under it.

Behaviour here is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security
problems go through [SECURITY.md](SECURITY.md) rather than the issue tracker.

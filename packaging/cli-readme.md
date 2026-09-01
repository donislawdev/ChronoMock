# Chrono Mock - CLI (`chrono`)

`chrono` runs a Windows application as if it were a different date - without touching the
system clock - and calculates test dates. This is the command-line tool on its own: no
installer, no .NET, no administrator rights. Licence: GPL-3.0 (see LICENSE). Third-party
notices: THIRD-PARTY-NOTICES.md.

## What is here

- `chrono.exe` - the 64-bit tool. Use it for 64-bit target applications and for every `calc`.
- `x86\chrono.exe` - the 32-bit tool. Use it for 32-bit target applications.
- `calendars\`, `presets\` - data, read from beside `chrono.exe` or from the current directory.

Bitness matters only for `run`, which injects a matching-bitness library into the target.
`calc` is pure computation, so either tool gives the same answer.

## Examples

Calculate a date:

    chrono calc --base 2026-07-01T00:00:00 --shift +5bd --calendar us-banking
    chrono calc --preset month-end
    chrono calc --analyze 04/08/2008

Run an application in a fake date:

    chrono run "C:\path\to\app.exe" --at 2038-01-01T00:00:00 --mode x60
    x86\chrono run "C:\path\to\app32.exe" --at 2027-01-01T00:00:00

Add `--json` for machine-readable output. Run `chrono` with no arguments for the full usage.

## Antivirus / Microsoft Defender

Time substitution injects a small library into the target process. Antivirus software,
Microsoft Defender included, may flag this legitimate technique. If `run` is blocked, add a
Defender exclusion for this folder. The Chromium / Electron mode does not inject.

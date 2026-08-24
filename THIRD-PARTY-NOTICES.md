# Third-party notices

Chrono Mock is distributed under GPL-3.0 (see [LICENSE](LICENSE)). It bundles the
third-party components listed below, each under its own licence. This file carries
their notices, because their licences require the notice to travel with every copy.

This file is also the dependency register required by the rule-8 licence sieve
(docs/zasady/09 section 10) for components linked into the shipped binaries.

---

## WPF-UI (`Wpf.Ui.dll`, `Wpf.Ui.Abstractions.dll`)

- Package: `WPF-UI` 4.3.0 (https://www.nuget.org/packages/WPF-UI), project lepoco/wpfui
- Licence: **MIT** - compatible with GPL-3.0 (a legal assessment, not a lawyer's ruling)
- Used by: the C# GUI (`gui/ChronoMock.App`)
- Font redistribution check (verified on 4.3.0 by inspecting the embedded resources of
  `Wpf.Ui.dll`, not the label): the only embedded font files are
  `resources/fonts/fluentsystemicons-filled.ttf` and `-regular.ttf` (FluentSystemIcons,
  MIT). No embedded resource references Segoe Fluent Icons - that name appears only as a
  font-family fallback pointing at the OS-installed font, so no non-redistributable font
  is shipped. **Re-run this check on every version bump.**

```
MIT License

Copyright (c) 2021-2025 Leszek Pomianowski and WPF UI Contributors. https://lepo.co/

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## Pending: native (Rust) shipped dependencies

The native core links Rust crates (for example `minhook` and the `windows` crate) into
the shipped x86/x64 binaries. Their licences are gated by `cargo deny check licenses`,
but their notices are not yet reproduced here. Add them before the first public release
(Stage 5).

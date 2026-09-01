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

## Native (Rust) shipped dependencies

The native cores (`chrono.exe` and the injected `chrono_hook.dll`, both x64 and x86)
statically link the third-party Rust crates below. This is the set of normal (non-build,
non-proc-macro) dependencies of the shipped binaries, verified with `cargo tree -e normal`
and `cargo metadata` on 2026-09-01. Build-time-only crates - proc-macro crates and their
dependencies, `cc`, and the `toml` family - are not linked into the binaries and are not
listed. Re-run that verification on every dependency change.

Every crate below is available under the MIT licence (several are dual MIT OR Apache-2.0,
and we take the MIT option). The single MIT text follows the table. One crate, `minhook`,
also compiles a vendored C library (MinHook) whose separate BSD-2-Clause notice is
reproduced after it. Several MIT crates ship the bare MIT template with no embedded
copyright line, so their attribution below is taken from the crate's declared authors.

| Crate | Version | Copyright / authors |
|---|---|---|
| `itoa` | 1.0.18 | David Tolnay |
| `memchr` | 2.8.3 | Copyright (c) 2015 Andrew Gallant |
| `serde` | 1.0.229 | Erick Tryzelaar, David Tolnay |
| `serde_core` | 1.0.229 | Erick Tryzelaar, David Tolnay |
| `serde_json` | 1.0.151 | Erick Tryzelaar, David Tolnay |
| `zmij` | 1.0.23 | David Tolnay |
| `log` | 0.4.33 | Copyright (c) 2014 The Rust Project Developers |
| `minhook` | 0.9.0 | Copyright (c) 2025 Jakobzs (Rust wrapper - MinHook C notice below) |
| `once_cell` | 1.21.4 | Aleksey Kladov |
| `pin-project-lite` | 0.2.17 | the pin-project-lite authors |
| `tracing` | 0.1.44 | Copyright (c) 2019 Tokio Contributors |
| `tracing-core` | 0.1.36 | Copyright (c) 2019 Tokio Contributors |
| `windows` | 0.62.2 | Copyright (c) Microsoft Corporation |
| `windows-collections` | 0.3.2 | Copyright (c) Microsoft Corporation |
| `windows-core` | 0.62.2 | Copyright (c) Microsoft Corporation |
| `windows-future` | 0.3.2 | Copyright (c) Microsoft Corporation |
| `windows-link` | 0.2.1 | Copyright (c) Microsoft Corporation |
| `windows-numerics` | 0.3.1 | Copyright (c) Microsoft Corporation |
| `windows-result` | 0.4.1 | Copyright (c) Microsoft Corporation |
| `windows-strings` | 0.5.1 | Copyright (c) Microsoft Corporation |
| `windows-threading` | 0.2.1 | Copyright (c) Microsoft Corporation |

### MIT License

```
MIT License

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

### MinHook (vendored C library, compiled into `chrono_hook.dll`)

The `minhook` crate compiles the MinHook C library, which carries its own BSD-2-Clause
licence, including the bundled Hacker Disassembler Engine portions:

```
MinHook - The Minimalistic API Hooking Library for x64/x86
Copyright (C) 2009-2017 Tsuda Kageyu.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

================================================================================
Portions of this software are Copyright (c) 2008-2009, Vyacheslav Patkov.
================================================================================
Hacker Disassembler Engine 32 C
Copyright (c) 2008-2009, Vyacheslav Patkov.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

-------------------------------------------------------------------------------
Hacker Disassembler Engine 64 C
Copyright (c) 2008-2009, Vyacheslav Patkov.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

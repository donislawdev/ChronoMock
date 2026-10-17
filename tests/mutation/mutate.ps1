# Proves the guards can fail: breaks the code on purpose and watches the named test go red.
#
# A green test run is not evidence that a test checks anything. The sentence that is evidence is
# "this test would be red if I broke that", and the only way to know it is to break that and watch.
# This project did exactly that by hand from its first slice, in one throwaway script per slice that
# lived outside the repository and ran on one machine. Since 2026-10-16 every such probe is an entry in
# tests/mutation/entries.json beside this file, and GitHub runs them: every entry once a week, cut into
# sixteen parts that run side by side, and on a pull request the entries the change reaches
# (.github/workflows/mutation.yml). The method comes from a sister project, the
# decisions are this project's own.
#
#     pwsh -File tests/mutation/mutate.ps1 -Only "r4-18 sign reversed"      # entries whose name holds it
#     pwsh -File tests/mutation/mutate.ps1 -Only "msi scope|guards"           # several, split on |
#     pwsh -File tests/mutation/mutate.ps1 -Base origin/main                  # what this branch reaches
#     pwsh -File tests/mutation/mutate.ps1 -SelfTest                          # its own canaries, no build
#
# A RUN OF EVERY ENTRY IS REFUSED UNLESS IT IS ASKED FOR BY NAME (-All). It takes hours on one machine
# and it is the weekly job's work, a decision of 2026-10-16. Name entries, compare with a base or take
# one part instead.
#
# THE VERDICTS, and why there are this many:
#
#   caught        the named test went red. The guard works.
#   MISSED        the named test ran and stayed green. The guard does not check what it claims to.
#   BROKEN        the mutated tree does not compile, so the run proves nothing either way.
#   STALE         an anchor of the entry is not in its file exactly once. The code moved on.
#   NO TEST       the filter ran no test at all. A test was renamed or removed.
#   BASELINE RED  the named test is red on the UNMUTATED tree, so a red run under the mutant would
#                 prove nothing. The mutant is not even built.
#   TIMEOUT       the test run did not end in its time. A guard that hangs instead of failing has to
#                 be fixed in the test, which must time out and fail rather than wait.
#   ERROR         the test command failed in a way that names no failed test - the run could not say.
#   NOT HERE      not judged by this runner: the entry names the machine it needs (a live browser, the
#                 environment harness, an elevated session, a virtual machine), or under -CiScope its C#
#                 test carries the Integration trait that CI filters out. Listed and never a failure.
#
# BROKEN IS A SEPARATE STEP ON PURPOSE. A build failure also ends a test command with a non-zero code,
# so a runner that only asks whether the test command failed reports a tree that does not compile as a
# caught mutation. Each entry is compiled first, with its own verdict, and two canaries per part - one
# Rust, one C# - must come back BROKEN, or nothing else the part says is believed.
#
# KILLING THIS RUN LEAVES A MUTATED FILE ON DISK. The originals go back in a finally block, and a killed
# process runs none. So before any file is written, its original is kept in a recovery file in the
# temporary folder, and the next run puts it back before it does anything else - or refuses, naming the
# file, when the file changed since. After an interrupted run, `git status` still shows the truth.
#
# THE ORIGINAL IS WRITTEN BACK, NEVER COPIED BACK. A copy keeps the old modification time, cargo then
# takes the file for unchanged, and the next test runs on a binary built from the mutation. Writing the
# bytes gives the file the time of the write.

[CmdletBinding(PositionalBinding = $false)]
param(
    [string] $Root = '',

    # Entries whose name holds any of these fragments, split on a bar and matched as plain text,
    # case aside. Names may hold commas, never a bar (the registry check refuses one).
    [string] $Only = '',

    # The entries a change reaches, compared with this base - anything git understands. The workflow
    # passes the base commit of the pull request. Select-Reached says which ways an entry is reached.
    [string] $Base = '',

    # One part of the selection, written i/k: part i of k, counted from zero. Entry n of the selection
    # goes to part n mod k, so every machine works out its own share from the same commit.
    [string] $Shard = '',

    # Appends entries=, parts= and shards= to this file (GitHub's step outputs), says what was selected
    # and why, and stops. Builds nothing.
    [string] $PlanTo = '',

    # Every entry, on purpose. Without it a run with no -Only, -Base or -Shard is refused.
    [switch] $All,

    # Judges only what CI can judge: a C# test carrying the Integration trait is NOT HERE, and the C#
    # runs leave that trait out, as the CI gate does.
    [switch] $CiScope,

    # Writes the verdicts to this JSON file after every entry, so a part that is stopped half way still
    # says how far it got. tests/mutation/report.ps1 reads these.
    [string] $Json = '',

    # Skips the rebuild of the clean tree at the end. Only for a tree thrown away afterwards, which a CI
    # runner is.
    [switch] $NoClosingBuild,

    # Proves the parsing, the selection, the cut into parts and the recovery on planted cases whose
    # answer is known before the run. Builds nothing and touches nothing in the tree.
    [switch] $SelfTest,

    # How many parts a run of every entry is cut into, the most a pull request gets, and how many
    # entries make one part of a pull request. Sixteen is a decision of 2026-10-16, and twenty
    # a part is the sister project's measurement, to be checked against the first runs here.
    [int] $FullParts = 16,
    [int] $MostPartsOnAPullRequest = 16,
    [int] $EntriesPerPart = 20,

    # How long one test command and one build may take before the run gives up on it.
    [int] $TestMinutes = 15,
    [int] $BuildMinutes = 60
)

$ErrorActionPreference = 'Stop'
# Native exit codes are read here, never turned into errors - a red test is the outcome this runner
# exists to produce.
$PSNativeCommandUseErrorActionPreference = $false
# Numbers read the same on every machine.
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture

if ([string]::IsNullOrWhiteSpace($Root)) {
    $Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
}

. (Join-Path $PSScriptRoot 'json.ps1')

# English output from the .NET tools on every machine, so the summary this runner reads is the same
# words on the maintainer's machine as on a runner. Measured 2026-10-16: without it the summary is in
# the machine's own language.
$env:DOTNET_CLI_UI_LANGUAGE = 'en'
$env:DOTNET_NOLOGO = '1'
$env:CARGO_TERM_COLOR = 'never'

# The random properties (each in a `mod properties`) draw from one fixed seed here, so an entry is
# caught or MISSED by the change it plants and never by the draw - a verdict that flips between two
# runs of one entry is no verdict. And a caught mutant must leave no failure file in the tree the
# next entry builds from: proptest writes one beside the source unless it is told not to.
$env:PROPTEST_RNG_SEED = '20261017'
$env:PROPTEST_DISABLE_FAILURE_PERSISTENCE = '1'

# The files that make every verdict. A change to any of them selects every entry, because a change to
# the judge is a change to every judgement.
$runnerFiles = @('tests/mutation/mutate.ps1', 'tests/mutation/json.ps1', 'tests/mutation/report.ps1', '.github/workflows/mutation.yml')

# The C# side as one build, the solution CI builds before its C# tests.
$csSolution = 'gui/ChronoMock.slnx'

# ---------------------------------------------------------------------------------------------------
# The registry
# ---------------------------------------------------------------------------------------------------

$fields = @('name', 'edits', 'runner', 'package', 'target', 'test', 'match', 'build', 'notHere')

# What is wrong with a list of entries, as sentences. A missing field would make an entry that matches
# nothing and reads as STALE, which looks like moved code rather than a typo, and two entries under one
# name would make one of them invisible to the comparison with a base.
function Get-RegistryProblems($Entries) {
    $problems = [Collections.Generic.List[string]]::new()
    $seen = @{}
    foreach ($entry in @($Entries)) {
        if ($entry -isnot [pscustomobject]) { $problems.Add('an entry is not an object'); continue }
        $missing = @($fields | Where-Object { $null -eq $entry.PSObject.Properties[$_] })
        if ($missing.Count) { $problems.Add("an entry has no $($missing -join ', '): $($entry.name)"); continue }

        $name = [string] $entry.name
        if ($name.Length -eq 0 -or $name.Contains('|') -or $name.Trim() -ne $name) { $problems.Add("an entry has a name the runner cannot select: '$name'") }
        if ($seen.ContainsKey($name)) { $problems.Add("two entries are named '$name'") } else { $seen[$name] = $true }

        $edits = @($entry.edits)
        if ($edits.Count -eq 0) { $problems.Add("an entry has no edit: $name") }
        foreach ($edit in $edits) {
            if ([string]::IsNullOrEmpty([string] $edit.file) -or [string]::IsNullOrEmpty([string] $edit.from) -or $edit.to -isnot [string]) {
                $problems.Add("an edit needs a file, a from and a to: $name")
            }
        }

        $runner = [string] $entry.runner
        $match = [string] $entry.match
        $test = [string] $entry.test
        switch ($runner) {
            'rust' {
                if (-not $entry.package) { $problems.Add("a Rust entry has no package: $name") }
                if ($test -and $match -notin 'exact', 'substring') { $problems.Add("a Rust entry matches its test neither exact nor substring: $name") }
                if (-not $test -and $match) { $problems.Add("a Rust entry with no test has a match: $name") }
            }
            'clippy' {
                if (-not $entry.package) { $problems.Add("a clippy entry has no package: $name") }
            }
            'cs' {
                if (-not $entry.package -or -not $test) { $problems.Add("a C# entry needs a project and a test: $name") }
                if ($match -notin 'method', 'class') { $problems.Add("a C# entry filters neither by method nor by class: $name") }
            }
            'none' {
                if (-not $entry.notHere) { $problems.Add("an entry no runner judges does not say why: $name") }
            }
            default { $problems.Add("an entry has the runner '$runner', which is not rust, clippy, cs or none: $name") }
        }
        if ([string] $entry.build -notin '', 'debug', 'release') { $problems.Add("an entry asks for the build '$($entry.build)': $name") }
    }
    return [string[]] $problems
}

function Read-Registry([string] $Text) {
    return @(ConvertFrom-JsonText $Text)
}

$registryPath = Join-Path $PSScriptRoot 'entries.json'
$mutations = Read-Registry ([IO.File]::ReadAllText($registryPath))
$registryProblems = @(Get-RegistryProblems $mutations)
if ($registryProblems.Count) {
    $registryProblems | ForEach-Object { Write-Output "entries.json: $_" }
    exit 1
}

# The canaries. Mutations that cannot compile, which must come back BROKEN rather than caught. If one
# comes back anything else, the runner is counting build failures as evidence and every other line of
# its output is worthless. Their anchors are lines that change rarely - when one moves, the canary says
# STALE and the part is not believed until the anchor is put right here.
$canaries = @(
    [pscustomobject]@{
        name = 'CANARY rust - deliberately does not compile'
        edits = @([pscustomobject]@{ file = 'crates/proto/src/lib.rs'; from = 'mod tests {'; to = 'mod tests { this line cannot compile' })
        runner = 'rust'; package = 'chrono-proto'; target = @('--lib'); test = 'tests::'; match = 'substring'; build = ''; notHere = ''
    }
    [pscustomobject]@{
        name = 'CANARY cs - deliberately does not compile'
        edits = @([pscustomobject]@{ file = 'gui/ChronoMock.Protocol/ChildProcesses.cs'; from = 'namespace ChronoMock.Protocol;'; to = 'namespace ChronoMock.Protocol; this line cannot compile' })
        runner = 'cs'; package = 'gui/ChronoMock.Protocol.Tests'; target = @(); test = '*ChildProcessesTests'; match = 'class'; build = ''; notHere = ''
    }
)

# ---------------------------------------------------------------------------------------------------
# Running a tool
# ---------------------------------------------------------------------------------------------------

# A command run to its end or to its time limit, both channels kept in the order they were written.
#
# Not the call operator, for three reasons. A time limit, so a mutation that makes a test hang costs one
# entry and not the whole part - on a timeout the whole process tree goes, the test binary under cargo
# included. An argument list passed as a list, so a test name is never split or quoted by a shell. And
# a BOUNDED wait for the end of the output: any process that inherited the pipe and outlives the
# command keeps it open after the command itself has ended, and a reader waiting for the pipe to close
# would wait as long as that process lives. The case in view is a target program that a mutated test
# started and failed to stop, because a program under test inherits the core's standard output.
# Whether a build server ever does the same was not measured. Here the wait for the last line is thirty
# seconds after the exit, and what came before is kept.
#
# In C# because the output arrives on threads that have no PowerShell runspace, where a script block
# handling it would take the whole process down.
if (-not ('ChronoMockMutation.NativeRun' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Text;
using System.Threading;

namespace ChronoMockMutation
{
    public static class NativeRun
    {
        public static object[] Run(string file, string[] arguments, string folder, int milliseconds)
        {
            var info = new ProcessStartInfo(file);
            info.WorkingDirectory = folder;
            info.UseShellExecute = false;
            info.RedirectStandardOutput = true;
            info.RedirectStandardError = true;
            foreach (var argument in arguments) { info.ArgumentList.Add(argument); }

            var text = new StringBuilder();
            var gate = new object();
            var outputEnded = new ManualResetEventSlim();
            var errorEnded = new ManualResetEventSlim();
            var process = new Process();
            process.StartInfo = info;
            process.OutputDataReceived += (sender, e) => { if (e.Data == null) { outputEnded.Set(); } else { lock (gate) { text.AppendLine(e.Data); } } };
            process.ErrorDataReceived += (sender, e) => { if (e.Data == null) { errorEnded.Set(); } else { lock (gate) { text.AppendLine(e.Data); } } };
            process.Start();
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();

            var ended = process.WaitForExit(milliseconds);
            if (!ended)
            {
                try { process.Kill(true); } catch (InvalidOperationException) { }
                process.WaitForExit(30000);
            }
            var drained = outputEnded.Wait(30000) & errorEnded.Wait(30000);
            var code = ended ? process.ExitCode : -1;
            if (drained) { process.Dispose(); }
            lock (gate) { return new object[] { code, text.ToString(), !ended, drained }; }
        }
    }
}
'@
}

# What a tool said, without the escape sequences of a coloured terminal. Measured on the first run on
# GitHub (2026-10-16): there the test platform colours its output even into a pipe, so a summary line
# reads ESC[31m  failed: 1 and every pattern anchored at the start of a line missed it. 197 C# entries
# came out ERROR although each had a named failure and the exit code of one, and the baseline could not
# have named a red C# test at all. Locally the same output had no colour, so no local run showed it.
function ConvertTo-PlainText([string] $Text) {
    $Text = [regex]::Replace($Text, '\x1B\][^\x07\x1B]*(?:\x07|\x1B\\)', '')
    return [regex]::Replace($Text, '\x1B\[[0-?]*[ -/]*[@-~]', '')
}

function Invoke-Native([string] $Command, [string[]] $Arguments, [int] $Minutes) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $limit = [int] [Math]::Min([int]::MaxValue, [long] $Minutes * 60000)
    $said = [ChronoMockMutation.NativeRun]::Run($Command, [string[]] @($Arguments), $Root, $limit)
    $output = ConvertTo-PlainText ([string] $said[1])
    if (-not $said[3]) { $output += "`n(the output did not end within 30 s of the exit - a process the command left behind holds it open)" }
    return [pscustomobject]@{
        Code = [int] $said[0]
        Output = $output
        TimedOut = [bool] $said[2]
        Seconds = [Math]::Round($clock.Elapsed.TotalSeconds, 1)
    }
}

# The last lines of what a command said, for the log of a verdict that is not caught - on a machine
# nobody can log in to, the log is all there is.
function Get-Tail([string] $Output, [int] $Lines = 60) {
    $all = @([regex]::Split($Output.TrimEnd(), '\r?\n'))
    if ($all.Count -le $Lines) { return [string[]] $all }
    return [string[]] (@("... $($all.Count - $Lines) earlier lines left out") + $all[($all.Count - $Lines)..($all.Count - 1)])
}

# ---------------------------------------------------------------------------------------------------
# What a test run said
# ---------------------------------------------------------------------------------------------------

# libtest's own summary, one line per test binary, added up.
function Get-RustSummary([string] $Output) {
    $passed = 0
    $failed = 0
    foreach ($line in [regex]::Matches($Output, 'test result: \w+\. (\d+) passed; (\d+) failed;')) {
        $passed += [int] $line.Groups[1].Value
        $failed += [int] $line.Groups[2].Value
    }
    return [pscustomobject]@{ Passed = $passed; Failed = $failed }
}

# The tests libtest names as failed, from the heading of each failure's captured output.
function Get-RustRed([string] $Output) {
    return [string[]] @([regex]::Matches($Output, '(?m)^---- (.+?) stdout ----\s*$') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
}

# A non-zero exit with no failed test in the summary is caught only when cargo says a test binary died,
# which is what a mutation causing a crash looks like. Anything else is a run that could not say.
function Get-RustVerdict($Run) {
    if ($Run.TimedOut) { return 'TIMEOUT' }
    $summary = Get-RustSummary $Run.Output
    if ($summary.Failed -gt 0) { return 'caught' }
    if ($Run.Code -ne 0) {
        if ($Run.Output -match "process didn't exit successfully") { return 'caught' }
        return 'ERROR'
    }
    if ($summary.Passed -eq 0) { return 'NO TEST' }
    return 'MISSED'
}

# The summary of Microsoft.Testing.Platform in English: total and failed, added up over the test
# applications of the run. Found = false when no summary was printed at all.
function Get-CsSummary([string] $Output) {
    $total = [regex]::Matches($Output, '(?m)^\s*total:\s*(\d+)\s*$')
    $failed = [regex]::Matches($Output, '(?m)^\s*failed:\s*(\d+)\s*$')
    return [pscustomobject]@{
        Found = $total.Count -gt 0
        Total = ($total | ForEach-Object { [int] $_.Groups[1].Value } | Measure-Object -Sum).Sum
        Failed = ($failed | ForEach-Object { [int] $_.Groups[1].Value } | Measure-Object -Sum).Sum
    }
}

# The tests the platform names as failed: a line "failed <name> (<duration>)". A theory's name carries
# its arguments, which are cut off here, because a filter names the method and not one case of it.
function Get-CsRed([string] $Output) {
    return [string[]] @([regex]::Matches($Output, '(?m)^\s*failed (\S.*?) \(\d[^)]*\)\s*$') | ForEach-Object { $_.Groups[1].Value.Split('(')[0].Trim() } | Sort-Object -Unique)
}

# Exit code 8 is the platform's own "the run executed zero tests" (measured 2026-10-16 with a filter
# that matched nothing), and 2 is "at least one test failed".
function Get-CsVerdict($Run) {
    if ($Run.TimedOut) { return 'TIMEOUT' }
    $summary = Get-CsSummary $Run.Output
    if ($summary.Failed -gt 0) { return 'caught' }
    if ($Run.Code -eq 8 -or ($summary.Found -and $summary.Total -eq 0)) { return 'NO TEST' }
    if ($Run.Code -ne 0 -or -not $summary.Found) { return 'ERROR' }
    return 'MISSED'
}

# A test filter with * as its only wildcard, the way --filter-method and --filter-class read one.
function Test-Wildcard([string] $Pattern, [string] $Text) {
    $regex = '^' + ([regex]::Escape($Pattern) -replace '\\\*', '.*') + '$'
    return $Text -cmatch $regex
}

# Whether a test named red on the clean tree is one an entry's filter runs.
function Test-NamesRed($Entry, [string] $Red) {
    $test = [string] $Entry.test
    switch ($Entry.runner) {
        'rust' {
            if (-not $test) { return $true }
            if ($Entry.match -eq 'exact') { return $Red -ceq $test }
            return $Red.Contains($test)
        }
        'cs' {
            if ($Entry.match -eq 'class') {
                $at = $Red.LastIndexOf('.')
                $class = if ($at -gt 0) { $Red.Substring(0, $at) } else { $Red }
                return Test-Wildcard $test $class
            }
            return Test-Wildcard $test $Red
        }
    }
    return $false
}

# ---------------------------------------------------------------------------------------------------
# The commands, one place for each
# ---------------------------------------------------------------------------------------------------

function Get-CrateFolder([string] $Package) {
    if ($Package.StartsWith('chrono-')) { return 'crates/' + $Package.Substring('chrono-'.Length) }
    return ''
}

function Get-RustTestArguments([string] $Package, [string[]] $Target, [string] $Match, [string[]] $Names, [switch] $NoFailFast) {
    $arguments = @('test', '-q', '-p', $Package) + @($Target)
    if ($NoFailFast) { $arguments += '--no-fail-fast' }
    if ($Names.Count) {
        $arguments += '--'
        if ($Match -eq 'exact') { $arguments += '--exact' }
        $arguments += $Names
    }
    return [string[]] $arguments
}

function Get-CsTestArguments([string] $Project, [string] $Match, [string[]] $Names) {
    $arguments = @('test', '--project', $Project, '--no-build')
    $switch = if ($Match -eq 'class') { '--filter-class' } else { '--filter-method' }
    foreach ($name in $Names) { $arguments += $switch; $arguments += $name }
    if ($CiScope) { $arguments += @('--filter-not-trait', 'Category=Integration') }
    return [string[]] $arguments
}

function Invoke-CsBuild([string] $Project) {
    # Warnings are not errors for a mutated tree. The question is whether the named test goes red, and a
    # mutation removes the only use of something as often as not - the sister project measured 38 of 602
    # entries BROKEN for that alone. The product's own build stays strict everywhere else.
    return Invoke-Native 'dotnet' @('build', $Project, '-c', 'Debug', '--nologo', '-v', 'q', '-p:TreatWarningsAsErrors=false') $BuildMinutes
}

# The artifacts an entry's test reads besides the test binary: the debug workspace (chrono_hook.dll,
# which cargo test never builds) or both release cores (BinaryImportsTests reads them).
function Invoke-ArtifactBuild([string] $Kind) {
    $runs = @()
    if ($Kind -eq 'debug') {
        $runs += Invoke-Native 'cargo' @('build', '-q', '--workspace') $BuildMinutes
    } elseif ($Kind -eq 'release') {
        $runs += Invoke-Native 'cargo' @('build', '-q', '--release', '--target', 'x86_64-pc-windows-msvc') $BuildMinutes
        if ($runs[-1].Code -eq 0) { $runs += Invoke-Native 'cargo' @('build', '-q', '--release', '--target', 'i686-pc-windows-msvc') $BuildMinutes }
    }
    $failed = @($runs | Where-Object { $_.Code -ne 0 })
    return [pscustomobject]@{ Ok = $failed.Count -eq 0; Run = if ($failed.Count) { $failed[0] } else { $null } }
}

# ---------------------------------------------------------------------------------------------------
# Mutating, and putting back
# ---------------------------------------------------------------------------------------------------

# A file as an entry sees it - text with LF line ends - and what it takes to write it back in its own
# shape: the bytes as found, whether it began with a byte order mark and whether it used CRLF. Line ends
# are taken out of the comparison because they are not what any entry is about.
function Read-Source([string] $Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $bom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    $skip = if ($bom) { 3 } else { 0 }
    $text = [Text.UTF8Encoding]::new($false).GetString($bytes, $skip, $bytes.Length - $skip)
    return [pscustomobject]@{
        Path = $Path; Bytes = $bytes; Bom = $bom; Crlf = $text.Contains("`r`n")
        Text = $text.Replace("`r`n", "`n"); Mutated = $text.Replace("`r`n", "`n")
    }
}

# The comma hands the bytes back as one array. Without it the pipeline unrolls them into a list of
# boxed numbers, one object per byte of the file.
function ConvertTo-SourceBytes($Source, [string] $Text) {
    $shaped = if ($Source.Crlf) { $Text.Replace("`n", "`r`n") } else { $Text }
    $body = [Text.UTF8Encoding]::new($false).GetBytes($shaped)
    if ($Source.Bom) { return , [byte[]] (@([byte] 0xEF, [byte] 0xBB, [byte] 0xBF) + $body) }
    return , $body
}

function Get-Occurrences([string] $Text, [string] $Part) {
    $count = 0
    $at = $Text.IndexOf($Part, [StringComparison]::Ordinal)
    while ($at -ge 0) {
        $count++
        $at = $Text.IndexOf($Part, $at + 1, [StringComparison]::Ordinal)
    }
    return $count
}

# Every edit of an entry applied in memory, in order, to the files it names. An anchor has to be in its
# file EXACTLY once at the moment it is applied: a second occurrence would make the entry break code
# nobody chose, and none means the code moved on. Either way the entry is STALE and nothing is written.
function Get-MutatedSources($Entry) {
    $sources = [ordered]@{}
    foreach ($edit in @($Entry.edits)) {
        $path = Join-Path $Root $edit.file
        if (-not $sources.Contains($path)) {
            if (-not [IO.File]::Exists($path)) { return [pscustomobject]@{ Stale = "$($edit.file) does not exist"; Sources = @() } }
            $sources[$path] = Read-Source $path
        }
        $source = $sources[$path]
        $from = ([string] $edit.from).Replace("`r`n", "`n")
        $count = Get-Occurrences $source.Mutated $from
        if ($count -ne 1) {
            return [pscustomobject]@{ Stale = "an anchor is in $($edit.file) $count times, not once"; Sources = @() }
        }
        $at = $source.Mutated.IndexOf($from, [StringComparison]::Ordinal)
        $source.Mutated = $source.Mutated.Substring(0, $at) + ([string] $edit.to).Replace("`r`n", "`n") + $source.Mutated.Substring($at + $from.Length)
    }
    return [pscustomobject]@{ Stale = ''; Sources = @($sources.Values) }
}

# The recovery file: one per working tree, in the temporary folder, written and read through the same
# API (on the maintainer's machine TMP and TEMP point at different folders, and a recovery written to
# one and looked for in the other would never be found).
function Get-RecoveryPath([string] $ForRoot) {
    $hash = [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($ForRoot.TrimEnd('\', '/').ToLowerInvariant()))
    return Join-Path ([IO.Path]::GetTempPath()) ('chronomock-mutation-{0}.json' -f [Convert]::ToHexString($hash).Substring(0, 16))
}

function Get-Sha([byte[]] $Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes))
}

function Write-Recovery([string] $Path, $Sources) {
    $files = @($Sources | ForEach-Object {
        [ordered]@{ path = $_.Path; original = [Convert]::ToBase64String($_.Bytes); mutated = (Get-Sha (ConvertTo-SourceBytes $_ $_.Mutated)) }
    })
    [IO.File]::WriteAllText($Path, (ConvertTo-Json @{ files = $files } -Depth 4), [Text.UTF8Encoding]::new($false))
}

# What a killed run left behind, put back. A file that still holds the mutation gets its original, a
# file already holding its original is left alone, and a file that is neither was changed by somebody
# since - it is named and nothing is overwritten. Returns the sentences to say and whether it is safe to
# go on.
function Restore-FromRecovery([string] $Path) {
    $said = [Collections.Generic.List[string]]::new()
    if (-not [IO.File]::Exists($Path)) { return [pscustomobject]@{ Safe = $true; Said = [string[]] @() } }

    $safe = $true
    $kept = ConvertFrom-JsonText ([IO.File]::ReadAllText($Path))
    foreach ($file in @($kept.files)) {
        $original = [Convert]::FromBase64String($file.original)
        $now = if ([IO.File]::Exists($file.path)) { [IO.File]::ReadAllBytes($file.path) } else { $null }
        if ($null -ne $now -and (Get-Sha $now) -eq $file.mutated) {
            [IO.File]::WriteAllBytes($file.path, $original)
            $said.Add("put back the original of $($file.path), which a stopped run left mutated")
        } elseif ($null -ne $now -and (Get-Sha $now) -eq (Get-Sha $original)) {
            $said.Add("$($file.path) already holds its original")
        } else {
            $safe = $false
            $said.Add("$($file.path) holds neither the original nor the mutation a stopped run wrote - compare it with git and put it right by hand")
        }
    }
    if ($safe) { [IO.File]::Delete($Path) }
    return [pscustomobject]@{ Safe = $safe; Said = [string[]] $said }
}

# ---------------------------------------------------------------------------------------------------
# One entry
# ---------------------------------------------------------------------------------------------------

function New-Verdict([string] $Verdict, [string[]] $Said = @()) {
    return [pscustomobject]@{ Verdict = $Verdict; Said = [string[]] $Said }
}

# The mutated tree judged by the entry's own command, compiled first so that BROKEN is its own answer.
function Invoke-Judgement($Entry) {
    if ($Entry.build) {
        $artifacts = Invoke-ArtifactBuild $Entry.build
        if (-not $artifacts.Ok) { return New-Verdict 'BROKEN' (Get-Tail $artifacts.Run.Output) }
    }

    switch ($Entry.runner) {
        'rust' {
            $target = [string[]] @($Entry.target)
            $compile = Invoke-Native 'cargo' (@('test', '--no-run', '-q', '-p', $Entry.package) + $target) $BuildMinutes
            if ($compile.Code -ne 0) { return New-Verdict 'BROKEN' (Get-Tail $compile.Output) }
            $names = if ($Entry.test) { [string[]] @($Entry.test) } else { [string[]] @() }
            $run = Invoke-Native 'cargo' (Get-RustTestArguments $Entry.package $target $Entry.match $names) $TestMinutes
            $verdict = Get-RustVerdict $run
            return New-Verdict $verdict $(if ($verdict -ne 'caught') { Get-Tail $run.Output } else { @() })
        }
        'clippy' {
            $compile = Invoke-Native 'cargo' @('check', '-q', '-p', $Entry.package) $BuildMinutes
            if ($compile.Code -ne 0) { return New-Verdict 'BROKEN' (Get-Tail $compile.Output) }
            $run = Invoke-Native 'cargo' @('clippy', '-q', '-p', $Entry.package, '--', '-D', 'warnings') $TestMinutes
            if ($run.TimedOut) { return New-Verdict 'TIMEOUT' }
            if ($run.Code -ne 0) { return New-Verdict 'caught' }
            return New-Verdict 'MISSED'
        }
        'cs' {
            $compile = Invoke-CsBuild $Entry.package
            if ($compile.Code -ne 0) { return New-Verdict 'BROKEN' (Get-Tail $compile.Output) }
            $run = Invoke-Native 'dotnet' (Get-CsTestArguments $Entry.package $Entry.match @([string] $Entry.test)) $TestMinutes
            $verdict = Get-CsVerdict $run
            return New-Verdict $verdict $(if ($verdict -ne 'caught') { Get-Tail $run.Output } else { @() })
        }
    }
    return New-Verdict 'ERROR' @("no runner judges '$($Entry.runner)'")
}

function Test-Mutation($Entry) {
    $planned = Get-MutatedSources $Entry
    if ($planned.Stale) { return New-Verdict 'STALE' @($planned.Stale) }

    Write-Recovery $recoveryPath $planned.Sources
    try {
        foreach ($source in $planned.Sources) { [IO.File]::WriteAllBytes($source.Path, (ConvertTo-SourceBytes $source $source.Mutated)) }
        return Invoke-Judgement $Entry
    }
    finally {
        foreach ($source in $planned.Sources) { [IO.File]::WriteAllBytes($source.Path, $source.Bytes) }
        if ([IO.File]::Exists($recoveryPath)) { [IO.File]::Delete($recoveryPath) }
    }
}

# ---------------------------------------------------------------------------------------------------
# What a change reaches, which part a machine judges, and what CI runs
# ---------------------------------------------------------------------------------------------------

function Get-RelativePath([string] $Full) {
    return $Full.Substring($Root.Length).TrimStart('\', '/').Replace('\', '/')
}

# Where tests are declared: C# test classes and methods in the test projects, Rust functions in the
# crates. A list of files per name, because a name can be declared twice. A declaration is a line that
# starts with modifiers and the keyword, so a comment saying "the class that" registers nothing.
function Get-SourceIndex([string] $ForRoot) {
    $classes = @{}
    $methods = @{}
    $functions = @{}
    $classDeclaration = '(?m)^\s*(?:(?:public|internal|private|protected|sealed|static|partial|abstract|file)\s+)*(?:class|record)\s+([A-Za-z_]\w*)'
    $methodDeclaration = '(?m)^\s*(?:(?:public|internal|private|protected|static|async)\s+)+(?:void|Task|ValueTask)\s+([A-Za-z_]\w*)\s*\('
    $functionDeclaration = '(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*fn\s+([A-Za-z_]\w*)'

    function Add-Place($Index, [string] $Name, [string] $File) {
        if (-not $Index.ContainsKey($Name)) { $Index[$Name] = [Collections.Generic.List[string]]::new() }
        if (-not $Index[$Name].Contains($File)) { $Index[$Name].Add($File) }
    }

    foreach ($file in Get-ChildItem (Join-Path $ForRoot 'gui') -Recurse -File -Filter '*.cs') {
        $relative = Get-RelativePath $file.FullName
        if ($relative -notmatch '^gui/[^/]*Tests/' -or $relative -match '/(bin|obj)/') { continue }
        $text = [IO.File]::ReadAllText($file.FullName)
        foreach ($found in [regex]::Matches($text, $classDeclaration)) { Add-Place $classes $found.Groups[1].Value $relative }
        foreach ($found in [regex]::Matches($text, $methodDeclaration)) { Add-Place $methods $found.Groups[1].Value $relative }
    }
    foreach ($file in Get-ChildItem (Join-Path $ForRoot 'crates') -Recurse -File -Filter '*.rs') {
        $relative = Get-RelativePath $file.FullName
        if ($relative -match '/target/') { continue }
        foreach ($found in [regex]::Matches([IO.File]::ReadAllText($file.FullName), $functionDeclaration)) { Add-Place $functions $found.Groups[1].Value $relative }
    }
    return [pscustomobject]@{ Classes = $classes; Methods = $methods; Functions = $functions }
}

# The class and method a C# filter names. A filter is Class.Method, sometimes with the namespace in
# front, sometimes *.Method or *Class.Method, and a class filter names only the class. The second
# segment from the right is asked first, then every other one from the right, and the first the index
# knows is the class. A filter naming only a method finds its files through the method index.
function Find-TestClass([string] $Test, $Index) {
    $segments = @($Test.Split('(')[0].Split('.') | ForEach-Object { $_.Trim('*') })
    $order = @()
    if ($segments.Count -ge 2) { $order += $segments.Count - 2 }
    $order += @(($segments.Count - 1)..0 | Where-Object { $_ -notin $order })

    foreach ($at in $order) {
        if ($segments[$at] -and $Index.Classes.ContainsKey($segments[$at])) {
            $method = if ($at -lt $segments.Count - 1) { $segments[$segments.Count - 1] } else { '' }
            return [pscustomobject]@{ Class = $segments[$at]; Method = $method; Places = [string[]] @($Index.Classes[$segments[$at]]) }
        }
    }
    $last = $segments[$segments.Count - 1]
    if ($last -and $Index.Methods.ContainsKey($last)) {
        return [pscustomobject]@{ Class = ''; Method = $last; Places = [string[]] @($Index.Methods[$last]) }
    }
    return [pscustomobject]@{ Class = ''; Method = ''; Places = [string[]] @() }
}

# The files whose change can change what an entry's test says. A path ending in / stands for every
# file under it.
function Get-GuardFiles($Entry, $Index) {
    switch ($Entry.runner) {
        'cs' { return [string[]] (Find-TestClass ([string] $Entry.test) $Index).Places }
        'clippy' {
            $crate = Get-CrateFolder $Entry.package
            return [string[]] @('Cargo.toml', 'clippy.toml', "$crate/Cargo.toml")
        }
        'rust' {
            $crate = Get-CrateFolder $Entry.package
            $target = @($Entry.target)
            if ($target.Count -eq 2 -and $target[0] -eq '--test') { return [string[]] @("$crate/tests/$($target[1]).rs") }
            $test = [string] $Entry.test
            if (-not $test) { return [string[]] @("$crate/") }
            if ($test.EndsWith('::')) {
                $module = $test.TrimEnd(':').Replace('::', '/')
                return [string[]] @("$crate/src/$module.rs", "$crate/src/$module/")
            }
            $name = @($test.Split('::') | Where-Object { $_ })[-1]
            if ($Index.Functions.ContainsKey($name)) { return [string[]] @($Index.Functions[$name] | Where-Object { $_.StartsWith("$crate/") }) }
        }
    }
    return [string[]] @()
}

function Test-Reaches([string[]] $Guards, $ChangedSet) {
    foreach ($guard in $Guards) {
        if ($guard.EndsWith('/')) {
            foreach ($changed in $ChangedSet.Keys) { if ($changed.StartsWith($guard)) { return $true } }
        } elseif ($ChangedSet.ContainsKey($guard)) {
            return $true
        }
    }
    return $false
}

# Everything an entry does, as one text, so two entries compare equal exactly when they break the same
# thing and judge it the same way. The name is left out - it is what the two are found by.
function Get-EntryKey($Entry) {
    return ($Entry | Select-Object -Property ($fields | Where-Object { $_ -ne 'name' }) | ConvertTo-Json -Compress -Depth 6)
}

# THE ENTRIES A CHANGE REACHES, four ways, and the reason travels with each one:
#
#   its file     the change edits a file the entry mutates
#   its guard    the change edits a file the entry's test is declared in
#   a new entry  the base has no entry under this name
#   a changed entry  the entry breaks or judges something else than under its name in the base
#
# WHAT NONE OF THEM CAN SEE is a change in a THIRD file that stops a guard reaching its code - a helper
# both share, a style the test reads. That is what the weekly run of every entry is for, and it is why
# the pull request run reports rather than gates.
function Select-Reached($Entries, $Previous, [string[]] $ChangedFiles, $Index) {
    $changedSet = @{}
    foreach ($one in $ChangedFiles) { if ($one) { $changedSet[$one.Trim().Replace('\', '/')] = $true } }
    $before = @{}
    foreach ($one in @($Previous)) { $before[$one.name] = $one }

    foreach ($entry in @($Entries)) {
        $why = ''
        if (@($entry.edits | Where-Object { $changedSet.ContainsKey(([string] $_.file).Replace('\', '/')) }).Count -gt 0) {
            $why = 'its file'
        } elseif (Test-Reaches (Get-GuardFiles $entry $Index) $changedSet) {
            $why = 'its guard'
        } elseif (-not $before.ContainsKey($entry.name)) {
            $why = 'a new entry'
        } elseif ((Get-EntryKey $entry) -cne (Get-EntryKey $before[$entry.name])) {
            $why = 'a changed entry'
        }
        if ($why) { [pscustomobject]@{ Entry = $entry; Why = $why } }
    }
}

# Whether the Integration trait reaches this class or this one method, read off the text. The trait
# stands on whole classes in some files and on single methods in others. What sits between the trait
# and the declaration may hold no brace and no semicolon, which keeps a trait on the previous method
# from reaching this one: that method has a body.
function Test-TraitReaches([string] $Text, [string] $Class, [string] $Method) {
    $trait = 'Trait\(\s*"Category"\s*,\s*"Integration"\s*\)'
    if ($Class -and $Text -match ('(?s)' + $trait + '[^{;]*?\b(?:class|record)\s+' + [regex]::Escape($Class) + '\b')) { return $true }
    if ($Method -and $Text -match ('(?s)' + $trait + '[^{;]*?\b' + [regex]::Escape($Method) + '\s*\(')) { return $true }
    return $false
}

# Why an entry is not judged here, or nothing when it is. Under -CiScope a C# test is NOT HERE when
# every file declaring it carries the trait on its class or its method, and a class filter whose class
# has the trait on only some methods is judged, with those methods left out of the run.
function Get-NotHere($Entry, $Index) {
    if ($Entry.notHere) { return [string] $Entry.notHere }
    if ($CiScope -and $Entry.runner -eq 'cs') {
        $found = Find-TestClass ([string] $Entry.test) $Index
        if ($found.Places.Count -eq 0) { return '' }
        foreach ($place in $found.Places) {
            if (-not (Test-TraitReaches ([IO.File]::ReadAllText((Join-Path $Root $place))) $found.Class $found.Method)) { return '' }
        }
        return 'its test carries the Integration trait, which CI filters out'
    }
    return ''
}

# How many parts: a run of every entry gets the full count, a pull request one part per twenty entries
# and never more than its ceiling, and never more parts than entries. None for none - an empty matrix is
# an error on GitHub, so the workflow asks this number before it starts one.
function Get-PartCount([int] $Count, [bool] $PullRequest) {
    if ($Count -le 0) { return 0 }
    $parts = if ($PullRequest) { [Math]::Min($MostPartsOnAPullRequest, [int] [Math]::Ceiling($Count / $EntriesPerPart)) } else { $FullParts }
    return [Math]::Max(1, [Math]::Min($parts, $Count))
}

# The matrix GitHub expands, as JSON. Spelled out for zero because 0..-1 is the two numbers 0 and -1 in
# PowerShell, not an empty range.
function Get-ShardList([int] $Parts) {
    if ($Parts -le 0) { return '[]' }
    return '[' + ((0..($Parts - 1)) -join ',') + ']'
}

function Select-Part($Entries, [int] $Part, [int] $Of) {
    $list = @($Entries)
    for ($n = 0; $n -lt $list.Count; $n++) {
        if ($n % $Of -eq $Part) { $list[$n] }
    }
}

# ---------------------------------------------------------------------------------------------------
# The baseline: every named test once on the UNMUTATED tree
# ---------------------------------------------------------------------------------------------------

# A test red before any mutation would make every mutant look caught. So after the clean build each
# group of entries runs its named tests once, forty names to a command, and an entry whose test is red
# here is BASELINE RED and never mutated. A command that failed without naming a red test cannot say
# which of its tests is red, so every entry in it is taken as red - a guess in the safe direction.
# Whether a baseline command failed without saying which test is red. Any exit but success is that,
# once no red test could be read - for C# the code of a failed test (2) included, because a failure line
# this runner cannot read would otherwise leave a red test green here and turn every mutant of it into a
# false caught. Only the platform's "zero tests ran" (8) is not a failure of the tree.
function Test-BaselineUnclear([string] $Runner, [int] $Code, [int] $RedCount) {
    if ($RedCount -gt 0 -or $Code -eq 0) { return $false }
    if ($Runner -eq 'cs' -and $Code -eq 8) { return $false }
    return $true
}

function Invoke-Baseline($Entries) {
    $red = @{}
    $said = [Collections.Generic.List[string]]::new()
    $ran = 0

    foreach ($group in ($Entries | Group-Object { '{0}|{1}|{2}|{3}' -f $_.runner, $_.package, (@($_.target) -join ' '), $_.match })) {
        $members = @($group.Group)
        $first = $members[0]

        if ($first.runner -eq 'clippy') {
            $run = Invoke-Native 'cargo' @('clippy', '-q', '-p', $first.package, '--', '-D', 'warnings') $TestMinutes
            $ran++
            if ($run.Code -ne 0) {
                foreach ($entry in $members) { $red[$entry.name] = $true }
                $said.Add("clippy is red on the clean tree of $($first.package)")
                foreach ($line in (Get-Tail $run.Output 40)) { $said.Add($line) }
            }
            continue
        }

        $names = [string[]] @($members | ForEach-Object { [string] $_.test } | Where-Object { $_ } | Sort-Object -Unique)
        $batches = [Collections.Generic.List[object]]::new()
        if ($names.Count -eq 0) { $batches.Add([string[]] @()) }
        for ($at = 0; $at -lt $names.Count; $at += 40) { $batches.Add([string[]] $names[$at..([Math]::Min($at + 39, $names.Count - 1))]) }

        foreach ($batch in $batches) {
            $ran += [Math]::Max(1, $batch.Count)
            if ($first.runner -eq 'rust') {
                $run = Invoke-Native 'cargo' (Get-RustTestArguments $first.package ([string[]] @($first.target)) $first.match $batch -NoFailFast) $TestMinutes
                $redNames = @(Get-RustRed $run.Output)
            } else {
                $run = Invoke-Native 'dotnet' (Get-CsTestArguments $first.package $first.match $batch) $TestMinutes
                $redNames = @(Get-CsRed $run.Output)
            }
            $unclear = Test-BaselineUnclear $first.runner $run.Code $redNames.Count

            $inBatch = @($members | Where-Object { ($batch.Count -eq 0 -and -not $_.test) -or $batch -ccontains [string] $_.test })
            foreach ($entry in $inBatch) {
                if ($unclear -or @($redNames | Where-Object { Test-NamesRed $entry $_ }).Count -gt 0) { $red[$entry.name] = $true }
            }
            if ($unclear) { $said.Add("a baseline run of $($first.package) failed without naming a red test, so all $($inBatch.Count) of its entries are taken as red") }
            foreach ($name in $redNames) { $said.Add("red already: $name") }
            if ($unclear -or $redNames.Count) { foreach ($line in (Get-Tail $run.Output 120)) { $said.Add("| $line") } }
        }
    }
    return [pscustomobject]@{ Red = $red; Said = [string[]] $said; Ran = $ran }
}

# ---------------------------------------------------------------------------------------------------
# The self test
# ---------------------------------------------------------------------------------------------------

if ($SelfTest) {
    $problems = [Collections.Generic.List[string]]::new()

    # libtest output with one failed test, as cargo prints it with -q, and the same binary green.
    $rustRed = @(
        'running 2 tests'
        'test tests::a_kept_test ... ok'
        'test tests::a_test_that_fails ... FAILED'
        ''
        'failures:'
        ''
        '---- tests::a_test_that_fails stdout ----'
        ''
        "thread 'tests::a_test_that_fails' panicked at crates/core/src/lib.rs:10:5:"
        'assertion failed: false'
        ''
        'failures:'
        '    tests::a_test_that_fails'
        ''
        'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s'
        ''
        'error: test failed, to rerun pass `-p chrono-core --lib`'
    ) -join "`n"
    $rustGreen = "running 1 test`ntest tests::a_kept_test ... ok`n`ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s"
    $rustNone = "running 0 tests`n`ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s"
    $rustCrash = "running 1 test`nerror: test failed, to rerun pass `--lib``n`nCaused by:`n  process didn't exit successfully: ``target\debug\deps\x.exe`` (exit code: 0xc00000fd, STATUS_STACK_OVERFLOW)"

    if ((@(Get-RustRed $rustRed) -join ',') -ne 'tests::a_test_that_fails') { $problems.Add("the red Rust test read was '$(@(Get-RustRed $rustRed) -join ',')'") }
    $cases = @(
        @{ Run = @{ Code = 101; Output = $rustRed; TimedOut = $false }; Expect = 'caught' }
        @{ Run = @{ Code = 0; Output = $rustGreen; TimedOut = $false }; Expect = 'MISSED' }
        @{ Run = @{ Code = 0; Output = $rustNone; TimedOut = $false }; Expect = 'NO TEST' }
        @{ Run = @{ Code = 101; Output = $rustCrash; TimedOut = $false }; Expect = 'caught' }
        @{ Run = @{ Code = 101; Output = 'error: could not find the package'; TimedOut = $false }; Expect = 'ERROR' }
        @{ Run = @{ Code = -1; Output = ''; TimedOut = $true }; Expect = 'TIMEOUT' }
    )
    foreach ($case in $cases) {
        $said = Get-RustVerdict ([pscustomobject] $case.Run)
        if ($said -ne $case.Expect) { $problems.Add("a planted Rust run came out $said, expected $($case.Expect)") }
    }

    # The same for the platform's English output. Two summaries, as a run over two test applications
    # prints them, must be added up.
    $csRed = @(
        'failed ChronoMock.App.Tests.ClockTests.A_clock_keeps_time (12ms)'
        '  Assert.Equal() Failure: Values differ'
        'failed ChronoMock.App.Tests.ClockTests.A_theory_case(value: "a b") (3ms)'
        ''
        'Test run summary: Failed!'
        '  total: 5'
        '  failed: 2'
        '  succeeded: 3'
        '  skipped: 0'
        '  duration: 1s 2ms'
    ) -join "`n"
    $csGreen = "Test run summary: Passed!`n  total: 1`n  failed: 0`n  succeeded: 1`n  skipped: 0`n  duration: 1s 528ms"
    $csTwo = "$csGreen`n$csGreen"
    $csRedNames = @(Get-CsRed $csRed) -join ','
    if ($csRedNames -ne 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time,ChronoMock.App.Tests.ClockTests.A_theory_case') { $problems.Add("the red C# tests read were '$csRedNames'") }
    if ((Get-CsSummary $csTwo).Total -ne 2) { $problems.Add('two C# summaries were not added up') }
    $cases = @(
        @{ Run = @{ Code = 2; Output = $csRed; TimedOut = $false }; Expect = 'caught' }
        @{ Run = @{ Code = 0; Output = $csGreen; TimedOut = $false }; Expect = 'MISSED' }
        @{ Run = @{ Code = 8; Output = 'Test run summary: Zero tests ran'; TimedOut = $false }; Expect = 'NO TEST' }
        @{ Run = @{ Code = 1; Output = 'the test host crashed'; TimedOut = $false }; Expect = 'ERROR' }
        @{ Run = @{ Code = 0; Output = 'nothing that looks like a summary'; TimedOut = $false }; Expect = 'ERROR' }
    )
    foreach ($case in $cases) {
        $said = Get-CsVerdict ([pscustomobject] $case.Run)
        if ($said -ne $case.Expect) { $problems.Add("a planted C# run came out $said, expected $($case.Expect)") }
    }

    # The same failure as a GitHub runner prints it, in colour - copied from the first run there, where
    # the colour codes made 197 caught entries read as ERROR. Every output passes ConvertTo-PlainText on
    # its way in, so the planted text does too.
    $csColoured = @(
        "`e[31mfailed`e[m ChronoMock.App.Tests.LiteralGuardTests.Views_have_no_design_literals `e[90m(165ms)`e[m"
        "Exit code: 2"
        "`e[31mTest run summary: Failed!"
        "`e[m  total: 1"
        "`e[31m  failed: 1"
        "`e[m  succeeded: 0"
        "  skipped: 0"
    ) -join "`n"
    $plain = ConvertTo-PlainText $csColoured
    if ($plain.Contains([char] 27)) { $problems.Add('a colour code was left in a tool''s output') }
    if ((Get-CsVerdict ([pscustomobject]@{ Code = 2; Output = $plain; TimedOut = $false })) -ne 'caught') { $problems.Add('a coloured C# failure did not come out caught') }
    if ((@(Get-CsRed $plain) -join ',') -ne 'ChronoMock.App.Tests.LiteralGuardTests.Views_have_no_design_literals') { $problems.Add('the red test of a coloured C# run was not read') }

    # And through the real door: a child process writes colour on both channels and exits with 3, and
    # what Invoke-Native hands back has the words, the exit code, and no colour.
    $child = Invoke-Native 'pwsh' @('-NoProfile', '-Command', '[Console]::Out.WriteLine([char] 27 + ''[31m  failed: 1''); [Console]::Error.WriteLine([char] 27 + ''[90mon the error channel''); exit 3') 1
    if ($child.Code -ne 3 -or $child.TimedOut -or $child.Output.Contains([char] 27) -or $child.Output -notmatch '(?m)^\s*failed: 1\s*$' -or -not $child.Output.Contains('on the error channel')) {
        $problems.Add("a child process came back with code $($child.Code) and '$($child.Output.Trim())' - expected 3, both channels and no colour")
    }

    # A baseline that failed without a red test the runner could read takes its whole batch as red - a
    # failed C# test's own exit code included - and only the platform's "zero tests ran" does not.
    $unclear = @((Test-BaselineUnclear 'cs' 2 0), (Test-BaselineUnclear 'cs' 8 0), (Test-BaselineUnclear 'cs' 0 0), (Test-BaselineUnclear 'cs' 2 1), (Test-BaselineUnclear 'rust' 101 0), (Test-BaselineUnclear 'rust' 101 1)) -join ','
    if ($unclear -ne 'True,False,False,False,True,False') { $problems.Add("the baseline took these as unclear: $unclear, expected True,False,False,False,True,False") }

    # Which red test an entry's filter stands for.
    $named = @(
        @{ Entry = @{ runner = 'rust'; test = 'tests::a_test_that_fails'; match = 'exact' }; Red = 'tests::a_test_that_fails'; Expect = $true }
        @{ Entry = @{ runner = 'rust'; test = 'tests::a_test'; match = 'exact' }; Red = 'tests::a_test_that_fails'; Expect = $false }
        @{ Entry = @{ runner = 'rust'; test = 'cdp::shim_exec::'; match = 'substring' }; Red = 'cdp::shim_exec::tests::one'; Expect = $true }
        @{ Entry = @{ runner = 'rust'; test = ''; match = '' }; Red = 'anything::at_all'; Expect = $true }
        @{ Entry = @{ runner = 'cs'; test = '*.A_clock_keeps_time'; match = 'method' }; Red = 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time'; Expect = $true }
        @{ Entry = @{ runner = 'cs'; test = '*ClockTests.A_clock_keeps_time'; match = 'method' }; Red = 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time'; Expect = $true }
        @{ Entry = @{ runner = 'cs'; test = 'ChronoMock.App.Tests.ClockTests.A_clock'; match = 'method' }; Red = 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time'; Expect = $false }
        @{ Entry = @{ runner = 'cs'; test = '*ClockTests'; match = 'class' }; Red = 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time'; Expect = $true }
        @{ Entry = @{ runner = 'cs'; test = '*OtherTests'; match = 'class' }; Red = 'ChronoMock.App.Tests.ClockTests.A_clock_keeps_time'; Expect = $false }
    )
    foreach ($case in $named) {
        if ((Test-NamesRed ([pscustomobject] $case.Entry) $case.Red) -ne $case.Expect) { $problems.Add("the filter '$($case.Entry.test)' against the red test '$($case.Red)' did not answer $($case.Expect)") }
    }

    # Mutating in memory, on files in the temporary folder and never in the tree: an anchor found once
    # is replaced, CRLF and a byte order mark survive the round trip, two edits apply in order, and an
    # anchor found twice or not at all is STALE.
    $realRoot = $Root
    $Root = Join-Path ([IO.Path]::GetTempPath()) ('chronomock-mutation-selftest-{0}' -f $PID)
    [void] [IO.Directory]::CreateDirectory($Root)
    try {
        [IO.File]::WriteAllBytes((Join-Path $Root 'crlf.txt'), [byte[]] (@([byte] 0xEF, [byte] 0xBB, [byte] 0xBF) + [Text.Encoding]::UTF8.GetBytes("one`r`ntwo`r`nthree`r`n")))
        [IO.File]::WriteAllText((Join-Path $Root 'twice.txt'), "same`nsame`n")
        $planned = Get-MutatedSources ([pscustomobject]@{ edits = @(
            [pscustomobject]@{ file = 'crlf.txt'; from = "one`ntwo"; to = "one`nTWO" }
            [pscustomobject]@{ file = 'crlf.txt'; from = 'TWO'; to = 'Two' }) })
        $bytes = ConvertTo-SourceBytes $planned.Sources[0] $planned.Sources[0].Mutated
        $expected = [byte[]] (@([byte] 0xEF, [byte] 0xBB, [byte] 0xBF) + [Text.Encoding]::UTF8.GetBytes("one`r`nTwo`r`nthree`r`n"))
        if ($planned.Stale -or (Get-Sha $bytes) -ne (Get-Sha $expected)) { $problems.Add('two edits on a CRLF file with a byte order mark did not come out as written, in its own shape') }
        if (-not (Get-MutatedSources ([pscustomobject]@{ edits = @([pscustomobject]@{ file = 'twice.txt'; from = 'same'; to = 'other' }) })).Stale) { $problems.Add('an anchor found twice was not STALE') }
        if (-not (Get-MutatedSources ([pscustomobject]@{ edits = @([pscustomobject]@{ file = 'twice.txt'; from = 'absent'; to = 'other' }) })).Stale) { $problems.Add('an anchor found nowhere was not STALE') }
        if (-not (Get-MutatedSources ([pscustomobject]@{ edits = @([pscustomobject]@{ file = 'missing.txt'; from = 'a'; to = 'b' }) })).Stale) { $problems.Add('a file that does not exist was not STALE') }

        # The recovery: a file still mutated gets its original back, a file somebody changed since is
        # named and left alone, and the recovery file is kept until every file is safe.
        $recovery = Join-Path $Root 'recovery.json'
        $sources = @(Get-MutatedSources ([pscustomobject]@{ edits = @([pscustomobject]@{ file = 'crlf.txt'; from = 'three'; to = 'THREE' }) })).Sources
        $original = [IO.File]::ReadAllBytes((Join-Path $Root 'crlf.txt'))
        Write-Recovery $recovery $sources
        [IO.File]::WriteAllBytes((Join-Path $Root 'crlf.txt'), (ConvertTo-SourceBytes $sources[0] $sources[0].Mutated))
        $back = Restore-FromRecovery $recovery
        if (-not $back.Safe -or (Get-Sha ([IO.File]::ReadAllBytes((Join-Path $Root 'crlf.txt')))) -ne (Get-Sha $original) -or [IO.File]::Exists($recovery)) { $problems.Add('a file a stopped run left mutated did not get its original back') }
        Write-Recovery $recovery $sources
        [IO.File]::WriteAllText((Join-Path $Root 'crlf.txt'), 'somebody wrote this since')
        $back = Restore-FromRecovery $recovery
        if ($back.Safe -or [IO.File]::ReadAllText((Join-Path $Root 'crlf.txt')) -ne 'somebody wrote this since' -or -not [IO.File]::Exists($recovery)) { $problems.Add('a file changed since a stopped run was overwritten, or the recovery was dropped') }
    }
    finally {
        [IO.Directory]::Delete($Root, $true)
        $Root = $realRoot
    }

    # THE SELECTION A PULL REQUEST GETS: one entry per way of being reached and one nothing reaches. A
    # synthetic index, so nothing here reads the tree.
    $index = [pscustomobject]@{
        Classes = @{ 'ClockTests' = [Collections.Generic.List[string]] @('gui/ChronoMock.App.Tests/ClockTests.cs') }
        Methods = @{ 'A_lone_method' = [Collections.Generic.List[string]] @('gui/ChronoMock.App.Tests/LoneTests.cs') }
        Functions = @{ 'a_core_test' = [Collections.Generic.List[string]] @('crates/core/src/lib.rs', 'crates/cli/src/other.rs') }
    }
    function New-Planted([string] $Name, [string] $File, [string] $Runner, [string] $Package, [string[]] $Target, [string] $Test, [string] $To = 'b') {
        $match = switch ($Runner) { 'cs' { 'method' } 'rust' { if ($Test) { 'exact' } else { '' } } default { '' } }
        return [pscustomobject]@{ name = $Name; edits = @([pscustomobject]@{ file = $File; from = 'a'; to = $To }); runner = $Runner; package = $Package; target = $Target; test = $Test; match = $match; build = ''; notHere = '' }
    }
    $now = @(
        New-Planted 'through its file' 'crates/core/src/a.rs' 'rust' 'chrono-core' @('--lib') 'tests::x'
        New-Planted 'through its C# guard' 'gui/ChronoMock.App/A.cs' 'cs' 'gui/ChronoMock.App.Tests' @() '*ClockTests.A_clock_keeps_time'
        New-Planted 'through its integration test' 'crates/cli/src/b.rs' 'rust' 'chrono-cli' @('--test', 'wire_keys') 'every_key'
        New-Planted 'through its unit test' 'crates/core/src/c.rs' 'rust' 'chrono-core' @('--lib') 'tests::a_core_test'
        New-Planted 'new' 'crates/core/src/d.rs' 'rust' 'chrono-core' @('--lib') 'tests::y'
        New-Planted 'changed' 'crates/core/src/e.rs' 'rust' 'chrono-core' @('--lib') 'tests::z' 'c'
        New-Planted 'untouched' 'crates/core/src/f.rs' 'rust' 'chrono-core' @('--lib') 'tests::w'
    )
    $then = @($now | Where-Object { $_.name -ne 'new' } | ForEach-Object {
        if ($_.name -eq 'changed') { New-Planted 'changed' 'crates/core/src/e.rs' 'rust' 'chrono-core' @('--lib') 'tests::z' } else { $_ }
    })
    $changed = @('crates/core/src/a.rs', 'gui/ChronoMock.App.Tests/ClockTests.cs', 'crates/cli/tests/wire_keys.rs', 'crates/core/src/lib.rs')
    $reached = @(Select-Reached $now $then $changed $index)
    $said = ($reached | ForEach-Object { "$($_.Entry.name)=$($_.Why)" }) -join ' / '
    $expected = 'through its file=its file / through its C# guard=its guard / through its integration test=its guard / through its unit test=its guard / new=a new entry / changed=a changed entry'
    if ($said -ne $expected) { $problems.Add("the selection said '$said', expected '$expected'") }
    if (@(Select-Reached $now $now @() $index).Count -ne 0) { $problems.Add('an unchanged list with no changed file selected something') }
    if ((@(Get-GuardFiles (New-Planted 'module' 'x' 'rust' 'chrono-cli' @('--bins') 'cdp::shim_exec::') $index) -join ',') -ne 'crates/cli/src/cdp/shim_exec.rs,crates/cli/src/cdp/shim_exec/') { $problems.Add('a module filter did not guard its module file and folder') }
    if ((@(Get-GuardFiles (New-Planted 'whole' 'x' 'rust' 'chrono-mech' @() '') $index) -join ',') -ne 'crates/mech/') { $problems.Add('an entry naming no test did not guard its whole crate') }
    if ((@((Find-TestClass '*.A_lone_method' $index).Places) -join ',') -ne 'gui/ChronoMock.App.Tests/LoneTests.cs') { $problems.Add('a filter naming only a method did not find its file') }

    # The trait on a class, on this method, on the method next door, and nowhere.
    $onClass = "[Trait(`"Category`", `"Integration`")]`npublic sealed class ClockTests`n{`n    [Fact]`n    public void Two() { }`n}"
    $onOther = "public sealed class ClockTests`n{`n    [Fact]`n    [Trait(`"Category`", `"Integration`")]`n    public void One() { }`n`n    [Fact]`n    public void Two() { }`n}"
    $onThis = "public sealed class ClockTests`n{`n    [Fact]`n    [Trait(`"Category`", `"Integration`")]`n    public async Task Two() { }`n}"
    if (-not (Test-TraitReaches $onClass 'ClockTests' 'Two')) { $problems.Add('a trait on the class did not reach its method') }
    if (-not (Test-TraitReaches $onThis 'ClockTests' 'Two')) { $problems.Add('a trait on the method did not reach it') }
    if (Test-TraitReaches $onOther 'ClockTests' 'Two') { $problems.Add('a trait on the method next door reached this one') }
    if (Test-TraitReaches 'public sealed class ClockTests { }' 'ClockTests' 'Two') { $problems.Add('a class with no trait was taken as Integration') }

    # The cut into parts: nothing lost, nothing twice, and the counts the workflow is given.
    $cut = @(0..2 | ForEach-Object { @(Select-Part (1..7) $_ 3) })
    if ((($cut | Sort-Object) -join ',') -ne '1,2,3,4,5,6,7') { $problems.Add("three parts of seven gave $($cut -join ',')") }
    $counts = @((Get-PartCount 0 $true), (Get-PartCount 1 $true), (Get-PartCount 41 $true), (Get-PartCount 400 $true), (Get-PartCount 745 $false), (Get-PartCount 10 $false)) -join ','
    if ($counts -ne '0,1,3,16,16,10') { $problems.Add("the part counts were $counts, expected 0,1,3,16,16,10") }
    $weekly = $FullParts
    $FullParts = 24
    $apart = @((Get-PartCount 745 $true), (Get-PartCount 745 $false)) -join ','
    $FullParts = $weekly
    if ($apart -ne '16,24') { $problems.Add("with the weekly count at 24 the parts were $apart, expected 16,24") }
    if ((Get-ShardList 0) -ne '[]' -or (Get-ShardList 3) -ne '[0,1,2]') { $problems.Add('the matrix for zero or three parts came out wrong') }

    # The registry check refuses what it must, and the JSON reader keeps a date a string.
    $twice = @((New-Planted 'one name' 'a' 'rust' 'chrono-core' @() ''), (New-Planted 'one name' 'b' 'rust' 'chrono-core' @() ''))
    if (@(Get-RegistryProblems $twice).Count -eq 0) { $problems.Add('two entries under one name passed the registry check') }
    if (@(Get-RegistryProblems @(New-Planted 'a|b' 'a' 'rust' 'chrono-core' @() '')).Count -eq 0) { $problems.Add('a name holding a bar passed the registry check') }
    if (@(Get-RegistryProblems @(New-Planted 'no reason' 'a' 'none' '' @() '')).Count -eq 0) { $problems.Add('an entry no runner judges passed without a reason') }
    if ((ConvertFrom-JsonText '{"from":"2026-10-16T02:07:00Z"}').from -isnot [string]) { $problems.Add('the JSON reader turned a date into something else than a string') }
    if (@(Get-RegistryProblems $mutations).Count -ne 0) { $problems.Add('the registry itself has problems') }

    if ($problems.Count) {
        $problems | ForEach-Object { Write-Output "self test FAILED: $_" }
        exit 1
    }
    Write-Output "self test passed: the Rust and C# outputs, the filters, the mutation in its file's own shape and the recovery answer as planted"
    Write-Output "self test passed: the selection, the trait, the cut into parts and the registry check answer as known in advance ($($mutations.Count) entries read)"
    exit 0
}

# ---------------------------------------------------------------------------------------------------
# Choosing the entries
# ---------------------------------------------------------------------------------------------------

if (-not ($Only -or $Base -or $Shard -or $PlanTo -or $All)) {
    Write-Output 'A run of every entry takes hours on one machine and is the weekly CI job''s work, so it is'
    Write-Output 'never started by accident. Name entries with -Only, compare with a base with -Base, take one'
    Write-Output 'part with -Shard i/k, or ask for every entry with -All.'
    exit 2
}

$recoveryPath = Get-RecoveryPath $Root
if (-not $PlanTo) {
    $back = Restore-FromRecovery $recoveryPath
    $back.Said | ForEach-Object { Write-Output "recovery: $_" }
    if (-not $back.Safe) {
        Write-Output "Stopping: the recovery file $recoveryPath is kept until those files are put right."
        exit 1
    }
}

$selected = @($mutations)

if ($Only) {
    $fragments = @($Only.Split('|') | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    $selected = @($selected | Where-Object {
        $name = $_.name
        @($fragments | Where-Object { $name.IndexOf($_, [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count -gt 0
    })
    Write-Output ("-Only: {0} of {1} entries hold one of {2} fragment(s)." -f $selected.Count, $mutations.Count, $fragments.Count)
}

$index = $null
if ($Base -or $CiScope) { $index = Get-SourceIndex $Root }
$selectedWhy = @{}

if ($Base) {
    $everything = ''
    $previous = @()
    $changedFiles = @()

    Push-Location $Root
    try {
        # Three dots: what the change did since it left the base, which is what a reviewer sees, and not
        # what the base gained since.
        $changedFiles = @(git diff --name-only "$Base...HEAD" 2>$null)
        if ($LASTEXITCODE -ne 0) { $everything = "git could not compare with $Base" }

        if (-not $everything -and @($changedFiles | Where-Object { $_ -in $runnerFiles }).Count -gt 0) {
            $everything = 'the runner, the report or the workflow changed'
        }
        if (-not $everything) {
            $baseText = @(git show "${Base}:tests/mutation/entries.json" 2>$null)
            if ($LASTEXITCODE -ne 0) {
                $everything = 'the base has no tests/mutation/entries.json'
            } else {
                try {
                    $previous = Read-Registry ($baseText -join "`n")
                } catch {
                    $everything = "the registry in the base could not be read: $($_.Exception.Message)"
                }
            }
        }
    }
    finally {
        Pop-Location
    }

    $before = $selected.Count
    if ($everything) {
        foreach ($entry in $selected) { $selectedWhy[$entry.name] = 'everything' }
        Write-Output ("-Base {0}: every entry, because {1}." -f $Base, $everything)
    } else {
        $reached = @(Select-Reached $selected $previous $changedFiles $index)
        $selected = @($reached | ForEach-Object { $_.Entry })
        foreach ($one in $reached) { $selectedWhy[$one.Entry.name] = $one.Why }
        Write-Output ("-Base {0}: {1} changed file(s), {2} of {3} entries reached." -f $Base, $changedFiles.Count, $selected.Count, $before)
        foreach ($group in ($reached | Group-Object Why | Sort-Object Name)) {
            Write-Output ("  {0,5}  through {1}" -f $group.Count, $group.Name)
        }
    }
    # Silence is forbidden: what was not run is said, not left to subtraction.
    Write-Output ("{0} entries were NOT selected. The weekly run of every entry is what judges them." -f ($before - $selected.Count))
}

if ($PlanTo) {
    $parts = Get-PartCount $selected.Count ([bool] $Base)
    $lines = @("entries=$($selected.Count)", "parts=$parts", "shards=$(Get-ShardList $parts)")
    # A bare newline: GitHub reads this file line by line, and Add-Content would choose the line end.
    [IO.File]::AppendAllText($PlanTo, (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
    Write-Output ''
    $lines | ForEach-Object { Write-Output "plan: $_" }
    exit 0
}

$partNumber = 0
$partCount = 1
$selectedTotal = $selected.Count

if ($Shard) {
    if ($Shard -notmatch '^(\d+)/(\d+)$' -or [int] $Matches[2] -lt 1 -or [int] $Matches[1] -ge [int] $Matches[2]) {
        throw "-Shard takes i/k with i counted from zero and below k, and was given '$Shard'"
    }
    $partNumber = [int] $Matches[1]
    $partCount = [int] $Matches[2]
    $selected = @(Select-Part $selected $partNumber $partCount)
    Write-Output ("part {0} of {1}: {2} of the {3} selected entries." -f $partNumber, $partCount, $selected.Count, $selectedTotal)
}

$notHere = @()
$judged = @()
foreach ($entry in $selected) {
    $why = Get-NotHere $entry $index
    if ($why) { $notHere += [pscustomobject]@{ Entry = $entry; Why = $why } } else { $judged += $entry }
}
if ($notHere.Count) { Write-Output ("{0} entries are NOT HERE - each says which machine judges it." -f $notHere.Count) }

# What -Json writes, kept up to date after every entry. A part that is stopped - a time limit, a
# cancelled run, a machine that went away - leaves finished = false, and report.ps1 reads that as a part
# that did not finish rather than as a smaller run.
$partState = [ordered]@{
    shard = $partNumber
    parts = $partCount
    selected = $selectedTotal
    entries = $selected.Count
    started = [DateTime]::UtcNow.ToString('o')
    seconds = 0
    canary = ''
    built = $true
    baselineRed = @()
    finished = $false
    results = [Collections.Generic.List[object]]::new()
}
$partClock = [Diagnostics.Stopwatch]::StartNew()
$partFile = if ($Json) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Json) } else { '' }

function Write-PartState {
    if (-not $partFile) { return }
    $partState.seconds = [Math]::Round($partClock.Elapsed.TotalSeconds, 1)
    [IO.File]::WriteAllText($partFile, ($partState | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
}

function Add-Result($Entry, [string] $Verdict, [double] $Seconds, [string] $Reason = '') {
    $partState.results.Add([ordered]@{
        verdict = $Verdict; name = $Entry.name; test = [string] $Entry.test; file = [string] @($Entry.edits)[0].file
        why = [string] $selectedWhy[$Entry.name]; notHere = $Reason; seconds = [Math]::Round($Seconds, 1)
    })
    Write-PartState
}

foreach ($one in $notHere) { Add-Result $one.Entry 'NOT HERE' 0 $one.Why }

if ($selected.Count -eq 0) {
    $partState.finished = $true
    Write-PartState
    Write-Output 'Nothing was selected, so nothing was proved. A fragment that matches no name is a typo, not a pass.'
    exit 3
}
if ($judged.Count -eq 0) {
    $partState.finished = $true
    Write-PartState
    Write-Output ("nothing to judge here: all {0} selected entries are NOT HERE." -f $selected.Count)
    foreach ($one in $notHere) { Write-Output ("  NOT HERE  {0}  ({1})" -f $one.Entry.name, $one.Why) }
    exit 0
}

# ---------------------------------------------------------------------------------------------------
# The run
# ---------------------------------------------------------------------------------------------------

function Stop-Part([string[]] $Sentences, $Run) {
    $partState.finished = $true
    Write-PartState
    Write-Output ''
    $Sentences | ForEach-Object { Write-Output $_ }
    if ($Run) { Get-Tail $Run.Output | ForEach-Object { Write-Output "  | $_" } }
    exit 1
}

$hasRust = @($judged | Where-Object { $_.runner -in 'rust', 'clippy' }).Count -gt 0
$csProjects = @($judged | Where-Object { $_.runner -eq 'cs' } | ForEach-Object { $_.package } | Sort-Object -Unique)
$hasRelease = @($judged | Where-Object { $_.build -eq 'release' }).Count -gt 0

# The clean tree first: the debug workspace for the tests that drive a session (cargo test never builds
# chrono_hook.dll), both release cores when an entry's test reads them, and the whole C# solution when the
# part has a C# entry - as CI builds it before its C# tests, not each test project alone, because a test
# reads what another project of the solution builds (the sample target a session test launches). A part
# that built the test projects alone found that test red before any mutation, on its first run in CI. A
# tree that does not build here cannot make a test red because of a mutation.
$cleanBuilds = @()
if ($hasRust) { $cleanBuilds += 'debug' }
if ($hasRelease) { $cleanBuilds += 'release' }
foreach ($kind in $cleanBuilds) {
    $artifacts = Invoke-ArtifactBuild $kind
    if (-not $artifacts.Ok) {
        $partState.built = $false
        Stop-Part @("THE UNMUTATED TREE DOES NOT BUILD ($kind), so no test here can be red because of a mutation. Stopping.") $artifacts.Run
    }
}
if ($csProjects.Count -gt 0) {
    $build = Invoke-CsBuild $csSolution
    if ($build.Code -ne 0) {
        $partState.built = $false
        Stop-Part @("THE UNMUTATED TREE DOES NOT BUILD ($csSolution), so no test here can be red because of a mutation. Stopping.") $build
    }
}

# The canaries, one per kind of runner the part uses.
$canaryVerdicts = @()
foreach ($canary in $canaries) {
    $needed = if ($canary.runner -eq 'cs') { $csProjects.Count -gt 0 } else { $hasRust }
    if (-not $needed) { continue }
    $verdict = Test-Mutation $canary
    $canaryVerdicts += $verdict.Verdict
    Write-Output ("{0,-12} {1}" -f $verdict.Verdict, $canary.name)
    if ($verdict.Verdict -ne 'BROKEN') {
        $partState.canary = "$($canary.runner) $($verdict.Verdict)"
        Stop-Part @('A canary did not come back BROKEN. This runner is not telling build failures apart from caught', 'mutations, so nothing below could be believed. Stopping.') $null
    }
}
$partState.canary = 'BROKEN'
# A canary leaves its crate or project built from the mutation that did not compile, which the next
# build puts right. The C# projects are built again here so that the baseline, which runs without a
# build, runs on the clean tree.
foreach ($project in $csProjects) {
    $build = Invoke-CsBuild $project
    if ($build.Code -ne 0) {
        $partState.built = $false
        Stop-Part @("$project did not build again after its canary. Stopping.") $build
    }
}
Write-PartState

$baseline = Invoke-Baseline $judged
$partState.baselineRed = [string[]] @($baseline.Red.Keys | Sort-Object)
Write-PartState
Write-Output ("baseline: {0} named test(s) run on the unmutated tree, {1} entr(ies) red before any mutation" -f $baseline.Ran, $baseline.Red.Count)
foreach ($line in $baseline.Said) { Write-Output "         $line" }

$results = @()
foreach ($entry in $judged) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $verdict = if ($baseline.Red.ContainsKey($entry.name)) { New-Verdict 'BASELINE RED' } else { Test-Mutation $entry }

    # An entry that rebuilt artifacts leaves them built from its mutation, and the next entry's test
    # would read them. They are built again from the restored tree before anything else runs.
    if ($entry.build -and $verdict.Verdict -notin 'STALE', 'BASELINE RED') {
        $again = Invoke-ArtifactBuild $entry.build
        if (-not $again.Ok) {
            Add-Result $entry $verdict.Verdict $clock.Elapsed.TotalSeconds
            $partState.built = $false
            Stop-Part @("The $($entry.build) build of the restored tree failed after '$($entry.name)', so the artifacts are still the mutation. Stopping.") $again.Run
        }
    }

    Add-Result $entry $verdict.Verdict $clock.Elapsed.TotalSeconds
    $results += [pscustomobject]@{ Verdict = $verdict.Verdict; Name = $entry.name; Test = [string] $entry.test }
    Write-Output ("{0,-12} {1}  ({2:N0} s)" -f $verdict.Verdict, $entry.name, $clock.Elapsed.TotalSeconds)
    foreach ($line in $verdict.Said) { Write-Output "             | $line" }
}

$partState.finished = $true
Write-PartState

# THE SOURCES ARE PUT BACK AND THE BINARIES ARE NOT. The last C# entry left its project built from the
# mutation, and a `dotnet test --no-build` afterwards would test that mutation and report a failure that
# looks exactly like a regression - the sister project paid twenty minutes for that once. So the clean
# tree is built here before anybody is told the run finished.
if ($NoClosingBuild) {
    Write-Output ''
    Write-Output 'NOT rebuilding at the end (-NoClosingBuild): the binaries are still the last mutation, which is'
    Write-Output 'harmless only because this tree is thrown away.'
} else {
    $closing = @()
    if ($hasRust) { $closing += (Invoke-ArtifactBuild 'debug').Ok }
    foreach ($project in $csProjects) { $closing += (Invoke-CsBuild $project).Code -eq 0 }
    if ($closing -contains $false) {
        Write-Output ''
        Write-Output 'THE CLOSING REBUILD FAILED. The sources are restored but some binaries are not - anything run'
        Write-Output 'without a build from here is testing a mutation. Build before believing it.'
    }
}

$caught = @($results | Where-Object { $_.Verdict -eq 'caught' }).Count
Write-Output ''
Write-Output ("{0} of {1} judged entries were caught, {2} NOT HERE." -f $caught, $results.Count, $notHere.Count)
foreach ($bad in $results | Where-Object { $_.Verdict -ne 'caught' }) {
    Write-Output ("  {0}: {1}  ->  {2}" -f $bad.Verdict, $bad.Name, $bad.Test)
}
foreach ($one in $notHere) {
    Write-Output ("  NOT HERE: {0}  ({1})" -f $one.Entry.name, $one.Why)
}
Write-Output ''
Write-Output 'A MISSED entry means the guard does not check what its name says. Fix the guard, never the'
Write-Output 'mutation. STALE and NO TEST mean the code or the test moved on and the entry proves nothing until'
Write-Output 'it is re-anchored. BASELINE RED names a test that is red with no mutation at all.'

if ($caught -ne $results.Count) { exit 1 }
exit 0

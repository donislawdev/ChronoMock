using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// Three constants exist twice - once in Rust, once in C# - each with a comment saying the two must
/// agree, and until now nothing checked that they did. The C# test for the speed limit compared
/// <c>MaxSpeed</c> against itself, which is true no matter what the core says.
///
/// A guard written in prose but absent from code is worse than none (untouchable rule 12), so these
/// read the Rust sources and compare. The technique is already in this repository: the architecture
/// guard parses <c>Cargo.toml</c>, <c>wire_keys.rs</c> reads Rust sources against the translation
/// files, and the literal guard reads the app's XAML. This is the same idea pointed the other way
/// across the language boundary - the direction nothing crossed before (R3-9).
///
/// These are deliberately textual. A build-time link between a Rust constant and a C# one would be
/// a code generator and a new moving part - a regex over one line of source is enough to fail loudly
/// on the day someone changes one side, which is the entire job.
/// </summary>
public class RustConstantMirrorTests
{
    private static string ReadRustSource(params string[] relativeParts)
    {
        var path = Path.Combine(new[] { TestPaths.RepoRoot() }.Concat(relativeParts).ToArray());
        Assert.True(File.Exists(path), $"expected a Rust source at '{path}'");
        return File.ReadAllText(path);
    }

    private static string CaptureOne(string source, string pattern, string what)
    {
        var matches = Regex.Matches(source, pattern);
        Assert.True(
            matches.Count == 1,
            $"expected exactly one match for {what} in the Rust source, found {matches.Count} - "
                + "the guard is reading the wrong thing, which is worse than not reading it");
        return matches[0].Groups[1].Value;
    }

    /// <summary>
    /// The About window states an SPDX identifier, and the workspace manifest declares one. Two halves of
    /// one product naming different licences would be the worst kind of quiet wrong: nothing breaks, the
    /// window looks authoritative, and it is telling a reader something untrue about their rights.
    /// </summary>
    [Fact]
    public void The_licence_the_window_states_matches_the_one_the_workspace_declares()
    {
        var manifest = ReadRustSource("Cargo.toml");
        var declared = CaptureOne(manifest, @"(?m)^license = ""([^""]+)""", "the workspace licence");

        Assert.True(
            declared == AppLicence.Spdx,
            $"Cargo.toml declares {declared} but the About window states {AppLicence.Spdx} - "
                + "change both together");
    }

    /// <summary>
    /// The About window cuts the core's notice off its component listing at the first group heading, so
    /// that the notice it has already stated in its own words does not appear a second time underneath.
    /// If the core rewords that heading, the cut silently stops finding it and the window goes back to
    /// showing everything twice - which looks like sloppiness rather than a bug, so nobody files it.
    /// </summary>
    [Fact]
    public void The_component_heading_the_window_cuts_at_is_the_one_the_core_prints()
    {
        var cli = ReadRustSource("crates", "cli", "src", "cli.rs");

        Assert.True(
            cli.Contains(LicenseClient.FirstGroupHeading, StringComparison.Ordinal),
            $"the core no longer prints \"{LicenseClient.FirstGroupHeading}\" - the About window cuts its "
                + "component list at that heading, and without it the whole notice appears twice");
    }

    /// <summary>
    /// The panel and <c>chrono run</c> skip a line on the core's stdout at the same length, and the core
    /// reads its commands with the same bound (R4-N52). A client with a longer bound would hand on a line
    /// the other reader gives up on, and one with a shorter bound would skip an event the core meant.
    /// <para>
    /// The Rust side is a product of factors (<c>1024 * 1024</c>), so the guard multiplies them rather than
    /// reading one literal - a rewrite into a single number is still read the same way.
    /// </para>
    /// </summary>
    [Fact]
    public void The_protocol_line_bound_matches_the_one_the_core_reads_with()
    {
        var wire = ReadRustSource("crates", "cli", "src", "wire.rs");
        var expression = CaptureOne(wire, @"const MAX_PROTOCOL_LINE: usize = ([0-9_ *]+);", "MAX_PROTOCOL_LINE");
        var value = expression
            .Split('*')
            .Select(factor => long.Parse(factor.Trim().Replace("_", string.Empty), System.Globalization.CultureInfo.InvariantCulture))
            .Aggregate(1L, (product, factor) => product * factor);

        Assert.True(
            value == ProtocolJson.MaxProtocolLine,
            $"the core reads protocol lines up to {value} bytes (MAX_PROTOCOL_LINE) but the panel up to "
                + $"{ProtocolJson.MaxProtocolLine} - change both together (R4-N52)");
    }

    /// <summary>
    /// The GUI validates the speed box before sending, as a courtesy - the core is the real gate.
    /// A courtesy that disagrees with the gate is worse than none: too low and the app refuses a
    /// speed the core accepts, too high and it offers one the core rejects mid-session (R2-K2).
    /// </summary>
    [Fact]
    public void The_speed_ceiling_matches_the_core_multiplier_max()
    {
        var lib = ReadRustSource("crates", "core", "src", "lib.rs");
        var rust = CaptureOne(lib, @"pub const MULTIPLIER_MAX: i64 = ([0-9_]+);", "MULTIPLIER_MAX");
        var value = long.Parse(rust.Replace("_", string.Empty), System.Globalization.CultureInfo.InvariantCulture);

        // Not Assert.Equal: the analyser insists a constant be the "expected" side, which would read
        // as if the C# value were the authority. It is not - the core is - and the message says so.
        Assert.True(
            value == SessionViewModel.MaxSpeed,
            $"the core allows up to {value} (MULTIPLIER_MAX) but the GUI caps at "
                + $"{SessionViewModel.MaxSpeed} - change both together (R2-K2)");
    }

    /// <summary>
    /// The window reads the preset catalogue only through the engine since R4/18, and both sides name the
    /// schema of that answer. If they drift, the window refuses every catalogue the engine writes and says
    /// "this window reads chronomock.presets/1" over an empty list - loudly, but for a reason nobody chose.
    /// </summary>
    [Fact]
    public void The_catalogue_schema_matches_the_one_the_engine_writes()
    {
        var catalogue = ReadRustSource("crates", "cli", "src", "preset_catalogue.rs");
        var rust = CaptureOne(catalogue, @"const PRESETS_SCHEMA: &str = ""([^""]+)"";", "PRESETS_SCHEMA");

        Assert.True(
            rust == PresetCatalogue.SupportedSchema,
            $"the engine writes catalogue schema '{rust}' but the window reads '{PresetCatalogue.SupportedSchema}'");
    }

    /// <summary>
    /// The note about a full process registry states the registry's size, on the command line and in
    /// the window in both languages. The size is a constant of the control block, and it changed once
    /// already (R5-P26) - a note still naming the old number would tell a reader the audit gave up far
    /// earlier, or later, than it did.
    /// </summary>
    [Fact]
    public void The_registry_size_the_notes_state_is_the_one_the_block_has()
    {
        var ctl = ReadRustSource("crates", "ctl", "src", "lib.rs");
        var size = CaptureOne(ctl, @"pub const MAX_COV_PIDS: usize = ([0-9_]+);", "MAX_COV_PIDS").Replace("_", string.Empty);
        var stated = $"({size})";

        var report = ReadRustSource("crates", "cli", "src", "report.rs");
        var cli = CaptureOne(report, @"""(this session ran more processes than the audit can follow[^""]*)""", "the CLI note");
        Assert.True(cli.Contains(stated, StringComparison.Ordinal), $"the CLI note does not state {stated}: {cli}");

        foreach (var language in new[] { "en", "pl" })
        {
            var path = Path.Combine(TestPaths.RepoRoot(), "gui", "ChronoMock.App", "Localization", $"Strings.{language}.json");
            // Read the way the app reads it: the string files carry comments and may end a list with a comma.
            using var strings = System.Text.Json.JsonDocument.Parse(
                File.ReadAllText(path),
                new System.Text.Json.JsonDocumentOptions
                {
                    CommentHandling = System.Text.Json.JsonCommentHandling.Skip,
                    AllowTrailingCommas = true,
                });
            var note = strings.RootElement.GetProperty("coverage.pid_registry_full").GetString() ?? string.Empty;
            Assert.True(note.Contains(stated, StringComparison.Ordinal), $"the {language} note does not state {stated}: {note}");
        }
    }

    /// <summary>
    /// The window and the CLI put the unreliable-evidence banner over a works session for the same
    /// warnings (R5-D3). A key on one list only would make the copied report and the evidence file of one
    /// session disagree about whether it proves anything.
    /// </summary>
    [Fact]
    public void The_warnings_that_undo_evidence_match_the_ones_the_cli_names()
    {
        var report = ReadRustSource("crates", "cli", "src", "report.rs");
        var list = CaptureOne(
            report,
            @"WARNINGS_THAT_UNDO_EVIDENCE: \[&str; \d+\] =\s*\[([^\]]*)\];",
            "WARNINGS_THAT_UNDO_EVIDENCE");
        var rust = Regex.Matches(list, @"""([^""]+)""").Select(m => m.Groups[1].Value).Order(StringComparer.Ordinal).ToArray();
        var window = SessionResult.WarningsThatUndoEvidence.Order(StringComparer.Ordinal).ToArray();

        Assert.True(rust.Length > 0, "the guard read no key from the CLI list, which is worse than not reading it");
        Assert.True(
            rust.SequenceEqual(window),
            $"the CLI flags evidence for [{string.Join(", ", rust)}] but the window for "
                + $"[{string.Join(", ", window)}] - change both together");
    }

    /// <summary>
    /// Both sides decide "is this a Chromium app?" from the same runtime files next to the exe, and
    /// they must decide it the same way: the GUI skips the bitness gate for a CDP target, so a
    /// disagreement means either a gate applied to a session that never needed it, or a native
    /// session started without the check that keeps it honest.
    /// </summary>
    [Fact]
    public void The_chromium_signature_files_match_the_ones_the_core_looks_for()
    {
        var launch = ReadRustSource("crates", "cli", "src", "cdp", "launch.rs");
        // The files are asked for in holds_runtime, which chromium_layout asks of the exe's own folder
        // first, and is_chromium_target answers from.
        var body = Regex.Match(
            launch,
            @"fn holds_runtime.*?\n\}",
            RegexOptions.Singleline);
        Assert.True(body.Success, "could not find holds_runtime in the Rust source");

        var rustFiles = Regex.Matches(body.Value, @"join\(""([^""]+)""\)")
            .Select(m => m.Groups[1].Value)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);

        // Assembled from the C# side by asking it, rather than by copying a list into this test -
        // a list copied here would be a third place to drift.
        var probed = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var dir = Path.Combine(Path.GetTempPath(), $"chrono-signature-probe-{Environment.ProcessId}");
        Directory.CreateDirectory(dir);
        try
        {
            var exe = Path.Combine(dir, "App.exe");
            File.WriteAllText(exe, string.Empty);
            foreach (var name in rustFiles)
            {
                var probe = Path.Combine(dir, name);
                File.WriteAllText(probe, string.Empty);
                probed.Add(name);
            }

            Assert.True(
                ChromiumTarget.IsChromium(exe),
                "the C# check refused a folder holding every file the core looks for: "
                    + string.Join(", ", rustFiles));

            // And the negative direction: remove any one of them and at least one side must say no,
            // so "signature" cannot quietly become "any folder".
            foreach (var name in probed)
            {
                var probe = Path.Combine(dir, name);
                File.Delete(probe);
                var stillChromium = ChromiumTarget.IsChromium(exe);
                File.WriteAllText(probe, string.Empty);
                if (!stillChromium)
                {
                    return; // one file is load-bearing on both sides - that is what we needed to see
                }
            }

            Assert.Fail("no signature file was load-bearing: the C# check would accept any folder");
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    /// <summary>The quoted words a Rust function returns from its match arms - <c>X => "word",</c>.</summary>
    private static List<string> ArmWords(string source, string function)
    {
        var body = CaptureOne(source, $@"(?s)fn {function}\([^)]*\) -> &'static str \{{(.*?)\n\}}", $"the body of {function}");
        return [.. Regex.Matches(body, @"=> ""([^""]+)""").Select(m => m.Groups[1].Value)];
    }

    /// <summary>
    /// The catalogue hands the window every unit, snap target and nearest target as the code the engine's
    /// grammar writes (<c>unit_code</c>, <c>snap_code</c>, <c>nearest_code</c>), and the window maps each code
    /// to an option of its step builder - which sends it straight back as a flag. A code with no option would
    /// leave a scenario's step on whatever the builder selected first (R4-S22 was the same fault from the
    /// other side: a word the window's own table did not know). Both directions: every code has an option,
    /// and every option is a code the engine reads.
    /// </summary>
    [Fact]
    public void The_builders_words_are_the_codes_the_engine_writes_in_the_catalogue()
    {
        var grammar = ReadRustSource("crates", "cli", "src", "grammar.rs");
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));

        Assert.Equal(ArmWords(grammar, "unit_code").Order(), vm.Units.Select(u => u.Token).Order());
        Assert.Equal(ArmWords(grammar, "snap_code").Order(), vm.SnapTargets.Select(t => t.Token).Order());
        Assert.Equal(ArmWords(grammar, "nearest_code").Order(), vm.NearestTargets.Select(t => t.Token).Order());
    }

    /// <summary>
    /// Keys the engine writes and the window turns into text: the labels of a variant parameter's choices
    /// (<c>VARIANTS</c>, shown as <c>calc.variant.*</c>), the readings of a pasted date
    /// (<c>DateReading::key</c>, <c>calc.reading.*</c>) and the landmarks a date lands on
    /// (<c>Significance::key</c>, <c>calc.sig.*</c>). A key without its text shows as the raw key - the two
    /// epoch readings did, in both languages, until R4-S23.
    /// </summary>
    [Fact]
    public void Every_label_the_engine_writes_has_its_text_in_both_languages()
    {
        var preset = ReadRustSource("crates", "cli", "src", "preset.rs");
        var calc = ReadRustSource("crates", "core", "src", "calc.rs");
        var variants = Regex.Matches(
                CaptureOne(preset, @"const VARIANTS: \[\(&str, i64\); \d+\] = \[(.*?)\];", "VARIANTS"),
                @"\(""([^""]+)"", -?\d+\)")
            .Select(m => $"calc.variant.{m.Groups[1].Value}");
        var readings = Regex.Matches(
                CaptureOne(calc, @"(?s)impl DateReading \{.*?pub fn key\(&self\) -> &'static str \{(.*?)\n    \}", "DateReading::key"),
                @"=> ""([^""]+)""")
            .Select(m => $"calc.reading.{m.Groups[1].Value}");
        var landmarks = Regex.Matches(
                CaptureOne(calc, @"(?s)impl Significance \{.*?pub fn key\(&self\) -> &'static str \{(.*?)\n    \}", "Significance::key"),
                @"=> ""([^""]+)""")
            .Select(m => $"calc.sig.{m.Groups[1].Value}");
        var keys = variants.Concat(readings).Concat(landmarks).ToList();
        Assert.True(keys.Count >= 3 + 5 + 13, $"read only {keys.Count} keys - the guard went blind");

        foreach (var language in new[] { "en", "pl" })
        {
            var strings = ReadRustSource("gui", "ChronoMock.App", "Localization", $"Strings.{language}.json");
            var missing = keys.Where(k => !strings.Contains($"\"{k}\":", StringComparison.Ordinal)).ToList();
            Assert.True(missing.Count == 0, $"Strings.{language}.json has no text for: {string.Join(", ", missing)}");
        }
    }
}

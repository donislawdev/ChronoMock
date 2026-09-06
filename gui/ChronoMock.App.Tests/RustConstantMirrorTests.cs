using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;
using ChronoMock.App.Calc;

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
/// a code generator and a new moving part; a regex over one line of source is enough to fail loudly
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
    /// Both readers of the preset catalogue gate on the schema string. If they drift, a preset the
    /// engine reads fine simply stops appearing in the GUI list - reported to the user as "preset
    /// not found" rather than "written for a newer schema", which points at the wrong thing (R2-S8).
    /// </summary>
    [Fact]
    public void The_preset_schema_matches_the_one_the_engine_accepts()
    {
        // Reads `preset.rs`, where the preset reader moved when `main.rs` was split into modules -
        // and this test found that move by going red, which is what it is for.
        //
        // Still anchored to the reader BY NAME rather than to the file: `main.rs` used to gate two
        // schemas (the calendar reader has the same shape) and the first version of this test matched
        // both, which the guard itself caught. The anchor survives the split for the same reason.
        var preset = ReadRustSource("crates", "cli", "src", "preset.rs");
        var presetReader = Regex.Match(preset, @"fn parse_preset.*?
\}", RegexOptions.Singleline);
        Assert.True(presetReader.Success, "could not find parse_preset in the Rust source");
        var rust = CaptureOne(presetReader.Value, @"dto\.schema != ""([^""]+)""", "the preset schema check");

        Assert.True(
            rust == PresetCatalog.SupportedSchema,
            $"the engine accepts preset schema '{rust}' but the GUI reader accepts "
                + $"'{PresetCatalog.SupportedSchema}' - a preset the engine reads would vanish from the list");
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
        var body = Regex.Match(
            launch,
            @"pub fn is_chromium_target.*?\n\}",
            RegexOptions.Singleline);
        Assert.True(body.Success, "could not find is_chromium_target in the Rust source");

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
}

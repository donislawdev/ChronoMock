using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;
using System.Windows;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Tests;

/// <summary>
/// The guard nobody had written. A view refers to interface text as
/// <c>{DynamicResource some.key}</c>, and a key that does not exist resolves to nothing: the label
/// renders EMPTY and every gate stays green, because no code path fails. It was checked by hand once
/// (104 keys, none missing) and a check done by hand is one that stops being done, which untouchable
/// rule 12 says is worse than none at all.
/// <para>
/// Translation keys are told apart from theme resources by their SHAPE, the same convention
/// <c>crates/cli/tests/wire_keys.rs</c> uses on the other side of the boundary: lowercase words
/// joined by dots. A brush or a spacing token is PascalCase with no dot, so <c>BrushTextPrimary</c>
/// and <c>SpaceXs</c> never enter this scan and a missing one of those still fails loudly at
/// startup, which is the reason they do not need this guard.
/// </para>
/// <para>
/// <b>What this does not prove.</b> Only keys written in XAML are covered. A key handed to the
/// KeyToText converter as DATA - a wire key from the core, a preset's label - is invisible here, and
/// that direction has its own guard in <c>wire_keys.rs</c>. Nor does it say anything about a key
/// that exists and holds the wrong text.
/// </para>
/// </summary>
public class XamlResourceKeyTests
{
    /// <summary>Lowercase dotted shape, matching the wire-key convention. The trailing brace has to be
    /// there so a key is never matched out of the middle of something longer.</summary>
    private static readonly Regex DynamicKey = new(
        @"\{DynamicResource\s+([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+)\s*\}",
        RegexOptions.Compiled);

    [Fact]
    public void Every_translation_key_used_in_a_view_exists_in_both_languages()
    {
        var used = ScanViewKeys();

        // Two canaries, because a scan that reads nothing is the failure mode this guard has: it would
        // report "no missing keys" over an empty set and look exactly like success. The count is a
        // literal, not derived from the same scan it checks, and the named key is one that has been in
        // the title bar since the first window.
        Assert.True(
            used.Count >= 100,
            $"the scan found only {used.Count} keys in the views - it is reading the wrong files, "
                + "which would make every assertion below vacuous");
        Assert.Contains("app.title", used.Keys);

        var en = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("en")));
        var pl = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("pl")));

        var missing = used
            .Where(pair => !en.Contains(pair.Key) || !pl.Contains(pair.Key))
            .Select(pair => $"{pair.Key} (used in {pair.Value})")
            .Order(StringComparer.Ordinal)
            .ToList();

        Assert.True(
            missing.Count == 0,
            "these keys are referenced from a view but missing from Strings.en.json and/or "
                + $"Strings.pl.json, so they would render as an empty label: {string.Join(", ", missing)}");
    }

    /// <summary>Every dotted DynamicResource key in the app's views, mapped to the file it was seen in
    /// first, so a failure says where to look rather than only what is wrong.</summary>
    private static Dictionary<string, string> ScanViewKeys()
    {
        var app = TestPaths.AppDirectory();
        var found = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var file in Directory.EnumerateFiles(app, "*.xaml", SearchOption.AllDirectories))
        {
            var rel = Path.GetRelativePath(app, file).Replace('\\', '/');
            if (rel.StartsWith("bin/", StringComparison.OrdinalIgnoreCase)
                || rel.StartsWith("obj/", StringComparison.OrdinalIgnoreCase)
                || rel.Contains("/bin/", StringComparison.OrdinalIgnoreCase)
                || rel.Contains("/obj/", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            foreach (Match m in DynamicKey.Matches(File.ReadAllText(file)))
            {
                found.TryAdd(m.Groups[1].Value, rel);
            }
        }

        return found;
    }

    private static HashSet<string> KeysOf(ResourceDictionary dictionary)
        => dictionary.Keys.Cast<object>().Select(key => key.ToString()!).ToHashSet(StringComparer.Ordinal);
}

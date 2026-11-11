using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The copied summary says everything the CLI report says (R5-D64). The core's test records one native
/// session in <c>tests/wire/session.ndjson</c> and the CLI report of it in <c>tests/wire/session-report.txt</c>
/// (<c>crates/cli/src/run/collect.rs</c>). Here the same events go into the session panel, and every section
/// heading of that report needs its line in the summary - a section added to the report and never to the
/// window turns this red, where before it was found by a reader comparing the two by hand, or not at all.
/// </summary>
public class SummaryParityTests
{
    /// <summary>Each CLI report section (its heading up to the colon or the bracket) and the summary key that
    /// says the same thing.</summary>
    private static readonly Dictionary<string, string> Counterpart = new(StringComparer.Ordinal)
    {
        ["target"] = "report.target",
        ["verdict"] = "report.verdict",
        ["session"] = "report.session",
        ["exited"] = "report.target_exit",
        ["followed"] = "report.followed",
        ["copies"] = "report.copies_outside",
        ["covered channels"] = "coverage.covered",
        ["observed channels"] = "coverage.observed",
        ["uncovered channels"] = "coverage.uncovered",
        ["channels we meant to watch and could not hook"] = "coverage.unobserved",
        ["channels hooked only once their module loaded"] = "coverage.installed_late",
        ["processes this app spawned that the hook could not follow"] = "report.processes_uncovered",
        ["web engines reached inside the app"] = "report.engines_reached",
        ["warnings"] = "coverage.warnings",
        ["not fully cleaned up"] = "report.cleanup",
        ["errors"] = "report.errors",
    };

    /// <summary>
    /// Sections the CLI report has and the summary knowingly does not, each with why and what makes it go.
    /// Checked from both ends: an entry the recorded report no longer has comes out. Only shrinks - and it is
    /// empty since R5/6b, which listed the last one, the errors (R5-D71).
    /// </summary>
    private static readonly (string Section, string Why)[] KnownCliOnly = [];

    private static string Wire(string name) => Path.Combine(TestPaths.RepoRoot(), "tests", "wire", name);

    /// <summary>The section headings of a CLI report: its lines that start two spaces in with a word, up to the
    /// colon, the bracket where the heading explains itself, or the end of the line - a heading written without
    /// a colon still reaches the pairing below, which names it, rather than throwing here.</summary>
    private static List<string> Sections(IEnumerable<string> report) =>
    [
        .. report
            .Where(line => line.Length > 2 && line.StartsWith("  ", StringComparison.Ordinal) && char.IsLetter(line[2]))
            .Select(line => line[2..].Split(':')[0].Split(" (")[0].Trim()),
    ];

    /// <summary>The lines under one section of a CLI report, up to the next heading.</summary>
    private static List<string> Under(IReadOnlyList<string> report, string section) =>
    [
        .. report
            .SkipWhile(line => !line.StartsWith("  " + section, StringComparison.Ordinal))
            .Skip(1)
            .TakeWhile(line => !(line.Length > 2 && line.StartsWith("  ", StringComparison.Ordinal) && char.IsLetter(line[2]))),
    ];

    /// <summary>The lines a summary list holds under its heading key, without their bullet.</summary>
    private static List<string> Listed(IReadOnlyList<string> summary, string key) =>
    [
        .. summary
            .SkipWhile(line => !line.StartsWith("  " + key + " (", StringComparison.Ordinal))
            .Skip(1)
            .TakeWhile(line => line.StartsWith("    - ", StringComparison.Ordinal))
            .Select(line => line["    - ".Length..]),
    ];

    /// <summary>
    /// The same sections say the same things: every warning the report names, by the key it closes its line
    /// with, is in the summary's warnings, and a count the report gives as a floor ("at least") is a floor in
    /// the summary too. Headings alone would pass a summary that kept the section and lost what it says.
    /// </summary>
    [Fact]
    public void What_the_report_says_inside_its_sections_the_summary_says_too()
    {
        var report = File.ReadAllLines(Wire("session-report.txt"));
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\app.exe");
        foreach (var line in File.ReadAllLines(Wire("session.ndjson")))
        {
            vm.Apply(EventParser.Parse(line) ?? throw new InvalidOperationException("an event the window does not know: " + line));
        }

        var summary = vm.BuildSummary(key => key).Split('\n');

        var warned = Under(report, "warnings:").Select(line => line[(line.LastIndexOf('(') + 1)..].TrimEnd(')')).ToList();
        Assert.True(warned.Count >= 2, $"read {warned.Count} warnings - the recorded session should carry two");
        Assert.Empty(warned.Except(Listed(summary, "coverage.warnings")));

        var floors = Under(report, "covered channels").Count(line => line.Contains("(at least ", StringComparison.Ordinal));
        Assert.True(floors >= 1, "the recorded session should have a count at the hook's ceiling");
        Assert.Equal(floors, Listed(summary, "coverage.covered").Count(line => line == "coverage.reads_at_least"));

        // How long the machine slept: the report says it inside its warning line, the summary in the session's
        // block (R5-D71). The warning alone, which both have, says THAT it slept and not for how long.
        Assert.Contains(Under(report, "warnings:"), line => line.Contains("the computer slept for ", StringComparison.Ordinal));
        Assert.Contains(summary, line => line == "    report.slept");

        // Every error the report lists, by the key it closes its line with, is listed in the summary.
        var errors = Under(report, "errors:").Select(line => line[(line.LastIndexOf('[') + 1)..].TrimEnd(']')).ToList();
        Assert.True(errors.Count >= 1, "the recorded session should carry an error the core sent while it ran");
        Assert.Empty(errors.Except(Listed(summary, "report.errors")));
    }

    [Fact]
    public void Every_section_of_the_cli_report_has_its_line_in_the_copied_summary()
    {
        var sections = Sections(File.ReadAllLines(Wire("session-report.txt")));
        // The canary: a heading rule that matched nothing would pass for a window that says nothing.
        Assert.True(sections.Count >= 10, $"read {sections.Count} sections - is session-report.txt the core's file?");

        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\app.exe");
        foreach (var line in File.ReadAllLines(Wire("session.ndjson")))
        {
            vm.Apply(EventParser.Parse(line) ?? throw new InvalidOperationException("an event the window does not know: " + line));
        }

        var summary = vm.BuildSummary(key => key).Split('\n');
        var missing = new List<string>();
        foreach (var section in sections.Where(s => KnownCliOnly.All(k => k.Section != s)))
        {
            Assert.True(
                Counterpart.TryGetValue(section, out var key),
                $"the CLI report has a section '{section}' this test does not pair with the summary - give the window "
                + "its line and pair them here, or list it in KnownCliOnly with why");
            // The key as the line's own heading: a colon, a count in brackets or a value after a space follows it.
            if (!summary.Any(l => l.StartsWith("  " + key, StringComparison.Ordinal)
                                  && (l.Length == key.Length + 2 || l[key.Length + 2] is ':' or ' ')))
            {
                missing.Add($"{section} ({key})");
            }
        }

        Assert.True(missing.Count == 0, "the copied summary leaves out what the CLI report says: " + string.Join("; ", missing));
        // From both ends: a pair or an exception the recorded report no longer needs has to come out.
        Assert.Equal(Counterpart.Keys.Order(StringComparer.Ordinal), sections.Except(KnownCliOnly.Select(k => k.Section)).Order(StringComparer.Ordinal));
        Assert.All(KnownCliOnly, k => Assert.Contains(k.Section, sections));
    }
}

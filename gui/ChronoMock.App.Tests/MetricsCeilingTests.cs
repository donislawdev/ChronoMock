using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App.Tests;

/// <summary>
/// The guard that the metric ceilings still exist, which the ceilings themselves cannot be.
///
/// CA1502 (cyclomatic complexity) and CA1506 (class coupling) are enforced by the compiler under
/// <c>TreatWarningsAsErrors</c>, and their numbers live in <c>gui/CodeMetricsConfig.txt</c>. Nothing
/// here re-measures the code. What it guards is everything around those numbers: that both rules
/// are still switched on, that the build still hands the analyzer its thresholds, that the
/// tightened copy is exactly one below on every rule, and that CI still runs the build that proves
/// each ceiling IS the measurement rather than headroom.
///
/// The Rust counterpart is <c>crates/cli/tests/shape.rs</c>. The two are deliberately the same
/// shape, because the failure they prevent is the same in both languages: a ceiling left standing
/// above the truth grants headroom nobody decided to grant, and the next arrival slips in under it
/// in silence.
///
/// One of these is not merely theoretical bookkeeping. If the AdditionalFiles wiring disappeared,
/// both rules would fall back to the analyzer's own defaults, and CA1502's default of 25 is above
/// everything in this codebase - so that ceiling would quietly stop existing while the build stayed
/// green.
/// </summary>
public class MetricsCeilingTests
{
    private const string RealConfig = "gui/CodeMetricsConfig.txt";
    private const string TightConfig = ".github/metrics-tight/CodeMetricsConfig.txt";

    /// <summary>
    /// Rule id to threshold, from a CodeMetricsConfig file. Comments and blank lines are skipped,
    /// which is the same shape the analyzer itself accepts.
    /// </summary>
    private static Dictionary<string, int> ReadThresholds(string relativePath)
    {
        var path = Path.Combine(TestPaths.RepoRoot(), relativePath.Replace('/', Path.DirectorySeparatorChar));
        Assert.True(File.Exists(path), $"expected a metrics configuration at '{path}'");

        var thresholds = new Dictionary<string, int>(StringComparer.Ordinal);
        foreach (var raw in File.ReadAllLines(path))
        {
            var line = raw.Trim();
            if (line.Length == 0 || line.StartsWith('#'))
            {
                continue;
            }

            var parts = line.Split(':', 2);
            Assert.True(parts.Length == 2, $"'{line}' in {relativePath} is neither a comment nor a rule");
            thresholds[parts[0].Trim()] = int.Parse(parts[1].Trim(), System.Globalization.CultureInfo.InvariantCulture);
        }

        // The canary every scanning guard needs: an empty file satisfies a loop over its contents
        // perfectly and looks exactly like a guard that works.
        Assert.True(thresholds.Count >= 2, $"{relativePath} declares only {thresholds.Count} rule(s)");
        return thresholds;
    }

    private static string ReadRepoFile(string relativePath)
    {
        var path = Path.Combine(TestPaths.RepoRoot(), relativePath.Replace('/', Path.DirectorySeparatorChar));
        Assert.True(File.Exists(path), $"expected a file at '{path}'");
        return File.ReadAllText(path);
    }

    /// <summary>
    /// The pinning half. Only a threshold exactly one below the real one turns the inversion build
    /// into a proof that the ceiling is touched.
    /// </summary>
    [Fact]
    public void The_tightened_configuration_is_exactly_one_below_every_ceiling()
    {
        var real = ReadThresholds(RealConfig);
        var tight = ReadThresholds(TightConfig);

        Assert.Equal(real.Keys.OrderBy(k => k, StringComparer.Ordinal), tight.Keys.OrderBy(k => k, StringComparer.Ordinal));

        foreach (var (rule, ceiling) in real)
        {
            Assert.True(
                tight[rule] == ceiling - 1,
                $"{rule} is {ceiling} but the tightened copy says {tight[rule]}. The inversion build only " +
                "proves the ceiling is the measurement when it is exactly one lower");
        }
    }

    /// <summary>
    /// A threshold that is configured but never enforced is a gate nobody runs.
    /// </summary>
    [Fact]
    public void Both_metric_rules_are_still_switched_on()
    {
        var editorConfig = ReadRepoFile("gui/.editorconfig");
        foreach (var rule in ReadThresholds(RealConfig).Keys)
        {
            Assert.Contains($"dotnet_diagnostic.{rule}.severity = warning", editorConfig, StringComparison.Ordinal);
        }
    }

    /// <summary>
    /// Without this item the analyzer never sees a threshold and silently uses its own defaults.
    /// </summary>
    [Fact]
    public void The_build_still_hands_the_analyzer_its_thresholds()
    {
        var props = ReadRepoFile("gui/Directory.Build.props");
        Assert.Contains("<AdditionalFiles Include=\"$(ChronoMetricsConfig)\" />", props, StringComparison.Ordinal);
        Assert.Contains("CodeMetricsConfig.txt", props, StringComparison.Ordinal);
    }

    /// <summary>
    /// The needle is the command line, not the two words in it. A search for the property name
    /// alone would be satisfied by the comment explaining the step, and by the message the step
    /// prints when it fails - an assertion that its own prose can satisfy is not an assertion.
    /// </summary>
    [Fact]
    public void Ci_still_runs_the_inversion_that_pins_the_ceilings()
    {
        var workflow = ReadRepoFile(".github/workflows/ci.yml");
        Assert.Contains(
            "-p:ChronoMetricsConfig=${{ github.workspace }}/.github/metrics-tight/CodeMetricsConfig.txt",
            workflow,
            StringComparison.Ordinal);
    }
}

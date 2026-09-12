using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App.Tests;

/// <summary>
/// Proves the literal guard reddens on a real literal (deterministic, via synthetic input - stronger than
/// hand-inserting and removing one) and that the real views are clean (zasady/13 section 5).
/// </summary>
public class LiteralGuardTests
{
    [Fact]
    public void Views_have_no_design_literals()
    {
        var violations = XamlLiteralGuard.ScanConsumers(TestPaths.AppDirectory());
        Assert.True(
            violations.Count == 0,
            "design literals found in views (use a named resource):\n"
                + string.Join("\n", violations.Select(v => $"  {v.File}:{v.Line} [{v.Kind}] {v.Snippet}")));
    }

    [Fact]
    public void Guard_reddens_on_a_literal_colour()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<TextBlock Foreground="#FFFF0000" />"""));

    [Fact]
    public void Guard_reddens_on_a_literal_font_size()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<TextBlock FontSize="13" />"""));

    [Fact]
    public void Guard_reddens_on_a_literal_margin()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<Border Margin="7" />"""));

    [Fact]
    public void Guard_allows_named_resources_and_zero()
    {
        Assert.Empty(XamlLiteralGuard.FindViolations(
            "x.xaml",
            """<TextBlock Foreground="{DynamicResource BrushTextPrimary}" FontSize="{StaticResource FontSizeBody}" Margin="{StaticResource SpaceSm}" />"""));
        Assert.Empty(XamlLiteralGuard.FindViolations("x.xaml", """<Border Margin="0" />"""));
    }

    /// <summary>
    /// 🔴 The guard walks the style dictionary, where control templates live. Asserting the VISITED set
    /// rather than the empty result is the point: a guard that quietly stops visiting Controls.xaml
    /// returns zero violations and is indistinguishable from a clean one.
    /// </summary>
    [Fact]
    public void Guard_walks_the_style_dictionary_and_skips_the_value_dictionaries()
    {
        var walked = XamlLiteralGuard.ConsumerPaths(TestPaths.AppDirectory());

        Assert.Contains("Themes/Controls.xaml", walked);
        Assert.Contains("MainWindow.xaml", walked);
        Assert.DoesNotContain("Themes/Colours.xaml", walked);
        Assert.DoesNotContain("Themes/Values.xaml", walked);
    }

    /// <summary>
    /// Every standing allowance still matches a line that is really there. An allowance whose line has
    /// been rewritten or deleted is an excuse with nothing left to excuse, and it has to come out -
    /// otherwise the list grows quietly and the next literal hides behind a dead entry.
    /// </summary>
    [Fact]
    public void Every_allowance_still_covers_a_line_that_exists()
    {
        var appDirectory = TestPaths.AppDirectory();
        var stale = new List<string>();

        foreach (var (file, line, reason) in XamlLiteralGuard.Allowances)
        {
            var path = Path.Combine(appDirectory, file.Replace('/', Path.DirectorySeparatorChar));
            var present = File.Exists(path)
                && File.ReadAllLines(path).Any(l => string.Equals(l.Trim(), line, StringComparison.Ordinal));
            if (!present)
            {
                stale.Add($"  {file}: {line}   (reason: {reason})");
            }
        }

        Assert.True(
            stale.Count == 0,
            "these allowances no longer match anything and must be deleted:\n" + string.Join("\n", stale));
    }

    /// <summary>
    /// An allowance silences ONE exact line and nothing else. Without this, widening an allowance by
    /// accident would switch the guard off across a whole file with no test noticing.
    /// </summary>
    [Fact]
    public void An_allowance_silences_only_its_own_line()
    {
        var (file, line, _) = XamlLiteralGuard.Allowances[0];

        Assert.Empty(XamlLiteralGuard.FindViolations(file, line));
        Assert.NotEmpty(XamlLiteralGuard.FindViolations(file, """<Border Margin="7" />"""));
        Assert.NotEmpty(XamlLiteralGuard.FindViolations("Themes/Somewhere.xaml", line));
    }
}

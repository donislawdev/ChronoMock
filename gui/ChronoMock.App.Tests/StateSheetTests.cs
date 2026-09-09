using System.Windows;
using ChronoMock.App;
using ChronoMock.App.Views;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// Writes the contact sheet: every screen, laid out and rendered, as a picture and a text dump.
/// </summary>
/// <remarks>
/// Tagged Integration so it stays out of the hermetic gate. It is a TOOL rather than a check - it asserts
/// only that it produced something, because its output is meant to be looked at. The rules that hold a
/// layout to account live in <see cref="LayoutGuardTests"/> and run on every build.
///
/// Run it with: pwsh tools/gui/render-states.ps1
/// </remarks>
[Trait("Category", "Integration")]
public class StateSheetTests
{
    [Fact]
    public void The_substitution_panel_renders_in_its_startup_state()
    {
        var elements = WpfTestHost.InvokeSettled(() =>
            StateSheet.Write("panel-startup", (FrameworkElement)new MainWindow().Content));

        Assert.NotEmpty(elements);
    }

    [Fact]
    public void The_calculator_renders_in_its_startup_state()
    {
        var elements = WpfTestHost.InvokeSettled(() =>
            StateSheet.Write("calculator-startup", new CalculatorView()));

        Assert.NotEmpty(elements);
    }

    [Fact]
    public void Both_screens_render_at_the_smallest_size_the_window_allows()
    {
        // MinWidth 1000 and MinHeight 360 from MainWindow.xaml. This is where a layout gives way first,
        // and it is the size nobody ever looks at, because reaching it means dragging a live window to
        // its floor by hand.
        var elements = WpfTestHost.InvokeSettled(() =>
        {
            var panel = StateSheet.Write(
                "panel-smallest", (FrameworkElement)new MainWindow().Content, MinimumWidth, MinimumHeight);
            var calculator = StateSheet.Write(
                "calculator-smallest", new CalculatorView(), MinimumWidth, MinimumHeight);
            return panel.Count + calculator.Count;
        });

        Assert.True(elements > 0);
    }

    [Fact]
    public void The_about_window_renders()
    {
        // Never seen on a render before this. It shipped in PR 15 and every claim about it stood on
        // bindings and unit tests.
        var elements = WpfTestHost.InvokeSettled(() =>
        {
            var about = new AboutDialog(LicenseClient.ForRepo(TestPaths.RepoRoot()));
            return StateSheet.Write("about", (FrameworkElement)about.Content, AboutWidth, AboutHeight);
        });

        Assert.NotEmpty(elements);
    }

    [Fact]
    public void The_panel_renders_in_every_state_a_session_passes_through()
    {
        // Seven states that a live run reaches only by spawning a core, waiting for a verdict and
        // provoking a failure. Here each is a view model with events applied to it.
        var written = WpfTestHost.InvokeSettled(() =>
        {
            var states = new (string Name, SessionViewModel Model)[]
            {
                ("panel-running", SessionStates.Running()),
                ("panel-coverage-warnings", SessionStates.RunningWithCoverageWarnings()),
                ("panel-ended", SessionStates.Ended()),
                ("panel-refused", SessionStates.Refused()),
                ("panel-inflight-error", SessionStates.InFlightError()),
                ("panel-vanished", SessionStates.TargetVanished()),
                ("panel-start-error", SessionStates.StartError()),
            };

            var total = 0;
            foreach (var (name, model) in states)
            {
                total += RenderPanel(name, model).Count;
            }

            return total;
        });

        Assert.True(written > 0);
    }

    [Fact]
    public void Every_line_of_text_is_measured_against_the_surface_it_was_painted_on()
    {
        var summary = WpfTestHost.InvokeSettled(() =>
        {
            var lines = new List<string>();
            foreach (var (name, root) in TextBearingScreens())
            {
                LayoutProbe.Settle(root);
                var readings = ContrastReport.Measure(
                    root, LayoutProbe.Walk(root), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
                File.WriteAllText(
                    Path.Combine(StateSheet.OutputDirectory, name + ".contrast.txt"),
                    ContrastReport.Describe(readings));
                lines.Add($"{name}: {readings.Count} readings, "
                    + $"{readings.Count(r => r.TooFaint)} too faint, "
                    + $"{readings.Count(r => r.OffScale)} off the type scale");
            }

            return string.Join('\n', lines);
        });

        Assert.NotEmpty(summary);
    }

    [Fact]
    public void Sweep_both_screens_across_every_window_size_and_report_what_gives_way()
    {
        var report = WpfTestHost.InvokeSettled(() =>
        {
            var lines = new List<string>();
            foreach (var size in SizeSweep.Sizes)
            {
                foreach (var (name, root) in TextBearingScreens())
                {
                    var complaints = SizeSweep.Inspect(root, size);
                    lines.Add($"{SizeSweep.Describe(size),-24} {name,-20} {complaints.Count} complaint(s)");
                    lines.AddRange(complaints.Select(c => "    " + c));
                    StateSheet.Write($"sweep-{name}-{size.Name}", root, size.Width, size.Height);
                }
            }

            return lines;
        });

        File.WriteAllLines(Path.Combine(StateSheet.OutputDirectory, "size-sweep.txt"), report);
        Assert.NotEmpty(report);
    }

    [Fact]
    public void Report_every_gap_the_layout_actually_produced()
    {
        var summary = WpfTestHost.InvokeSettled(() =>
        {
            var lines = new List<string>();
            foreach (var (name, root) in TextBearingScreens())
            {
                LayoutProbe.Settle(root);
                var gaps = SpacingReport.Measure(LayoutProbe.Walk(root));
                File.WriteAllText(
                    Path.Combine(StateSheet.OutputDirectory, name + ".spacing.txt"),
                    SpacingReport.Describe(gaps));
                lines.Add($"{name}: {gaps.Count} gaps, {gaps.Count(g => g.OffScale)} off the scale");
            }

            return lines;
        });

        Assert.NotEmpty(summary);
    }

    [Fact]
    public void Report_how_much_text_is_trimmed_away_on_each_screen()
    {
        var summary = WpfTestHost.InvokeSettled(() =>
        {
            var lines = new List<string>();
            foreach (var (name, root) in TextBearingScreens())
            {
                LayoutProbe.Settle(root);
                var walk = LayoutProbe.Walk(root);
                var trimmed = LayoutRules.TrimmedAway(walk);
                var spilled = LayoutRules.SpillsOutOfItsParent(walk);
                lines.Add($"{name}: {trimmed.Count} trimmed, {spilled.Count} spilling");
                lines.AddRange(trimmed);
                lines.AddRange(spilled);
            }

            return lines;
        });

        File.WriteAllLines(Path.Combine(StateSheet.OutputDirectory, "trimmed.txt"), summary);
        Assert.NotEmpty(summary);
    }

    [Fact]
    public void Report_where_each_rendered_value_came_from()
    {
        // Answers "is this ours or the library's" for every named element, which is the question that
        // costs a session every time a colour or a size comes out wrong on a wpfui control.
        var pattern = Environment.GetEnvironmentVariable("CHRONO_WHY") ?? string.Empty;
        var text = WpfTestHost.InvokeSettled(() =>
        {
            var answers = ValueSourceReport.Ask((FrameworkElement)new MainWindow().Content, pattern)
                .Concat(ValueSourceReport.Ask(new CalculatorView(), pattern))
                .ToList();
            return ValueSourceReport.Describe(answers);
        });

        File.WriteAllText(Path.Combine(StateSheet.OutputDirectory, "value-sources.txt"), text);
        Assert.NotEmpty(text);
    }

    private static IEnumerable<(string Name, FrameworkElement Root)> TextBearingScreens()
    {
        yield return ("panel-startup", (FrameworkElement)new MainWindow().Content);
        yield return ("calculator-startup", new CalculatorView());
    }

    /// <summary>
    /// The window with a given session model bound to it.
    /// </summary>
    /// <remarks>
    /// The relative-moment row is pointed at its own model separately, exactly as MainWindow does in its
    /// constructor. Without that line the row keeps the model the window built for itself, and the state
    /// under test would be half applied - a sheet that looks right and shows two different sessions.
    /// </remarks>
    private static IReadOnlyList<LaidOutElement> RenderPanel(string name, SessionViewModel model)
    {
        var window = new MainWindow();
        window.DataContext = model;
        if (window.FindName("RelativeMomentRow") is FrameworkElement row)
        {
            row.DataContext = model.Relative;
        }

        return StateSheet.Write(name, (FrameworkElement)window.Content);
    }

    /// <summary>MainWindow declares MinWidth 1000 and MinHeight 360.</summary>
    private const int MinimumWidth = 1000;

    /// <summary>See <see cref="MinimumWidth"/>.</summary>
    private const int MinimumHeight = 360;

    /// <summary>AboutWidth from Themes/Values.xaml, with room to show the whole window.</summary>
    private const int AboutWidth = 520;

    /// <summary>See <see cref="AboutWidth"/>.</summary>
    private const int AboutHeight = 720;
}

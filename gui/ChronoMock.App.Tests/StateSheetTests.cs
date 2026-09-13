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
    /// <summary>
    /// The component catalogue, which is where the parts library is looked at.
    /// </summary>
    /// <remarks>
    /// It renders taller than any window, on purpose: it is a sheet to read, not a screen to fit.
    ///
    /// 🔴 The height is a fixed number, and the test asserts the catalogue FITS in it. Without that
    /// assertion, adding a part would quietly push the newest one out of frame - and the newest part is
    /// exactly the one somebody wanted to look at. When this reddens, raise the number rather than crop.
    /// </remarks>
    [Fact]
    public void The_component_catalogue_renders_and_fits_in_the_sheet()
    {
        var (elements, needed) = WpfTestHost.InvokeSettled(() =>
        {
            var catalogue = new ComponentCatalogue();
            catalogue.Measure(new Size(CatalogueWidth, double.PositiveInfinity));
            var wanted = catalogue.DesiredSize.Height;
            return (StateSheet.Write("catalogue", catalogue, CatalogueWidth, CatalogueHeight), wanted);
        });

        Assert.NotEmpty(elements);
        Assert.True(
            needed <= CatalogueHeight,
            $"the catalogue needs {needed:F0} px and the sheet is {CatalogueHeight} px - "
                + "raise CatalogueHeight so the part you just added is actually in the picture");
    }

    private const int CatalogueWidth = 900;

    /// <summary>
    /// Tall enough for every part in every state it can be shown in. A canvas is not a ratchet: it exists
    /// to hold the picture, and a figure sitting exactly on today's content would have to move for every
    /// part added after it.
    /// </summary>
    /// <remarks>
    /// 🔴 THE SCALE IS WRITTEN DOWN NOW, because "one step of the same scale" was not enough to act on -
    /// 4 096 is both a power of two and a multiple of 1 024, and the two readings part company above it.
    /// The grid is 1 024. The clock and the fact list took the content to 4 851 px, so this went to 5 120:
    /// one step, not the measurement. Doubling to 8 192 would have doubled the cost of every render of it
    /// for 269 px of content.
    /// </remarks>
    private const int CatalogueHeight = 5120;

    /// <summary>
    /// The rebuilt setup phase, in the three states that decide whether it works.
    /// </summary>
    /// <remarks>
    /// First contact is the one that matters: the shipped panel's measured fault was that twelve blocks
    /// looked equally required, so the state where nothing has been chosen yet is the state the rework is
    /// answering. The other two exist because a filter that finds nothing and a session that is fully
    /// configured are the two ends this screen has to hold without moving anything under them.
    ///
    /// The view is not wired to the window yet and MainWindow is untouched, which is what keeps the 22
    /// product renders identical while this one is designed.
    /// </remarks>
    [Fact]
    public void The_setup_phase_renders_in_the_states_that_decide_whether_it_works()
    {
        var written = WpfTestHost.InvokeSettled(() =>
        {
            var total = 0;

            total += RenderSetup("setup-startup", PhaseStates.SetupStartup()).Count;

            // The catalogue open, which is the only render showing the well, its rows and the sentence
            // saying what choosing from it will do.
            total += RenderSetup("setup-scenarios", PhaseStates.SetupStartup(), "ScenarioSection").Count;

            total += RenderSetup("setup-no-matches", PhaseStates.SetupSearchingForNothing(), "ScenarioSection").Count;

            // 🔴 The merged group open, with everything in it turned on. The speed section absorbed the
            // launch fields when five groups would not fit the window, and without this render the merge
            // is a thing nobody looked at - the two halves meeting, the chips in the header, and the label
            // column agreeing across a boundary that used to be two separate scopes.
            total += RenderSetup("setup-options", PhaseStates.SetupWithEveryOption(), "SpeedSection").Count;

            // 🔴 AN APPLICATION CHOSEN AND A DATE THAT DOES NOT PARSE, which is the state the footer used
            // to meet in silence: the contract line has no moment to print, Start is disabled, and the
            // only sentence the footer knew was "choose an application" - which had been done. Nothing in
            // the sheet had ever rendered a refusal other than the first one.
            total += RenderSetup("setup-bad-date", PhaseStates.SetupWithBadDate()).Count;

            total += RenderSetup("setup-configured", PhaseStates.SetupConfigured()).Count;

            // 🔴 The window's own floor, which is the whole reason the footer is pinned. At 360 px the
            // form is far taller than the frame, so this is the sheet that shows whether the action and
            // the sentence explaining it survived - or whether they went below the fold with everything
            // else, which is what they used to do.
            total += StateSheet
                .Write("setup-floor", new SetupPhaseView { DataContext = PhaseStates.SetupStartup() }, MinimumWidth, MinimumHeight)
                .Count;

            return total;
        });

        Assert.True(written > 0);
    }

    /// <summary>
    /// The rebuilt session phase, in the states that decide whether it works.
    /// </summary>
    /// <remarks>
    /// The FIXTURES ARE THE PANEL'S OWN - SessionStates drives the same model into the same shapes the
    /// shipped renders use. That is the point: the two screens are then pictures of one state, and any
    /// difference between them is a difference in the drawing rather than in the data.
    ///
    /// The audit state is rendered with its section OPEN and scrolled to, because a folded section's
    /// contents are the half of this screen that carries untouchable rule 4 - the lists that say what was
    /// covered and, more importantly, what was not.
    /// </remarks>
    [Fact]
    public void The_session_phase_renders_in_the_states_that_decide_whether_it_works()
    {
        var written = WpfTestHost.InvokeSettled(() =>
        {
            var total = 0;

            total += RenderSession("session-running", SessionStates.Running()).Count;
            total += RenderSession(
                "session-audit",
                SessionStates.RunningWithCoverageWarnings(),
                "AuditSection").Count;
            total += RenderSession("session-error", SessionStates.InFlightError()).Count;
            total += RenderSession("session-ended", SessionStates.Ended()).Count;
            // A session over before any report: no controls, and a sentence saying the report will not come.
            total += RenderSession("session-vanished", SessionStates.TargetVanished()).Count;

            // The window's floor, where the clocks and the action have to survive together. Its model
            // goes through the same helper, so it carries a target like every other session state.
            total += StateSheet.Write(
                "session-floor",
                new SessionPhaseView { DataContext = PhaseStates.WithTarget(SessionStates.Running()) },
                MinimumWidth,
                MinimumHeight).Count;

            return total;
        });

        Assert.True(written > 0);
    }

    private static IReadOnlyList<LaidOutElement> RenderSession(
        string name,
        SessionViewModel model,
        string? openSection = null)
    {
        // A session has an application and the shared fixtures do not set one - PhaseStates.WithTarget says
        // why it is added there rather than in SessionStates.
        var view = new SessionPhaseView { DataContext = PhaseStates.WithTarget(model) };

        if (openSection is not null
            && view.FindName(openSection) is System.Windows.Controls.Expander section)
        {
            section.IsExpanded = true;
            LayoutProbe.Settle(view);
            if (view.FindName("FormScroll") is System.Windows.Controls.ScrollViewer scroll)
            {
                scroll.ScrollToEnd();
                LayoutProbe.Settle(view);
            }
        }

        return StateSheet.Write(name, view);
    }

    /// <summary>
    /// The phase at the window's REAL size, which is the only size worth judging it at.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS USED TO RENDER AT 1 000 px, 200 TALLER THAN THE WINDOW, on the reasoning that a shorter
    /// sheet would crop the contract sentence and the button. That reasoning was wrong about its own
    /// screen: the footer is PINNED, in its own grid row outside the scroll area, so it cannot go below
    /// any fold - being pinned is the whole point of it. What the extra 200 px actually did was hide how
    /// much of the form is below the fold at the size a user gets, and every judgement made on those
    /// renders was made on a window nobody has.
    ///
    /// The size comes from LayoutProbe rather than a second copy of the numbers, so the sheet cannot
    /// drift away from MainWindow's declared size the way this constant did.
    /// </remarks>
    private static IReadOnlyList<LaidOutElement> RenderSetup(
        string name,
        SessionViewModel model,
        string? openSection = null)
    {
        var view = new SetupPhaseView { DataContext = model };

        // 🔴 A FOLDED SECTION'S CONTENTS ARE A STATE, and this is the only way to reach it from here. The
        // catalogue moved into a section that starts closed, and for one render that turned the empty-list
        // state into a copy of the startup one - the sheet kept writing a file and stopped showing the
        // thing the file was for. Opening it is a user action, so it belongs to the sheet and not to a
        // flag on the model.
        // Fully qualified: Wpf.Ui.Controls has an Expander too, and the one in the view is the stock WPF
        // control (the toolkit's does not appear anywhere in this project).
        if (openSection is not null
            && view.FindName(openSection) is System.Windows.Controls.Expander section)
        {
            section.IsExpanded = true;

            // 🔴 AND SCROLLED TO IT, because opening it is not enough. The window is 800 px and the two
            // groups above the catalogue come to 446, so an opened 240 px well starts below the fold: the
            // first attempt at this render wrote a picture of a search box with an empty box under it and
            // the sentence explaining the emptiness out of frame. A sheet that writes a file showing
            // nothing is worse than no sheet. This is the screen as somebody who opened the section and
            // scrolled down sees it.
            LayoutProbe.Settle(view);
            if (view.FindName("FormScroll") is System.Windows.Controls.ScrollViewer scroll)
            {
                scroll.ScrollToEnd();
                LayoutProbe.Settle(view);
            }
        }

        return StateSheet.Write(name, view);
    }

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

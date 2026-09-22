using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using System.Windows.Controls;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The window's own four dimensions, and the one relationship between them that used to live only in
/// prose: the minimum width is derived from the calculator's three columns.
/// </summary>
/// <remarks>
/// 🔴 THE NUMBERS WERE IN THREE PLACES AND TIED IN NONE. `MainWindow.xaml` carried four raw literals
/// (GUI rules 21 and 22 say a dimension is a token like a colour is), `Values.xaml` carried the
/// calculator's column floors with a comment saying "their sum with the card margins has to fit the
/// window's own MinWidth", and `LayoutProbe` carried a third copy of the default size described as "the
/// main window's own declared size". Three copies of one decision, and nothing that failed when they
/// disagreed - change a column floor and the window quietly stops fitting its own calculator, on a
/// screen nobody resizes in a test.
///
/// These are cheap assertions about resources rather than renders, and that is on purpose: the render
/// side of the same claim lives in <c>LayoutGuardTests</c>, which squeezes the calculator to the
/// window's floor and measures the columns. This class asserts the arithmetic, that one the picture.
/// </remarks>
public class WindowSizeTests
{
    private static double Token(string key)
        => WpfTestHost.InvokeSettled(() => (double)Application.Current.Resources[key]);

    [Fact]
    public void The_window_takes_its_four_dimensions_from_the_tokens()
    {
        // What this proves is that the reference RESOLVES - a StaticResource that does not throws at
        // construction rather than at compile time, so it has to be measured on a real window. What it
        // does NOT prove is that the markup uses the token at all: a literal 1040 written back would
        // equal the token and pass here. The guard for that is the one below, which reads the markup.
        var (width, height, minWidth, minHeight) = WpfTestHost.InvokeSettled(() =>
        {
            var w = new MainWindow();
            return (w.Width, w.Height, w.MinWidth, w.MinHeight);
        });

        Assert.Equal(Token("WindowDefaultWidth"), width);
        Assert.Equal(Token("WindowDefaultHeight"), height);
        Assert.Equal(Token("WindowMinWidth"), minWidth);
        Assert.Equal(Token("WindowMinHeight"), minHeight);
    }

    [Fact]
    public void The_minimum_width_holds_the_calculator_s_three_columns()
    {
        // The derivation the column comment describes, as arithmetic a build can check: the three floors
        // plus the grid's own margin on both sides. A column floor raised past this stops the build here
        // rather than on a user's screen.
        var scenarios = Token("CalcScenariosColumnMinWidth");
        var builder = Token("CalcBuilderColumnMinWidth");
        var result = Token("CalcResultColumnMinWidth");
        var gridMargin = WpfTestHost.InvokeSettled(() => (Thickness)Application.Current.Resources["SpaceMd"]);
        var needed = scenarios + builder + result + gridMargin.Left + gridMargin.Right;

        Assert.Equal(needed, Token("CalculatorMinWidth"));
        Assert.True(
            Token("WindowMinWidth") >= needed,
            $"the window's floor is {Token("WindowMinWidth"):F0} and its calculator needs {needed:F0}");
    }

    [Fact]
    public void The_window_opens_larger_than_its_own_floor()
    {
        // A window that opens AT its minimum opens at its worst: the minimum is the point below which
        // the layout stops being readable, not a size to greet anybody with (GUI rule 22).
        Assert.True(Token("WindowDefaultWidth") > Token("WindowMinWidth"), "the default width is not above the floor");
        Assert.True(Token("WindowDefaultHeight") > Token("WindowMinHeight"), "the default height is not above the floor");
    }

    [Fact]
    public void The_layout_probe_measures_at_the_size_the_window_actually_opens_at()
    {
        // The third copy. Every render-side guard in this project settles its view at LayoutProbe's size
        // and reads the result as "what a user gets" - so if that constant drifts from the window, every
        // one of those guards is measuring a window nobody has.
        Assert.Equal(LayoutProbe.WindowWidth, Token("WindowDefaultWidth"));
        Assert.Equal(LayoutProbe.WindowHeight, Token("WindowDefaultHeight"));
    }

    [Fact]
    public void The_calculator_does_not_overflow_the_window_s_floor()
    {
        // 🔴 THE PICTURE, and the SECOND draft of it. The first asked whether each column came out at or
        // above its floor, which WPF guarantees on its own: a star column with a MinWidth never goes
        // under it. The columns hold and the GRID overflows instead, off the right of the window, which
        // is exactly the failure and exactly what the first draft could not see - it passed with the
        // floor dropped to 900 (measured).
        //
        // So this measures the overflow: the three columns as laid out, against the width actually
        // available to them. Reversal probe: lower WindowMinWidth below CalculatorMinWidth and this
        // fails on the difference.
        var (needed, available) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new ChronoMock.App.Views.CalculatorView();
            var min = (double)Application.Current.Resources["WindowMinWidth"];
            LayoutProbe.Settle(view, (int)min, LayoutProbe.WindowHeight);
            var grid = FindCalculatorGrid(view);
            if (grid is null)
            {
                return (0d, 0d);
            }

            // The grid against the VIEW that holds it, not against itself. Measured at a 900 px floor:
            // the view came out 900 and the grid 948 - the grid is what overflows, and comparing the
            // columns to their own grid compares 948 with 948 and always passes.
            return (grid.ActualWidth + grid.Margin.Left + grid.Margin.Right, view.ActualWidth);
        });

        Assert.True(available > 0, "the calculator's three-column grid was not found in the render");
        Assert.True(
            needed <= available + 0.5,
            $"the calculator lays out to {needed:F0} px inside a window floor of {available:F0}");
    }

    [Fact]
    public void The_window_markup_carries_no_raw_dimension()
    {
        // 🔴 THE GUARD THE VALUE ASSERTIONS CANNOT BE. Every test above compares numbers, and a literal
        // written back into the markup has the same number as the token it replaced - it would pass all
        // of them, silently, which is how four literals survived in this file until 2026-09-22. So this
        // one reads the markup and asks for the SHAPE: each of the four dimensions is a resource
        // reference (GUI rules 21 and 22).
        //
        // XamlLiteralGuard does not cover this file - it reads the parts library, where a raw number is
        // always wrong, and a view legitimately carries numbers like Grid.Row. This is the narrow case
        // where the meaning is a dimension and the answer is always a token.
        var path = Path.Combine(TestPaths.RepoRoot(), "gui", "ChronoMock.App", "MainWindow.xaml");
        // Named rather than assumed: a guard that reads a file by path goes quiet when the file moves,
        // and quiet is indistinguishable from passing.
        Assert.True(File.Exists(path), $"the window markup is not where this guard looks: {path}");
        var markup = File.ReadAllText(path);

        foreach (var attribute in new[] { "Width", "Height", "MinWidth", "MinHeight" })
        {
            var match = System.Text.RegularExpressions.Regex.Match(
                markup, $@"\s{attribute}=""([^""]*)""");
            Assert.True(match.Success, $"the window does not declare {attribute} at all");
            Assert.True(
                match.Groups[1].Value.StartsWith("{StaticResource", StringComparison.Ordinal),
                $"{attribute} is the literal {match.Groups[1].Value} rather than a token");
        }
    }

    /// <summary>The three-column grid that holds the calculator, found by its shape: three columns, each
    /// with a floor. Found rather than named, because naming it would be a fourth copy of the same
    /// decision.</summary>
    private static Grid? FindCalculatorGrid(DependencyObject root)
        => Descendants(root)
            .OfType<Grid>()
            .FirstOrDefault(g => g.ColumnDefinitions.Count == 3 && g.ColumnDefinitions.All(c => c.MinWidth > 0));

    private static IEnumerable<DependencyObject> Descendants(DependencyObject root)
    {
        var count = System.Windows.Media.VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < count; i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            yield return child;
            foreach (var deeper in Descendants(child))
            {
                yield return deeper;
            }
        }
    }
}

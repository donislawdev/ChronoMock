using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace ChronoMock.App.Tests;

/// <summary>
/// The shared list well (the implicit ListBox style in Parts.xaml, worn by the scenario list and the
/// calculator's preset list) has to do two things at once, and each was lost while fixing the other:
/// build only the rows in view, and scroll to a bottom that is flush with the last row.
///
/// Both are measured on a LAID-OUT list carrying the real ScenarioRow template with rows of differing
/// height, because both faults are invisible in the declaration. Item scrolling, the toolkit default,
/// virtualises and leaves a 68 px strip of empty dark under the last row. Physical scrolling
/// (CanContentScroll=False, the first fix) closes the strip and silently builds every container, since a
/// panel that is not the scrolling panel does not virtualise. Only ScrollUnit=Pixel does both, and this
/// class goes red on either regression: revert the pixel unit and the tail comes back, set physical
/// scrolling and the container count does.
///
/// What this does NOT prove: that a particular VIEW still wears the part unchanged. A view can set
/// CanContentScroll on its own ListBox and the bare list here would not notice, which is why the third
/// test reads the view sources for that one attribute - a declaration check, admitted as such, standing
/// in for a render of every list with 200 rows in it.
/// </summary>
public class ListWellScrollTests
{
    private sealed record Row(string DisplayName, string DisplayExplains);

    // Enough rows that a list which builds them all is unmistakable, and enough variation in the
    // second line (one to four sentences) that item scrolling cannot land the last row by luck.
    private const int RowCount = 200;

    // A well of the part's own height sits inside this host with room to spare, so the list, not the
    // host, is what bounds the rows.
    private const int HostWidth = 400;
    private const int HostHeight = 300;

    // A virtualised well of 240 px holds a handful of rows plus the panel's own overscan. Forty is far
    // above any honest count and far below the two hundred a non-virtualising list builds.
    private const int RealisedCeiling = 40;

    private const double FlushTolerance = 0.5;

    [Fact]
    public void The_list_well_builds_only_the_rows_in_view()
    {
        var (realised, height) = WpfTestHost.InvokeSettled(() =>
        {
            var list = ScenarioList();
            Settle(list);
            return (Realised(list), list.ActualHeight);
        });

        Assert.True(height > 0, "the list arranged to nothing, so the container count below is meaningless");
        Assert.True(realised > 0, "no container was built at all - the generator did not run");
        Assert.True(
            realised <= RealisedCeiling,
            $"the list well built {realised} of {RowCount} rows for a {height} px well - virtualisation is off");
    }

    [Fact]
    public void Scrolling_to_the_end_leaves_no_empty_strip_under_the_last_row()
    {
        var (tail, lastBuilt) = WpfTestHost.InvokeSettled(() =>
        {
            var list = ScenarioList();
            Settle(list);

            var viewer = FindChild<ScrollViewer>(list)
                ?? throw new InvalidOperationException("the list well template has no ScrollViewer");
            viewer.ScrollToEnd();
            list.UpdateLayout();

            var presenter = FindChild<ScrollContentPresenter>(viewer)
                ?? throw new InvalidOperationException("the list well's ScrollViewer has no content presenter");
            if (list.ItemContainerGenerator.ContainerFromIndex(RowCount - 1) is not FrameworkElement last)
            {
                return (double.NaN, false);
            }

            double lastBottom = last.TransformToAncestor(presenter).Transform(new Point(0, 0)).Y + last.ActualHeight;
            return (presenter.ActualHeight - lastBottom, true);
        });

        Assert.True(lastBuilt, "after scrolling to the end the last row was not built, so it cannot be in view");
        Assert.True(
            Math.Abs(tail) <= FlushTolerance,
            $"scrolled to the end, the last row's bottom sits {tail:F1} px above the bottom of the well");
    }

    [Fact]
    public void No_view_overrides_the_scrolling_of_a_list_well()
    {
        var appDirectory = TestPaths.AppDirectory();
        var offenders = new List<string>();
        foreach (var folder in new[] { "Views", "Controls" })
        {
            foreach (var file in Directory.EnumerateFiles(Path.Combine(appDirectory, folder), "*.xaml"))
            {
                var lines = File.ReadAllLines(file);
                for (int i = 0; i < lines.Length; i++)
                {
                    if (lines[i].Contains("CanContentScroll", StringComparison.Ordinal))
                    {
                        offenders.Add($"{folder}/{Path.GetFileName(file)}:{i + 1}");
                    }
                }
            }
        }

        Assert.True(
            offenders.Count == 0,
            "a view sets CanContentScroll on a list, which takes the list well off its virtualising panel: "
                + string.Join(", ", offenders));
    }

    private static ListBox ScenarioList()
    {
        var rows = Enumerable.Range(0, RowCount)
            .Select(i => new Row(
                $"Scenario {i}",
                string.Concat(Enumerable.Repeat("An explanation long enough to wrap onto a second line. ", 1 + i % 4))))
            .ToList();

        return new ListBox
        {
            ItemsSource = rows,
            ItemTemplate = (DataTemplate)Application.Current.FindResource("ScenarioRow"),
            ItemContainerStyle = (Style)Application.Current.FindResource("ScenarioRowContainer"),
            Width = HostWidth,
        };
    }

    private static void Settle(ListBox list)
    {
        var host = new Grid { Width = HostWidth, Height = HostHeight };
        host.Children.Add(list);
        LayoutProbe.Settle(host, HostWidth, HostHeight);
    }

    private static int Realised(ListBox list)
    {
        int count = 0;
        for (int i = 0; i < RowCount; i++)
        {
            if (list.ItemContainerGenerator.ContainerFromIndex(i) is not null)
            {
                count++;
            }
        }

        return count;
    }

    private static T? FindChild<T>(DependencyObject root) where T : DependencyObject
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T hit)
            {
                return hit;
            }

            if (FindChild<T>(child) is { } deeper)
            {
                return deeper;
            }
        }

        return null;
    }
}

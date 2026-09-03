using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The recent-targets box renders two different rows from ONE template: the closed box shows the file name
/// alone - exactly the plain label it replaced - while the open list adds the directory that tells two
/// same-named builds apart (an x64 and an x86 output are both app.exe). The split is a template trigger on
/// "am I inside a ComboBoxItem", which no view-model test can reach: it is a rendering fact, so it is
/// asserted on a laid-out window rather than trusted.
/// </summary>
public class TargetBoxTests
{
    [Fact]
    public void The_closed_target_box_shows_the_file_name_without_its_directory()
    {
        var texts = WpfTestHost.InvokeSettled(() =>
        {
            var window = new MainWindow();

            // Lay out the window's CONTENT, not the window: an unshown Window has no HWND, so measuring it
            // never reaches through the panel's ScrollViewer template - every binding below stays
            // Unattached and this assertion would pass against an empty tree (measured, not assumed).
            var content = (FrameworkElement)window.Content;
            content.Measure(new Size(1600, 1400));
            content.Arrange(new Rect(0, 0, 1600, 1400));
            content.UpdateLayout();

            var box = (ComboBox)window.FindName("TargetBox");

            // A dev checkout preselects the bundled sample target, so the box always has a selection here.
            // Asserting it (rather than letting an empty box pass silently) keeps this from going vacuous.
            Assert.True(box.HasItems, "the sample target should have filled the recent list");
            return VisibleTexts(box).ToList();
        });

        Assert.Contains(texts, t => t.EndsWith(".exe", StringComparison.OrdinalIgnoreCase));
        Assert.DoesNotContain(texts, t => t.Contains('\\', StringComparison.Ordinal));
    }

    /// <summary>Every rendered TextBlock under <paramref name="root"/>, skipping collapsed subtrees. Reads
    /// Visibility rather than IsVisible: the window under test is laid out but never shown, so IsVisible is
    /// false for everything and would make the whole assertion vacuous.</summary>
    private static IEnumerable<string> VisibleTexts(DependencyObject root)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is UIElement { Visibility: not Visibility.Visible })
            {
                continue;
            }

            if (child is TextBlock text && !string.IsNullOrWhiteSpace(text.Text))
            {
                yield return text.Text;
            }

            foreach (var nested in VisibleTexts(child))
            {
                yield return nested;
            }
        }
    }
}

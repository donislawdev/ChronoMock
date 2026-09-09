using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// Both halves of the app stand on one background, and this proves it by RENDERING the window and
/// counting the colour that covers most of it - not by looking for an attribute.
///
/// The distinction is the whole point. Until this slice the window carried
/// Background="{DynamicResource BrushBgBase}" and painted the wpfui default instead, on every pixel, for
/// the entire life of the file. An attribute-presence guard would have been green throughout, and would
/// have gone on being green if someone moved the declaration back onto the window tomorrow. So the
/// assertion is on the paint.
///
/// What this does NOT prove: how it looks, where the seam was, or that the two views agree with each
/// other beyond this one colour. Those were measured on live windows by pixel, and belong in the
/// changelog.
/// </summary>
public class BackgroundGuardTests
{
    // The window's own declared size, so the render matches the layout a user gets rather than some
    // arbitrary test geometry.
    private const int RenderWidth = 1040;
    private const int RenderHeight = 800;

    // Every 4th pixel in each direction - a sixteenth of the surface, which is plenty to find a colour
    // that covers most of a window and keeps the render assertion off the slow list.
    private const int SampleStep = 4;

    [Fact]
    public void The_window_paints_the_palette_base_over_most_of_its_surface()
    {
        var (dominant, share, distinct) = WpfTestHost.InvokeSettled(() => Rendered(new MainWindow()));

        // Canary first: a blank render would hand the assertion below a single flat colour and pass for
        // the wrong reason. A window that actually drew its content has hundreds of colours in it.
        Assert.True(
            distinct > 20,
            $"the render produced only {distinct} distinct colours - it drew nothing, so the colour "
                + "assertion below would be meaningless");

        var expected = WpfTestHost.InvokeSettled(
            () => (Application.Current.TryFindResource("BrushBgBase") as SolidColorBrush)?.Color);

        Assert.True(expected is not null, "BrushBgBase is missing from the palette");
        Assert.Equal(expected!.Value, dominant);
        Assert.True(
            share > 0.5,
            $"the base colour covers only {share:P1} of the window - something else is painting the "
                + "background, or the content root lost its Background");
    }

    [Fact]
    public void No_view_writes_a_background_onto_a_FluentWindow()
    {
        var offenders = WindowBackgroundGuard.Scan(TestPaths.AppDirectory());

        Assert.True(
            offenders.Count == 0,
            "a Background on a ui:FluentWindow paints nothing - move it to the content root inside:\n"
                + string.Join("\n", offenders.Select(o => $"  {o.File}  Background=\"{o.Value}\"")));
    }

    [Fact]
    public void The_scan_actually_reaches_the_views_it_claims_to_cover()
    {
        // The canary for the scan: a path that resolves to nothing makes the test above pass forever.
        // Every window of this app is a FluentWindow root, so anything under the count means the walk
        // broke. Raised from two to three when the About window arrived - a floor left below the real
        // number is a canary that stops noticing the newest window, which is the one most likely to carry
        // the dead attribute this guard exists for.
        var (views, windows) = WindowBackgroundGuard.Coverage(TestPaths.AppDirectory());

        Assert.True(views >= 5, $"the guard walked only {views} view files - the app directory is wrong");
        Assert.True(windows >= 3, $"the guard found only {windows} FluentWindow roots, expected at least 3");
    }

    [Fact]
    public void Guard_reddens_on_a_background_written_on_a_FluentWindow()
        => Assert.NotEmpty(WindowBackgroundGuard.Inspect(
            "x.xaml",
            """
            <ui:FluentWindow xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                             xmlns:ui="http://schemas.lepo.co/wpfui/2022/xaml"
                             Background="{DynamicResource BrushBgBase}">
              <Grid />
            </ui:FluentWindow>
            """));

    [Fact]
    public void Guard_allows_a_background_on_the_content_root()
        => Assert.Empty(WindowBackgroundGuard.Inspect(
            "x.xaml",
            """
            <ui:FluentWindow xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                             xmlns:ui="http://schemas.lepo.co/wpfui/2022/xaml">
              <Grid Background="{DynamicResource BrushBgBase}" />
            </ui:FluentWindow>
            """));

    [Fact]
    public void Guard_leaves_a_plain_Window_alone()
    {
        // A stock Window honours its own Background, so the attribute is not dead there and the guard
        // has no business objecting to it.
        Assert.Empty(WindowBackgroundGuard.Inspect(
            "x.xaml",
            """
            <Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                    Background="{DynamicResource BrushBgBase}" />
            """));
    }

    /// <summary>Render the window's content root and report the colour covering most of it, the share it
    /// covers, and how many distinct colours the render contains.</summary>
    private static (Color Dominant, double Share, int Distinct) Rendered(Window window)
    {
        // 🔴 An unshown Window has no HWND, so what is rendered is the CONTENT, laid out by hand. Without
        // the measure and arrange pass the visual tree has zero size and the bitmap comes back empty -
        // which the canary in the test above is there to catch.
        var root = (FrameworkElement)window.Content;
        root.Measure(new Size(RenderWidth, RenderHeight));
        root.Arrange(new Rect(0, 0, RenderWidth, RenderHeight));
        root.UpdateLayout();

        var bitmap = new RenderTargetBitmap(RenderWidth, RenderHeight, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(root);

        int stride = RenderWidth * 4;
        var pixels = new byte[stride * RenderHeight];
        bitmap.CopyPixels(pixels, stride, 0);

        var counts = new Dictionary<uint, int>();
        int sampled = 0;
        for (int y = 0; y < RenderHeight; y += SampleStep)
        {
            for (int x = 0; x < RenderWidth; x += SampleStep)
            {
                int i = (y * stride) + (x * 4);
                uint key = ((uint)pixels[i + 3] << 24) | ((uint)pixels[i + 2] << 16)
                    | ((uint)pixels[i + 1] << 8) | pixels[i];
                counts[key] = counts.TryGetValue(key, out int n) ? n + 1 : 1;
                sampled++;
            }
        }

        var top = counts.MaxBy(pair => pair.Value);
        var colour = Color.FromArgb(
            (byte)(top.Key >> 24), (byte)(top.Key >> 16), (byte)(top.Key >> 8), (byte)top.Key);
        return (colour, (double)top.Value / sampled, counts.Count);
    }
}

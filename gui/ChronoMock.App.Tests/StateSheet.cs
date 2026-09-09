using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace ChronoMock.App.Tests;

/// <summary>
/// Writes one laid-out view to a PNG and a text dump, so a screen can be LOOKED at without running the
/// app, and so every state of it can be looked at rather than only the one a live run happened to reach.
/// </summary>
/// <remarks>
/// The interface has some seventy regions switched by a binding - Visibility and IsEnabled on the panel
/// and the calculator together. Reaching one of them through a live window means driving a real session,
/// so in practice only the empty startup state ever gets seen, and everything else is judged from source.
/// Here a state is a view model in a given shape, which costs nothing and covers all of them.
///
/// 🔴 THIS IS A LAYOUT INSTRUMENT, NOT A FIDELITY ONE. It renders the CONTENT root at 96 DPI with no
/// HWND, so there is no window chrome, no title bar, no backdrop, no shadow and no display scaling. What
/// it shows is the arrangement and the paint inside the frame. How the finished window looks still has to
/// be seen on a real one, and that is what tools/gui/ChronoUia.psm1 is for.
/// </remarks>
internal static class StateSheet
{
    /// <summary>
    /// Where the sheet lands: tools/gui/renders, which is outside git and beside the scripts that read
    /// it. CHRONO_GUI_SHEET is honoured for a one-off run elsewhere.
    /// </summary>
    /// <remarks>
    /// 🔴 NOT Path.GetTempPath(), and that is a measurement rather than a preference. On this machine
    /// TMP points at C:\WINDOWS\TEMP while TEMP points at the user profile, and .NET reads TMP first
    /// while PowerShell's $env:TEMP reads the other one. The sheet therefore landed in two different
    /// directories depending on which shell started it, and the reader was shown a stale one while a
    /// fresh one sat elsewhere - a tool quietly answering about the wrong run.
    /// </remarks>
    public static string OutputDirectory =>
        Environment.GetEnvironmentVariable("CHRONO_GUI_SHEET") is { Length: > 0 } chosen
            ? chosen
            : Path.Combine(TestPaths.RepoRoot(), "tools", "gui", "renders");

    /// <summary>
    /// Lay the root out, render it, and write both halves of the evidence beside each other.
    /// </summary>
    /// <returns>The elements found, so the caller can assert on the same walk that produced the picture.</returns>
    public static IReadOnlyList<LaidOutElement> Write(
        string name,
        FrameworkElement root,
        int width = LayoutProbe.WindowWidth,
        int height = LayoutProbe.WindowHeight)
    {
        LayoutProbe.Settle(root, width, height);
        var elements = LayoutProbe.Walk(root);

        Directory.CreateDirectory(OutputDirectory);
        SavePng(root, width, height, Path.Combine(OutputDirectory, name + ".png"));
        File.WriteAllText(Path.Combine(OutputDirectory, name + ".txt"), LayoutReport.Describe(elements));
        File.WriteAllLines(Path.Combine(OutputDirectory, name + ".names.tsv"), NamedRows(elements));
        return elements;
    }

    /// <summary>
    /// The named elements as tab separated rows, for the calibration script to line up against the same
    /// elements in a LIVE window. Only named ones, because an unnamed template part has no address that
    /// survives the trip through the accessibility tree.
    /// </summary>
    private static IEnumerable<string> NamedRows(IReadOnlyList<LaidOutElement> elements) => elements
        .Where(e => e.Name.Length > 0 && e.IsVisible)
        .Select(e => string.Create(
            CultureInfo.InvariantCulture,
            $"{e.Name}\t{e.Bounds.X:F1}\t{e.Bounds.Y:F1}\t{e.Bounds.Width:F1}\t{e.Bounds.Height:F1}"));

    private static void SavePng(FrameworkElement root, int width, int height, string path)
    {
        var bitmap = new RenderTargetBitmap(width, height, Dpi, Dpi, PixelFormats.Pbgra32);
        bitmap.Render(root);

        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var file = File.Create(path);
        encoder.Save(file);
    }

    /// <summary>96 keeps one rendered pixel equal to one WPF unit, so a measured number matches the XAML.</summary>
    private const int Dpi = 96;
}

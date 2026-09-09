using System.Globalization;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace ChronoMock.App.Tests;

/// <summary>
/// Reads every piece of text off a RENDERED screen and measures it against the surface it was actually
/// painted on: contrast ratio, and whether its size comes from the declared type scale.
/// </summary>
/// <remarks>
/// 🔴 WHY THE PAINT AND NOT THE XAML. The controls in this interface are semi transparent - a wpfui
/// button, a card, a chip all let what is behind them through and settle on a colour that appears in no
/// declaration. A brush key resolved from the palette therefore says nothing about what a reader ends up
/// looking at. The only place the answer exists is the rendered pixel, which is why this samples one.
///
/// A dark interface makes this worse rather than better: the palette is a four rung ladder of near
/// greys, so a caption that lands one rung off is still plausible in a screenshot and unreadable in a
/// lit room.
///
/// What is NOT claimed: this is not an accessibility audit. It measures the two things that are
/// objective - a ratio against a threshold, and membership of a closed set of sizes. Whether a screen
/// reads WELL stays a judgement made on the sheet.
/// </remarks>
internal static class ContrastReport
{
    /// <summary>The type sizes declared in Themes/Values.xaml. Anything else is a size nobody chose.</summary>
    private static readonly double[] DeclaredSizes = [12, 14, 18, 24, 34];

    /// <summary>WCAG 2.1 AA for body text.</summary>
    private const double NormalTextRatio = 4.5;

    /// <summary>WCAG 2.1 AA for large text. 24 px is the conservative end of the large-text rule.</summary>
    private const double LargeTextRatio = 3.0;

    /// <summary>See <see cref="LargeTextRatio"/>.</summary>
    private const double LargeTextSize = 24;

    /// <summary>
    /// One measured line of text: what it says, how big, on what, and at what ratio.
    /// </summary>
    internal sealed record Reading
    {
        public required string Label { get; init; }
        public required string Text { get; init; }
        public required double FontSize { get; init; }
        public required double Ratio { get; init; }
        public required bool BackgroundKnown { get; init; }
        public required string Ink { get; init; }
        public required string Paper { get; init; }

        /// <summary>What this size has to reach, given how large it is.</summary>
        public double Required => FontSize >= LargeTextSize ? LargeTextRatio : NormalTextRatio;

        /// <summary>Too faint to read at this size, on the surface it was actually painted on.</summary>
        public bool TooFaint => BackgroundKnown && Ratio < Required;

        /// <summary>
        /// An icon glyph rather than words: one character from the Unicode private use area, which is
        /// where Segoe Fluent Icons lives.
        /// </summary>
        /// <remarks>
        /// 🔴 Measured before this existed: all fifteen off-scale readings across both screens were
        /// unnamed TextBlocks with empty-looking text at 11 px and 16 px - chevrons, checkmarks and
        /// window buttons drawn by wpfui control templates at sizes of their own. A type-scale gate
        /// without this filter reddens on a dependency, which is a gate nobody can act on.
        /// </remarks>
        public bool IsGlyph => Text.Length == 1 && Text[0] >= '\uE000' && Text[0] <= '\uF8FF';

        /// <summary>A size that is not on the declared scale, for text that is actually words.</summary>
        public bool OffScale => !IsGlyph && !DeclaredSizes.Contains(FontSize);
    }

    /// <summary>
    /// Measure every visible piece of text on a laid-out screen.
    /// </summary>
    public static IReadOnlyList<Reading> Measure(
        FrameworkElement root, IReadOnlyList<LaidOutElement> elements, int width, int height)
    {
        var bitmap = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(root);

        int stride = width * 4;
        var pixels = new byte[stride * height];
        bitmap.CopyPixels(pixels, stride, 0);

        var readings = new List<Reading>();
        foreach (var element in elements)
        {
            if (!element.IsVisible || element.Text.Length == 0 || element.Foreground is not { } ink)
            {
                continue;
            }

            var paper = DominantColour(pixels, stride, width, height, element.Bounds);
            readings.Add(Describe(element, ink, paper));
        }

        return readings;
    }

    private static Reading Describe(LaidOutElement element, Color ink, Color? paper) => new()
    {
        Label = element.Label,
        Text = element.Text.Length > 40 ? element.Text[..40] + "..." : element.Text,
        FontSize = element.FontSize,
        Ratio = paper is { } known ? Math.Round(Contrast(ink, known), 2) : 0,
        // 🔴 A text box whose rectangle is mostly ink has no readable background in it, and guessing one
        // would invent a finding. Reported as unknown rather than as a failure.
        BackgroundKnown = paper is { } surface && Distance(ink, surface) > InkConfusionDistance,
        Ink = Hex(ink),
        Paper = paper is { } shown ? Hex(shown) : "?",
    };

    /// <summary>How close a sampled background may get to the ink before we stop trusting it.</summary>
    private const double InkConfusionDistance = 32;

    /// <summary>
    /// The colour covering most of a rectangle, which for a line of text is the surface behind it.
    /// Buckets of eight per channel, so antialiasing does not scatter one fill across a dozen shades.
    /// </summary>
    private static Color? DominantColour(byte[] pixels, int stride, int width, int height, Rect bounds)
    {
        int x0 = Math.Max(0, (int)bounds.X);
        int y0 = Math.Max(0, (int)bounds.Y);
        int x1 = Math.Min(width, (int)Math.Ceiling(bounds.Right));
        int y1 = Math.Min(height, (int)Math.Ceiling(bounds.Bottom));
        if (x1 <= x0 || y1 <= y0)
        {
            return null;
        }

        var counts = new Dictionary<int, int>();
        for (int y = y0; y < y1; y++)
        {
            for (int x = x0; x < x1; x++)
            {
                int i = (y * stride) + (x * 4);
                int key = ((pixels[i + 2] / 8) << 16) | ((pixels[i + 1] / 8) << 8) | (pixels[i] / 8);
                counts[key] = counts.TryGetValue(key, out int n) ? n + 1 : 1;
            }
        }

        if (counts.Count == 0)
        {
            return null;
        }

        int top = counts.MaxBy(pair => pair.Value).Key;
        return Color.FromRgb((byte)((top >> 16) * 8), (byte)(((top >> 8) & 0xFF) * 8), (byte)((top & 0xFF) * 8));
    }

    /// <summary>WCAG 2.1 contrast ratio between two colours.</summary>
    private static double Contrast(Color a, Color b)
    {
        double la = Luminance(a);
        double lb = Luminance(b);
        return (Math.Max(la, lb) + 0.05) / (Math.Min(la, lb) + 0.05);
    }

    private static double Luminance(Color c)
        => (0.2126 * Channel(c.R)) + (0.7152 * Channel(c.G)) + (0.0722 * Channel(c.B));

    private static double Channel(byte value)
    {
        double c = value / 255.0;
        return c <= 0.03928 ? c / 12.92 : Math.Pow((c + 0.055) / 1.055, 2.4);
    }

    private static double Distance(Color a, Color b)
        => Math.Sqrt(Math.Pow(a.R - b.R, 2) + Math.Pow(a.G - b.G, 2) + Math.Pow(a.B - b.B, 2));

    private static string Hex(Color c) => string.Create(
        CultureInfo.InvariantCulture, $"#{c.R:X2}{c.G:X2}{c.B:X2}");

    /// <summary>The readings as text, worst first, for the sheet and for a failure message.</summary>
    public static string Describe(IReadOnlyList<Reading> readings)
    {
        var lines = readings
            .Where(r => r.BackgroundKnown)
            .OrderBy(r => r.Ratio)
            .Select(r => string.Create(
                CultureInfo.InvariantCulture,
                $"{r.Ratio,6:N2} (needs {r.Required,4:N1})  {r.FontSize,4:N0}px  {r.Ink} on {r.Paper}  {r.Label}  {r.Text}"));

        // An off-scale size disappears among the contrast lines, so it gets a section of its own.
        var offScale = readings.Where(r => r.OffScale).Select(r => string.Create(
            CultureInfo.InvariantCulture, $"  {r.FontSize,4:N0}px  {r.Label}  {r.Text}")).ToList();

        int unknown = readings.Count(r => !r.BackgroundKnown);
        return string.Join('\n', lines)
            + string.Create(CultureInfo.InvariantCulture,
                $"\n\nOFF THE TYPE SCALE ({offScale.Count}):\n")
            + string.Join('\n', offScale)
            + string.Create(CultureInfo.InvariantCulture, $"\n\n{readings.Count} readings, {unknown} with no readable background");
    }
}

using System.Globalization;
using System.Text;

namespace ChronoMock.App.Tests;

/// <summary>
/// Renders a layout walk as text a person can read, for the state sheet and for failure messages.
/// </summary>
/// <remarks>
/// A rule that fails with only a count sends the reader back to reproduce the run before they can act.
/// The same lines that make the sheet readable make the failure actionable, so they live in one place.
/// </remarks>
internal static class LayoutReport
{
    /// <summary>One line per element: indent for depth, then geometry, then whatever it shows.</summary>
    public static string Describe(IReadOnlyList<LaidOutElement> elements)
    {
        var text = new StringBuilder();
        foreach (var element in elements)
        {
            text.Append(' ', Math.Min(element.Depth, MaxIndent) * 2);
            text.Append(CultureInfo.InvariantCulture, $"{element.Label} ");
            text.Append(CultureInfo.InvariantCulture, $"[{Round(element.Bounds.X)},{Round(element.Bounds.Y)} ");
            text.Append(CultureInfo.InvariantCulture, $"{Round(element.Bounds.Width)}x{Round(element.Bounds.Height)}]");
            if (!element.IsVisible)
            {
                text.Append(" hidden");
            }

            if (!element.IsEnabled)
            {
                text.Append(" disabled");
            }

            if (element.Text.Length > 0)
            {
                text.Append(CultureInfo.InvariantCulture, $" \"{Shorten(element.Text)}\"");
            }

            text.Append('\n');
        }

        return text.ToString();
    }

    /// <summary>A short line naming one element and where it sits, for a rule's failure message.</summary>
    public static string Locate(LaidOutElement element) => string.Create(
        CultureInfo.InvariantCulture,
        $"{element.Label} at [{Round(element.Bounds.X)},{Round(element.Bounds.Y)} {Round(element.Bounds.Width)}x{Round(element.Bounds.Height)}]");

    private const int MaxIndent = 12;
    private const int MaxTextLength = 60;

    private static int Round(double value) => (int)Math.Round(value, MidpointRounding.AwayFromZero);

    private static string Shorten(string text)
    {
        var flat = text.ReplaceLineEndings(" ");
        return flat.Length <= MaxTextLength ? flat : flat[..MaxTextLength] + "...";
    }
}

using System.Globalization;
using System.Windows.Data;

namespace ChronoMock.App;

/// <summary>
/// Maps a <see cref="VerdictKind"/> to a distinct glyph, so the verdict is told apart by SHAPE and not by
/// colour alone (zasady/13 section 9): a check, a warning, a cross, a query. Paired with the coloured label.
/// </summary>
public sealed class VerdictGlyphConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is VerdictKind kind
            ? kind switch
            {
                VerdictKind.Works => "✓",        // check mark
                VerdictKind.Partial => "⚠",      // warning sign
                VerdictKind.Fails => "✕",        // multiplication x (cross)
                VerdictKind.Undetermined => "?", // question mark
                _ => "•",                         // bullet
            }
            : "•";

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;

namespace ChronoMock.App;

/// <summary>
/// Maps a <see cref="VerdictKind"/> to its named status brush from the palette (Colours.xaml), so the
/// verdict colour stays a named design value, never a literal (zasady/13 section 2.1). Colour only
/// reinforces the verdict - the glyph and the label carry the meaning on their own (section 9).
/// </summary>
public sealed class VerdictBrushConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var key = value is VerdictKind kind
            ? kind switch
            {
                VerdictKind.Works => "BrushStatusWorks",
                VerdictKind.Partial => "BrushStatusPartial",
                VerdictKind.Fails => "BrushStatusFails",
                _ => "BrushStatusUndetermined",
            }
            : "BrushStatusUndetermined";

        return Application.Current?.TryFindResource(key) as Brush ?? Brushes.Gray;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

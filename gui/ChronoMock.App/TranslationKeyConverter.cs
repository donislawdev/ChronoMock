using System.Globalization;
using System.Windows;
using System.Windows.Data;

namespace ChronoMock.App;

/// <summary>
/// Resolves a translation KEY (as the core and the view model emit - untouchable rules 15/16) to display
/// text in the current language, by looking it up in the merged string dictionaries. A missing key renders
/// as the key itself, never blank - a visible "status.running" is an honest "translation missing", not a
/// silent gap. This is the consumer's render step: keys travel, prose is produced here.
/// </summary>
public sealed class TranslationKeyConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is not string key || key.Length == 0)
        {
            return string.Empty;
        }

        return Application.Current?.TryFindResource(key) as string ?? key;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

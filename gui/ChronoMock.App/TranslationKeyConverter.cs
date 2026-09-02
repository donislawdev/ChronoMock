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
        => value is string key ? Resolve(key) : string.Empty;

    /// <summary>Resolve a translation key to display text in the current language, or the key itself when it
    /// is not found (an honest "translation missing", never a blank). Empty in, empty out. Used both by this
    /// converter (the view's render step) and by the view model when it must COMPOSE a display string from
    /// keyed parts and data (e.g. the calculator metadata line), where a single key binding will not do.</summary>
    public static string Resolve(string key)
    {
        if (key.Length == 0)
        {
            return string.Empty;
        }

        return Application.Current?.TryFindResource(key) as string ?? key;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

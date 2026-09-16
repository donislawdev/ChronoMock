using System.Globalization;
using System.Windows.Data;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Renders one covered or observed channel - a name and a read count - as a row. The count is folded into a
/// sentence (coverage.reads) so it does not read as a speed the way a bare count did next to the mode
/// multiplier. A plain string passes through untouched, so the same fact list can still carry a keyed or
/// pre-rendered row where one is bound instead of a channel.
/// </summary>
/// <remarks>
/// The wording is resolved with <see cref="TranslationKeyConverter.Resolve"/>, the same lookup a key binding
/// uses, so a row reads in the current language and follows a language change. The copy-summary builds the
/// identical line in code (SessionViewModel.FormatReadRow) - two surfaces, one key. The count is formatted
/// with the invariant culture, matching that summary, so a row and its copy read the same.
/// </remarks>
public sealed class CoverageRowConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture) => value switch
    {
        CoveredChannel channel => FormatChannel(channel),
        string text => text,
        _ => string.Empty,
    };

    // A malformed placeholder in a loose translation file must not throw out of a binding conversion and
    // blank the row - degrade to the raw template, matching SessionViewModel.Fmt on the copy-summary side.
    private static string FormatChannel(CoveredChannel channel)
    {
        var format = TranslationKeyConverter.Resolve(channel.Calls == 1 ? "coverage.reads_one" : "coverage.reads");
        try
        {
            return string.Format(CultureInfo.InvariantCulture, format, channel.Channel, channel.Calls);
        }
        catch (FormatException)
        {
            return format;
        }
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

using System.Globalization;
using System.Text.RegularExpressions;

namespace ChronoMock.App;

/// <summary>Which part of the moment an input error belongs to, so the UI can mark the right field.</summary>
internal enum MomentPart
{
    None,
    Date,
    Time,
}

/// <summary>The outcome of composing a date and an optional time into a canonical moment.</summary>
internal readonly record struct MomentResult(bool Ok, string Canonical, MomentPart ErrorPart, string ErrorKey);

/// <summary>
/// Parses and composes the moment input culture-invariantly. Dates are ISO yyyy-MM-dd and times are
/// 24-hour HH:mm[:ss], always read with InvariantCulture - the OS locale (a Polish box, a US VM) must
/// never change what a typed moment means (rule 2). The calendar picker feeds this the same ISO date
/// string, so the typed path and the picked path agree. Deep validation (DST gap, range) stays in the
/// core (docs/08 section 5) - this is the well-formed-ness and easy-entry layer only.
/// </summary>
internal static partial class MomentParse
{
    private const string DatePattern = "yyyy-MM-dd";
    private static readonly string[] TimePatterns = ["HH:mm", "HH:mm:ss"];

    /// <summary>
    /// Compose a date and an optional time into a canonical yyyy-MM-ddTHH:mm:ss. An empty time defaults to
    /// midnight, and seconds are optional. Returns the first part that fails, with a translation key.
    /// </summary>
    public static MomentResult Compose(string? dateText, string? timeText)
    {
        var d = (dateText ?? string.Empty).Trim();
        if (d.Length == 0)
        {
            return new MomentResult(false, string.Empty, MomentPart.Date, "moment.date_empty");
        }

        if (!DateTime.TryParseExact(d, DatePattern, CultureInfo.InvariantCulture, DateTimeStyles.None, out var date))
        {
            // Three different reasons, three different messages. A well-shaped but impossible date
            // (2025-04-31) is not the same problem as a wrong shape (a Polish 28.02.2025 or a US
            // 02/28/2025), and neither is a year this window cannot hold: the engine computes on
            // -262143..=262143 while DateTime stops at 1..=9999, so a calculator result like
            // -0974-01-01 arrives here perfectly well-formed and simply out of range. Telling that
            // reader to "use the form YYYY-MM-DD" sends them to fix a shape that is already right.
            var key = IsoDateShape().IsMatch(d) ? "moment.date_invalid"
                : WideYearIsoShape().IsMatch(d) ? "moment.date_year_range"
                : "moment.date_format";
            return new MomentResult(false, string.Empty, MomentPart.Date, key);
        }

        var t = (timeText ?? string.Empty).Trim();
        var timeOfDay = TimeSpan.Zero;
        if (t.Length > 0)
        {
            if (!DateTime.TryParseExact(t, TimePatterns, CultureInfo.InvariantCulture, DateTimeStyles.None, out var parsed))
            {
                return new MomentResult(false, string.Empty, MomentPart.Time, "moment.time_format");
            }

            timeOfDay = parsed.TimeOfDay;
        }

        var canonical = date.Add(timeOfDay).ToString("yyyy-MM-ddTHH:mm:ss", CultureInfo.InvariantCulture);
        return new MomentResult(true, canonical, MomentPart.None, string.Empty);
    }

    /// <summary>
    /// Split a canonical yyyy-MM-ddTHH:mm:ss (from history or the calculator bridge) back into the date and
    /// time fields. The time drops trailing :00 seconds so the field shows the tidy HH:mm most of the time.
    /// A value that is not canonical comes back as (whole-string, empty) so nothing is silently lost.
    /// </summary>
    public static (string Date, string Time) Split(string? canonical)
    {
        var c = (canonical ?? string.Empty).Trim();
        if (!DateTime.TryParseExact(
                c, "yyyy-MM-ddTHH:mm:ss", CultureInfo.InvariantCulture, DateTimeStyles.None, out var moment))
        {
            return (c, string.Empty);
        }

        var date = moment.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
        var time = moment.Second == 0
            ? moment.ToString("HH:mm", CultureInfo.InvariantCulture)
            : moment.ToString("HH:mm:ss", CultureInfo.InvariantCulture);
        return (date, time);
    }

    [GeneratedRegex(@"^\d{4}-\d{2}-\d{2}$")]
    private static partial Regex IsoDateShape();

    /// <summary>An ISO date whose YEAR is outside what <see cref="DateTime"/> holds: negative, or more
    /// than four digits. The engine's band is far wider, so its own output can land here - the shape is
    /// right and only the year is unreachable, which is a different message.</summary>
    [GeneratedRegex(@"^(-\d{1,6}|\d{5,6})-\d{2}-\d{2}$")]
    private static partial Regex WideYearIsoShape();
}

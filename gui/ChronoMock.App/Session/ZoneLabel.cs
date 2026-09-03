using System.Globalization;

namespace ChronoMock.App;

/// <summary>
/// Renders a session zone bias (minutes, Windows convention where UTC = local + bias) as a UTC offset label
/// like "UTC+02:00". Every clock reading carries its zone explicitly (untouchable rule 2), so this label is
/// never optional - it is the visible half of the rule that keeps a "N hours short" error from hiding.
/// </summary>
public static class ZoneLabel
{
    public static string FromBiasMinutes(int biasMinutes) => "UTC" + OffsetFromBiasMinutes(biasMinutes);

    /// <summary>The bare offset (<c>+02:00</c>), as the engine's <c>--zone</c> flag spells it. Same
    /// arithmetic as the display label, so the zone the panel shows and the zone it computes in can never
    /// disagree (untouchable rule 2).</summary>
    public static string OffsetFromBiasMinutes(int biasMinutes)
    {
        // Windows Bias: UTC = local + Bias, so the offset a reader expects to see is the negation of the bias.
        int offset = -biasMinutes;
        char sign = offset >= 0 ? '+' : '-';
        int magnitude = Math.Abs(offset);
        return string.Create(
            CultureInfo.InvariantCulture,
            $"{sign}{magnitude / 60:00}:{magnitude % 60:00}");
    }
}

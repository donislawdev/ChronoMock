using System.Globalization;

namespace ChronoMock.App;

/// <summary>
/// Renders a session zone bias (minutes, Windows convention where UTC = local + bias) as a UTC offset label
/// like "UTC+02:00". Every clock reading carries its zone explicitly (untouchable rule 2), so this label is
/// never optional - it is the visible half of the rule that keeps a "N hours short" error from hiding.
/// </summary>
public static class ZoneLabel
{
    public static string FromBiasMinutes(int biasMinutes)
    {
        // Windows Bias: UTC = local + Bias, so the offset a reader expects to see is the negation of the bias.
        int offset = -biasMinutes;
        char sign = offset >= 0 ? '+' : '-';
        int magnitude = Math.Abs(offset);
        return string.Create(
            CultureInfo.InvariantCulture,
            $"UTC{sign}{magnitude / 60:00}:{magnitude % 60:00}");
    }
}

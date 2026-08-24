using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The zone label (untouchable rule 2): a clock reading is never shown without its zone. Pure, so the
/// formatting is checked directly, including a non-hour offset and both signs.
/// </summary>
public class ZoneLabelTests
{
    [Theory]
    [InlineData(0, "UTC+00:00")]      // UTC
    [InlineData(-120, "UTC+02:00")]   // Poland summer, ahead of UTC
    [InlineData(300, "UTC-05:00")]    // US Eastern standard, behind UTC
    [InlineData(-330, "UTC+05:30")]   // half-hour offset, both digits exercised
    public void Formats_bias_as_utc_offset(int biasMinutes, string expected)
        => Assert.Equal(expected, ZoneLabel.FromBiasMinutes(biasMinutes));
}

using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The bridge to substitution (slice G6): a moment may travel only with a zone the substitution panel
/// offers, so it is never transferred as a bare local date paired with a wrong zone (rule 2).
/// </summary>
public class BridgeTests
{
    [Theory]
    [InlineData(0, true)]      // UTC - offered
    [InlineData(-120, true)]   // UTC+02:00 (Poland summer) - offered
    [InlineData(300, true)]    // UTC-05:00 (US Eastern) - offered
    [InlineData(-345, false)]  // UTC+05:45 (from a zone step) - not offered, cannot transfer faithfully
    [InlineData(-30, false)]   // UTC+00:30 - not offered
    public void A_zone_is_transferable_only_when_substitution_offers_it(int biasMinutes, bool expected)
        => Assert.Equal(expected, CalculatorViewModel.CanTransferZone(biasMinutes));
}

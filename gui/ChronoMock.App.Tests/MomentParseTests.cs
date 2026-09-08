using System.Globalization;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

public class MomentParseTests
{
    private static MomentResult InCulture(string culture, string? date, string? time)
    {
        var prevCulture = CultureInfo.CurrentCulture;
        var prevUi = CultureInfo.CurrentUICulture;
        try
        {
            var c = culture.Length == 0 ? CultureInfo.InvariantCulture : new CultureInfo(culture);
            CultureInfo.CurrentCulture = c;
            CultureInfo.CurrentUICulture = c;
            return MomentParse.Compose(date, time);
        }
        finally
        {
            CultureInfo.CurrentCulture = prevCulture;
            CultureInfo.CurrentUICulture = prevUi;
        }
    }

    // The headline: a Polish box and a US VM must read the same typed moment identically. The OS date
    // format must never change the meaning of the input (rule 2).
    [Theory]
    [InlineData("pl-PL")]
    [InlineData("en-US")]
    [InlineData("de-DE")]
    [InlineData("")]
    public void Compose_is_culture_invariant(string culture)
    {
        var r = InCulture(culture, "2025-02-28", "03:14");
        Assert.True(r.Ok);
        Assert.Equal("2025-02-28T03:14:00", r.Canonical);
    }

    [Fact]
    public void Date_only_defaults_to_midnight()
    {
        var r = MomentParse.Compose("2038-01-19", "");
        Assert.True(r.Ok);
        Assert.Equal("2038-01-19T00:00:00", r.Canonical);
    }

    [Fact]
    public void Seconds_are_optional_and_kept_when_given()
    {
        Assert.Equal("2038-01-19T03:14:00", MomentParse.Compose("2038-01-19", "03:14").Canonical);
        Assert.Equal("2038-01-19T03:14:07", MomentParse.Compose("2038-01-19", "03:14:07").Canonical);
    }

    [Fact]
    public void Impossible_date_is_flagged_on_the_date_part()
    {
        var r = MomentParse.Compose("2025-04-31", "");
        Assert.False(r.Ok);
        Assert.Equal(MomentPart.Date, r.ErrorPart);
        Assert.Equal("moment.date_invalid", r.ErrorKey);
    }

    /// <summary>R2-C6. A year the engine computes on but this window cannot hold gets its OWN message.
    /// The calculator's band is -262143..=262143 and DateTime stops at 1..=9999, so a result like
    /// -0974-01-01 comes back through Split perfectly well-formed and unusable here. It used to be
    /// reported as a format error, which sends the reader to fix a shape that is already correct.</summary>
    [Theory]
    [InlineData("-0974-01-01")]   // a real calculator result: 2026 minus 3000 years
    [InlineData("-000001-01-01")]
    [InlineData("10000-01-01")]
    [InlineData("262143-12-31")]
    public void A_year_outside_this_windows_range_says_so(string wideYear)
    {
        var r = MomentParse.Compose(wideYear, string.Empty);
        Assert.False(r.Ok);
        Assert.Equal(MomentPart.Date, r.ErrorPart);
        Assert.Equal("moment.date_year_range", r.ErrorKey);
    }

    /// <summary>The neighbours keep their own messages - the range case must not swallow them.</summary>
    [Fact]
    public void The_year_range_message_does_not_swallow_the_other_two()
    {
        // Right shape, impossible day.
        Assert.Equal("moment.date_invalid", MomentParse.Compose("2025-04-31", string.Empty).ErrorKey);
        // Wrong shape entirely.
        Assert.Equal("moment.date_format", MomentParse.Compose("28.02.2025", string.Empty).ErrorKey);
        // A four-digit year in range still parses.
        Assert.True(MomentParse.Compose("0999-01-01", string.Empty).Ok);
    }

    [Fact]
    public void Out_of_range_time_is_flagged_on_the_time_part()
    {
        var r = MomentParse.Compose("2025-01-01", "25:00");
        Assert.False(r.Ok);
        Assert.Equal(MomentPart.Time, r.ErrorPart);
        Assert.Equal("moment.time_format", r.ErrorKey);
    }

    // A locale-formatted date is rejected, never silently reinterpreted - the exact Polish-box / US-VM trap.
    [Theory]
    [InlineData("28.02.2025")]  // Polish
    [InlineData("02/28/2025")]  // US
    [InlineData("28/02/2025")]  // day-first
    public void Locale_formatted_date_is_rejected(string localeDate)
    {
        var r = MomentParse.Compose(localeDate, string.Empty);
        Assert.False(r.Ok);
        Assert.Equal(MomentPart.Date, r.ErrorPart);
        Assert.Equal("moment.date_format", r.ErrorKey);
    }

    [Fact]
    public void Empty_date_is_flagged()
    {
        var r = MomentParse.Compose(string.Empty, string.Empty);
        Assert.False(r.Ok);
        Assert.Equal("moment.date_empty", r.ErrorKey);
    }

    [Fact]
    public void Split_round_trips_a_canonical_moment()
    {
        Assert.Equal(("2038-01-19", "03:14:07"), MomentParse.Split("2038-01-19T03:14:07"));
        Assert.Equal(("2038-01-19", "00:00"), MomentParse.Split("2038-01-19T00:00:00"));

        var (d, t) = MomentParse.Split("2038-01-19T00:00:00");
        Assert.Equal("2038-01-19T00:00:00", MomentParse.Compose(d, t).Canonical);
    }
}

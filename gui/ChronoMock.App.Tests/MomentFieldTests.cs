using System.Globalization;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

public class MomentFieldTests
{
    [Fact]
    public void Fields_compose_into_a_canonical_moment()
    {
        var f = new MomentField { DateText = "2038-01-19", TimeText = "03:14" };
        Assert.True(f.IsValid);
        Assert.Equal("2038-01-19T03:14:00", f.Canonical);
    }

    // The calendar popup binds SelectedDate two-way: a picked day writes the ISO date text, and a valid
    // typed date exposes the DateTime the calendar highlights. Culture-invariant either way (rule 2).
    [Fact]
    public void Selected_date_round_trips_with_the_date_text()
    {
        var f = new MomentField { DateText = "2040-06-15" };
        Assert.Equal(new DateTime(2040, 6, 15), f.SelectedDate);

        f.SelectedDate = new DateTime(2041, 7, 16);
        Assert.Equal("2041-07-16", f.DateText);
    }

    [Fact]
    public void An_invalid_date_has_no_selected_date()
        => Assert.Null(new MomentField { DateText = "nope" }.SelectedDate);

    [Fact]
    public void Load_canonical_splits_into_the_two_fields_and_validates()
    {
        var f = new MomentField();
        f.LoadCanonical("2040-06-15T08:30:00");
        Assert.Equal("2040-06-15", f.DateText);
        Assert.Equal("08:30", f.TimeText);
        Assert.True(f.IsValid);
    }

    [Fact]
    public void Changed_fires_when_a_field_changes()
    {
        var f = new MomentField();
        var fired = 0;
        f.Changed += (_, _) => fired++;
        f.DateText = "2038-01-19";
        Assert.True(fired > 0);
    }

    [Fact]
    public void Per_part_error_flags_point_at_the_wrong_field()
    {
        var f = new MomentField { DateText = "2025-04-31", TimeText = "03:14" };
        Assert.True(f.HasDateError);
        Assert.False(f.HasTimeError);
        Assert.Equal("moment.date_invalid", f.DateErrorKey);
    }

    // Today is midnight in the SESSION zone, not the OS local zone (rule 2). With a fixed UTC instant and a
    // +02:00 session zone, an evening-UTC instant is already the next calendar day locally.
    [Fact]
    public void Set_today_uses_the_session_zone_date_at_midnight()
    {
        var f = new MomentField();
        f.SetToday(-120, new DateTime(2026, 9, 1, 23, 30, 0, DateTimeKind.Utc)); // UTC+02:00 -> 2026-09-02 01:30
        Assert.Equal("2026-09-02", f.DateText);
        Assert.Equal(string.Empty, f.TimeText);
        Assert.Equal("2026-09-02T00:00:00", f.Canonical);
    }

    // Now is the wall time in the session zone. A US Pacific bias (UTC-08:00) shifts an early-UTC instant
    // back to the previous day, proving the bias sign (local = UTC - bias).
    [Fact]
    public void Set_now_uses_the_session_zone_wall_time()
    {
        var f = new MomentField();
        f.SetNow(480, new DateTime(2026, 9, 1, 2, 0, 0, DateTimeKind.Utc)); // UTC-08:00 -> 2026-08-31 18:00
        Assert.Equal("2026-08-31", f.DateText);
        Assert.Equal("18:00", f.TimeText);
        Assert.Equal("2026-08-31T18:00:00", f.Canonical);
    }

    // Seconds are kept only when non-zero, matching the field's tidy Split convention.
    [Fact]
    public void Set_now_keeps_seconds_only_when_non_zero()
    {
        var onTheMinute = new MomentField();
        onTheMinute.SetNow(0, new DateTime(2026, 9, 1, 8, 30, 0, DateTimeKind.Utc));
        Assert.Equal("08:30", onTheMinute.TimeText);

        var withSeconds = new MomentField();
        withSeconds.SetNow(0, new DateTime(2026, 9, 1, 8, 30, 7, DateTimeKind.Utc));
        Assert.Equal("08:30:07", withSeconds.TimeText);
    }

    // The headline: Today/Now must read the same on a Polish box and a US VM. The OS date format never
    // enters the computed text (rule 2), so the same instant and bias give the same fields in any culture.
    [Theory]
    [InlineData("pl-PL")]
    [InlineData("en-US")]
    [InlineData("de-DE")]
    public void Set_today_and_now_are_culture_invariant(string culture)
    {
        var utc = new DateTime(2026, 3, 4, 5, 6, 7, DateTimeKind.Utc);
        var (today, now) = InCulture(culture, utc);
        Assert.Equal("2026-03-04", today.DateText);
        Assert.Equal("2026-03-04T00:00:00", today.Canonical);
        Assert.Equal("2026-03-04", now.DateText);
        Assert.Equal("05:06:07", now.TimeText);
    }

    private static (MomentField Today, MomentField Now) InCulture(string culture, DateTime utcNow)
    {
        var prevCulture = CultureInfo.CurrentCulture;
        var prevUi = CultureInfo.CurrentUICulture;
        try
        {
            var c = new CultureInfo(culture);
            CultureInfo.CurrentCulture = c;
            CultureInfo.CurrentUICulture = c;
            var today = new MomentField();
            today.SetToday(0, utcNow);
            var now = new MomentField();
            now.SetNow(0, utcNow);
            return (today, now);
        }
        finally
        {
            CultureInfo.CurrentCulture = prevCulture;
            CultureInfo.CurrentUICulture = prevUi;
        }
    }
}

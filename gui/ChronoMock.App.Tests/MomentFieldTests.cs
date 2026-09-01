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
}

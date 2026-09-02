using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// Reverse analysis (slice G5): the analyze argument list and the mapping from an engine reading to a row.
/// Pure - no UI thread, no process.
/// </summary>
public class ReverseAnalysisTests
{
    [Fact]
    public void Analyze_args_wrap_the_trimmed_text()
        => Assert.Equal(new[] { "--analyze", "04/08/2008" }, CalculatorViewModel.BuildAnalyzeArgs("  04/08/2008 "));

    [Fact]
    public void A_reading_row_maps_the_key_date_and_significance()
    {
        var metadata = new CalcMetadata("Tuesday", 2008, 15, 15, 99, 2, true, -6714, null, null);
        var reading = new CalcReading("us_month_day", "2008-04-08T00:00:00", ["end_of_quarter"], metadata);

        var row = new ReadingRow(reading);

        Assert.Equal("calc.reading.us_month_day", row.ReadingLabelKey);
        // The weekday is a translation key (mapped from the engine's English name), rendered via KeyToText;
        // the date is data. Kept as a key so the row stays language-neutral and testable without WPF.
        Assert.Equal("calc.weekday.tuesday", row.WeekdayKey);
        Assert.Equal("2008-04-08", row.Date);
        Assert.Equal(new[] { "calc.sig.end_of_quarter" }, row.Significance);
    }
}

using System.Text.Json;
using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// The calculator client (Stage 4, GUI slice G3): parsing the chronomock.calc/1 contract, and driving
/// the REAL <c>chrono calc</c> end to end (like the conformance tests drive the real core). The parse
/// tests need no process; the integration tests spawn the built core and require a release build.
/// </summary>
public class CalcClientTests
{
    [Fact]
    public void Parses_a_moment_result()
    {
        const string json = """
        {"schema":"chronomock.calc/1","moment":{"iso":"2026-09-30T23:59:59","zone_bias_min":0,
         "base":"2026-08-26T00:00:00","steps":["2026-09-30T23:59:59"],
         "formats":{"iso_date":"2026-09-30","iso_datetime":"2026-09-30T23:59:59+00:00","us":"09/30/2026",
                    "pl":"30.09.2026","epoch_seconds":1790812799,"epoch_millis":1790812799000,
                    "filetime":134352863990000000,"rfc1123":"Wed, 30 Sep 2026 23:59:59 GMT"},
         "metadata":{"weekday":"Wednesday","iso_week_year":2026,"iso_week":40,"us_week":40,
                     "day_of_year":273,"quarter":3,"is_leap_year":false,"days_from_today":35,
                     "business_day":true,"holiday":null},
         "significance":["end_of_quarter"]}}
        """;
        var r = JsonSerializer.Deserialize<CalcResult>(json, ProtocolJson.Options)!;
        Assert.Equal("chronomock.calc/1", r.Schema);
        Assert.Null(r.Analysis);
        Assert.NotNull(r.Moment);
        Assert.Equal("2026-09-30T23:59:59", r.Moment!.Iso);
        Assert.Equal("2026-09-30", r.Moment.Formats.IsoDate);
        Assert.Equal(1790812799L, r.Moment.Formats.EpochSeconds!.Value);
        Assert.True(r.Moment.Metadata.BusinessDay is true);
        Assert.Contains("end_of_quarter", r.Moment.Significance);
    }

    [Fact]
    public void Parses_out_of_range_instant_and_no_calendar_as_null()
    {
        const string json = """
        {"schema":"chronomock.calc/1","moment":{"iso":"40000-01-01T00:00:00","zone_bias_min":0,
         "base":"40000-01-01T00:00:00","steps":[],
         "formats":{"iso_date":"40000-01-01","iso_datetime":"40000-01-01T00:00:00+00:00","us":"01/01/40000",
                    "pl":"01.01.40000","epoch_seconds":null,"epoch_millis":null,"filetime":null,"rfc1123":null},
         "metadata":{"weekday":"Saturday","iso_week_year":39999,"iso_week":52,"us_week":1,"day_of_year":1,
                     "quarter":1,"is_leap_year":true,"days_from_today":13869481,"business_day":null,"holiday":null},
         "significance":["start_of_year"]}}
        """;
        var r = JsonSerializer.Deserialize<CalcResult>(json, ProtocolJson.Options)!;
        Assert.Null(r.Moment!.Formats.EpochSeconds);
        Assert.Null(r.Moment.Formats.Rfc1123);
        Assert.Null(r.Moment.Metadata.BusinessDay);
    }

    [Fact]
    public void Parses_an_ambiguous_analysis()
    {
        const string json = """
        {"schema":"chronomock.calc/1","analysis":{"input":"04/08/2008","ambiguous":true,"readings":[
          {"reading":"us_month_day","iso":"2008-04-08T00:00:00","significance":[],
           "metadata":{"weekday":"Tuesday","iso_week_year":2008,"iso_week":15,"us_week":15,"day_of_year":99,
                       "quarter":2,"is_leap_year":true,"days_from_today":-6714,"business_day":null,"holiday":null}},
          {"reading":"pl_day_month","iso":"2008-08-04T00:00:00","significance":[],
           "metadata":{"weekday":"Monday","iso_week_year":2008,"iso_week":32,"us_week":32,"day_of_year":217,
                       "quarter":3,"is_leap_year":true,"days_from_today":-6596,"business_day":null,"holiday":null}}]}}
        """;
        var r = JsonSerializer.Deserialize<CalcResult>(json, ProtocolJson.Options)!;
        Assert.Null(r.Moment);
        Assert.NotNull(r.Analysis);
        Assert.True(r.Analysis!.Ambiguous);
        Assert.Equal(2, r.Analysis.Readings.Count);
        Assert.Equal("us_month_day", r.Analysis.Readings[0].Reading);
        Assert.Equal("pl_day_month", r.Analysis.Readings[1].Reading);
    }

    [Fact]
    [Trait("Category", "Integration")] // spawns the real chrono - excluded from the hermetic gate (RELEASE-010)
    public async Task Evaluates_against_the_real_core()
    {
        // Spawns the REAL chrono calc, proving the GUI client and the engine's JSON contract agree end
        // to end (the method the conformance tests use for the core). Requires `cargo build --release`.
        var client = CalcClient.ForRepo(RepoPaths.RepoRoot());
        var r = await client.EvaluateAsync(["--base", "2026-09-30T23:59:59", "--calendar", "us-banking"]);
        Assert.Equal("chronomock.calc/1", r.Schema);
        Assert.NotNull(r.Moment);
        Assert.Equal("2026-09-30T23:59:59", r.Moment!.Iso);
        Assert.True(r.Moment.Metadata.BusinessDay is true);
        Assert.Contains("end_of_quarter", r.Moment.Significance);
    }

    [Fact]
    [Trait("Category", "Integration")] // spawns the real chrono - excluded from the hermetic gate (RELEASE-010)
    public async Task Reports_a_usage_error_as_an_exception_with_the_exit_code()
    {
        var client = CalcClient.ForRepo(RepoPaths.RepoRoot());
        var ex = await Assert.ThrowsAsync<CalcException>(
            () => client.EvaluateAsync(["--base", "2026-02-31T00:00:00"])); // impossible day -> exit 1
        Assert.Equal(1, ex.ExitCode);
    }
}

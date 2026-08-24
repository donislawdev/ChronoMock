using System.Text.Json;
using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>Parser round-trip and robustness against the flat wire from <c>crates/proto</c>.</summary>
public class ParserTests
{
    [Fact]
    public void Parses_ready_event_fields()
    {
        var line = """{"type":"ready","v":1,"protocol":1,"core_version":"0.1.0","bitness":"x64","capabilities":[]}""";
        var evt = Assert.IsType<ReadyEvent>(EventParser.Parse(line));
        Assert.Equal(1, evt.Protocol);
        Assert.Equal("0.1.0", evt.CoreVersion);
        Assert.Equal("x64", evt.Bitness);
    }

    [Fact]
    public void Parses_state_with_both_clocks()
    {
        var line = """{"type":"state","v":1,"fake":{"wall":"2038-01-19T03:14:07","zone_bias_min":0},"real":{"wall":"2026-08-24T10:00:00","zone_bias_min":0},"multiplier":60,"elapsed_fake_ms":90000,"elapsed_real_ms":1500}""";
        var evt = Assert.IsType<StateEvent>(EventParser.Parse(line));
        Assert.Equal("2038-01-19T03:14:07", evt.Fake.Wall);
        Assert.Equal(60, evt.Multiplier);
        Assert.Equal(90000, evt.ElapsedFakeMs);
    }

    [Fact]
    public void Unknown_event_type_is_ignored()
        => Assert.Null(EventParser.Parse("""{"type":"future_thing","v":1}"""));

    [Fact]
    public void Unknown_fields_are_ignored()
    {
        var line = """{"type":"ack","v":1,"id":3,"extra_future_field":true}""";
        var evt = Assert.IsType<AckEvent>(EventParser.Parse(line));
        Assert.Equal(3, evt.Id);
    }

    [Fact]
    public void Malformed_line_throws_for_the_caller_to_record()
        => Assert.ThrowsAny<JsonException>(() => EventParser.Parse("this is not json"));

    [Fact]
    public void Coverage_defaults_absent_observed_to_empty()
    {
        // A coverage line from before `observed` existed still parses (additive evolution).
        var line = """{"type":"coverage","v":1,"pid":42,"covered":[],"uncovered":[],"warning_keys":[]}""";
        var evt = Assert.IsType<CoverageEvent>(EventParser.Parse(line));
        Assert.Empty(evt.Observed);
    }

    [Fact]
    public void Start_command_serializes_to_the_expected_wire()
    {
        var start = new StartCommand
        {
            Id = 1,
            Target = new TargetSpec { Path = "C:/app.exe" },
            Time = new TimeSpec
            {
                Moment = new MomentSpec { Kind = "absolute", Local = "2038-01-19T03:14:07", TzBiasMin = 0 },
                Mode = "frozen",
            },
        };

        var json = start.ToNdjson();
        Assert.Contains("\"type\":\"start\"", json, StringComparison.Ordinal);
        Assert.Contains("\"scale_duration\":false", json, StringComparison.Ordinal);
        Assert.Contains("\"tz_bias_min\":0", json, StringComparison.Ordinal);
        Assert.DoesNotContain("\"cwd\"", json, StringComparison.Ordinal); // null is omitted on write
        Assert.DoesNotContain("\"multiplier\"", json, StringComparison.Ordinal); // null is omitted on write
    }
}

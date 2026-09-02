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
    public void Parses_ended_with_target_exit_code_and_residue()
    {
        // Native end: the app exited on its own (a real exit code), teardown was clean (no residue) -
        // exactly the shape crates/cli emits at main.rs run_session.
        var native = """{"type":"ended","v":1,"clean":true,"residue_keys":[],"target_exit_code":3,"elapsed_real_ms":1500,"elapsed_fake_ms":90000,"fake_end_wall":"2038-01-20T00:00:00"}""";
        var n = Assert.IsType<EndedEvent>(EventParser.Parse(native));
        Assert.Equal(3, n.TargetExitCode);
        Assert.Empty(n.ResidueKeys);

        // CDP end: no target exit code (null), but the temp profile could not be removed - the shape the
        // CDP driver emits when shutdown_with_residue reports a leftover.
        var cdp = """{"type":"ended","v":1,"clean":false,"residue_keys":["cleanup.chromium_profile_left"],"target_exit_code":null,"elapsed_real_ms":500,"elapsed_fake_ms":30000,"fake_end_wall":"2038-01-19T03:14:07"}""";
        var c = Assert.IsType<EndedEvent>(EventParser.Parse(cdp));
        Assert.Null(c.TargetExitCode);
        Assert.Equal(["cleanup.chromium_profile_left"], c.ResidueKeys);
        Assert.False(c.Clean);
    }

    [Fact]
    public void Ended_from_before_the_new_fields_still_parses()
    {
        // Additive evolution: an older `ended` with neither field parses, defaulting to empty/none.
        var line = """{"type":"ended","v":1,"clean":true,"elapsed_real_ms":0,"elapsed_fake_ms":0}""";
        var evt = Assert.IsType<EndedEvent>(EventParser.Parse(line));
        Assert.Null(evt.TargetExitCode);
        Assert.Empty(evt.ResidueKeys);
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
        Assert.Contains("\"scale_qpc\":false", json, StringComparison.Ordinal);
        Assert.Contains("\"tz_bias_min\":0", json, StringComparison.Ordinal);
        Assert.DoesNotContain("\"cwd\"", json, StringComparison.Ordinal); // null is omitted on write
        Assert.DoesNotContain("\"multiplier\"", json, StringComparison.Ordinal); // null is omitted on write
    }

    [Fact]
    public void Start_command_carries_scale_qpc_when_set()
    {
        var start = new StartCommand
        {
            Id = 1,
            Target = new TargetSpec { Path = "C:/app.exe" },
            Time = new TimeSpec
            {
                Moment = new MomentSpec { Kind = "absolute", Local = "2038-01-19T03:14:07", TzBiasMin = 0 },
                Mode = "multiplier",
                Multiplier = 60,
                ScaleQpc = true,
            },
        };

        Assert.Contains("\"scale_qpc\":true", start.ToNdjson(), StringComparison.Ordinal);
    }
}

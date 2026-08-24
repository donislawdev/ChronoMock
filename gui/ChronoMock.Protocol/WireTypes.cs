using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>What to run, as it appears on the wire (mirrors <c>chrono_proto::TargetSpec</c>).</summary>
public sealed record TargetSpec
{
    [JsonPropertyName("path")] public required string Path { get; init; }
    [JsonPropertyName("args")] public IReadOnlyList<string> Args { get; init; } = [];
    [JsonPropertyName("cwd")] public string? Cwd { get; init; }
}

/// <summary>The target moment, session-zone semantics (mirrors <c>chrono_proto::MomentSpec</c>).</summary>
public sealed record MomentSpec
{
    /// <summary>"absolute" or "relative".</summary>
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("local")] public string? Local { get; init; }
    [JsonPropertyName("tz_bias_min")] public int? TzBiasMin { get; init; }
    [JsonPropertyName("delta")] public string? Delta { get; init; }
}

/// <summary>Time-flow selection (mirrors <c>chrono_proto::TimeSpec</c>).</summary>
public sealed record TimeSpec
{
    [JsonPropertyName("moment")] public required MomentSpec Moment { get; init; }

    /// <summary>"flow" | "frozen" | "multiplier".</summary>
    [JsonPropertyName("mode")] public required string Mode { get; init; }

    [JsonPropertyName("multiplier")] public long? Multiplier { get; init; }
    [JsonPropertyName("scale_duration")] public bool ScaleDuration { get; init; }
}

/// <summary>One clock reading: wall-clock text plus the session zone it is expressed in.</summary>
public sealed record Clock
{
    [JsonPropertyName("wall")] public required string Wall { get; init; }
    [JsonPropertyName("zone_bias_min")] public int ZoneBiasMin { get; init; }
}

/// <summary>One channel's coverage and how many times the target has called it so far.</summary>
public sealed record CoveredChannel
{
    [JsonPropertyName("channel")] public required string Channel { get; init; }
    [JsonPropertyName("calls")] public long Calls { get; init; }
}

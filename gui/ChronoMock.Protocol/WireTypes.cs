using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>What to run, as it appears on the wire (mirrors <c>chrono_proto::TargetSpec</c>).</summary>
public sealed record TargetSpec
{
    [JsonPropertyName("path")] public required string Path { get; init; }
    [JsonPropertyName("args")] public IReadOnlyList<string> Args { get; init; } = [];
    [JsonPropertyName("cwd")] public string? Cwd { get; init; }

    /// <summary>Reach the web pages inside the application through its embedded web engine's debugging
    /// port (docs/09). On by default, the opt-out is the panel's checkbox. Ignored for a target that IS
    /// Chromium, which the core drives over its own port either way.</summary>
    [JsonPropertyName("embedded")] public bool Embedded { get; init; } = true;
}

/// <summary>A DevTools endpoint the session reached inside the application: the pid that holds it,
/// the loopback port, and what the engine calls itself (mirrors <c>chrono_proto::ReachedEngine</c>).</summary>
public sealed record ReachedEngine
{
    [JsonPropertyName("pid")] public uint Pid { get; init; }
    [JsonPropertyName("port")] public int Port { get; init; }
    [JsonPropertyName("browser")] public string Browser { get; init; } = "";
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

    /// <summary>Also scale QueryPerformanceCounter (ADR-2 reversal, opt-in). Separate from ScaleDuration
    /// because scaling QPC also scales a target's QPC-timed rendering. Additive: an older core defaults it
    /// to false (QPC left real).</summary>
    [JsonPropertyName("scale_qpc")] public bool ScaleQpc { get; init; }
}

/// <summary>One clock reading: wall-clock text plus the session zone it is expressed in.</summary>
public sealed record Clock
{
    [JsonPropertyName("wall")] public required string Wall { get; init; }
    [JsonPropertyName("zone_bias_min")] public int ZoneBiasMin { get; init; }
}

/// <summary>
/// A process the family spawned without the hook inside it - it ran on the real clock. <c>Image</c> is
/// the executable's file name when the child was still alive to be asked, null otherwise.
/// </summary>
public sealed record UncoveredChild
{
    [JsonPropertyName("pid")] public uint Pid { get; init; }
    [JsonPropertyName("parent_pid")] public uint ParentPid { get; init; }
    [JsonPropertyName("image")] public string? Image { get; init; }
    /// <summary>The <c>--type=</c> role a Chromium-based engine gives each subprocess (renderer,
    /// gpu-process, utility), read off the command line while the child was alive. Null without one.</summary>
    [JsonPropertyName("role")] public string? Role { get; init; }
}

/// <summary>One channel's coverage and how many times the target has called it so far.</summary>
public sealed record CoveredChannel
{
    [JsonPropertyName("channel")] public required string Channel { get; init; }
    [JsonPropertyName("calls")] public long Calls { get; init; }
}

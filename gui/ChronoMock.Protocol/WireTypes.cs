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

    /// <summary>Reach the web pages of an application that runs as administrator (docs/09 section 12.19): the
    /// session writes one WebView2 value to the machine registry for its duration and removes it at the end,
    /// because an elevated WebView2 host ignores the setting <see cref="Embedded"/> relies on. OFF by default,
    /// since it changes the machine and opens a debugging port in an application with administrator rights.
    /// Written out rather than left to the core's default, like <see cref="Embedded"/>.</summary>
    [JsonPropertyName("elevated_embedded")] public bool ElevatedEmbedded { get; init; }

    /// <summary>Start the web engine with its slowdown of timers in hidden windows switched off (R4-N28):
    /// a Chromium engine runs the timers of a minimized or hidden window about once a second, which takes
    /// the speed-up away from them. The browser of a Chromium target and the engine inside an application
    /// alike. OFF by default, because the application then also works at full pace while hidden. Written
    /// out rather than left to the core's default, like <see cref="Embedded"/>.</summary>
    [JsonPropertyName("keep_background_timers")] public bool KeepBackgroundTimers { get; init; }

    /// <summary>Leave the sandbox of an application on Mozilla's Gecko engine on
    /// (ADR-28). False starts the target with the variables that switch it off, so the hook enters the
    /// processes its pages run in, at the cost of that protection while they run. ON by default, and written
    /// out rather than left to the core's default, like <see cref="Embedded"/>.</summary>
    [JsonPropertyName("gecko_sandbox")] public bool GeckoSandbox { get; init; } = true;

    /// <summary>Where a console program's console is (mirrors <c>chrono_proto::TargetConsole</c>, docs/08,
    /// ADR-17): <see cref="SharedConsole"/> gives it the core's console and stderr, <see cref="NewConsole"/>
    /// a console window of its own. Neither hands it the core's stdin or stdout, which carry this protocol.
    /// Written out rather than left to the core's default, like <see cref="Embedded"/>, so the wire says
    /// what the client decided. A value the core does not know makes it refuse the whole start.</summary>
    [JsonPropertyName("console")] public string Console { get; init; } = SharedConsole;

    public const string SharedConsole = "shared";
    public const string NewConsole = "new";
}

/// <summary>A DevTools endpoint the session reached inside the application: the pid that holds it,
/// the loopback port, and what the engine calls itself (mirrors <c>chrono_proto::ReachedEngine</c>).</summary>
public sealed record ReachedEngine
{
    [JsonPropertyName("pid")] public uint Pid { get; init; }
    [JsonPropertyName("port")] public int Port { get; init; }
    [JsonPropertyName("browser")] public string Browser { get; init; } = "";

    /// <summary>How many pages and workers the session put on its clock through this endpoint - this engine's
    /// share of the verdict's context count (R5, P15). Zero for an engine the session attached to and shimmed
    /// nothing in, and in messages from an older core, which is the side that claims none of its pages.</summary>
    [JsonPropertyName("context_count")] public int ContextCount { get; init; }
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

/// <summary>
/// A process on the session clock that was still running when the target closed, so the session went on
/// for it (ADR-16, mirrors <c>chrono_proto::FollowedProcess</c>). <c>Image</c> is the executable's file name
/// when the process list had it, null otherwise.
/// </summary>
public sealed record FollowedProcess
{
    [JsonPropertyName("pid")] public uint Pid { get; init; }
    [JsonPropertyName("image")] public string? Image { get; init; }
}

/// <summary>One channel's coverage and how many times the target has called it so far.</summary>
public sealed record CoveredChannel
{
    [JsonPropertyName("channel")] public required string Channel { get; init; }
    [JsonPropertyName("calls")] public long Calls { get; init; }

    /// <summary>The count stopped at the hook's ceiling, so the target called the channel AT LEAST
    /// <see cref="Calls"/> times (R5-WY11, mirrors <c>chrono_proto::CoveredChannel::capped</c>). Absent on the
    /// wire when false, and an older message reads as an exact count.</summary>
    [JsonPropertyName("capped")] public bool Capped { get; init; }
}

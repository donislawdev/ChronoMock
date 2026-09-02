using System.Text.Json;
using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>
/// Base for commands the interface sends to the core on its stdin (mirrors <c>chrono_proto::Command</c>).
/// The wire is flat: every message repeats <c>v</c> and carries a <c>type</c> discriminator.
/// </summary>
public abstract record Command
{
    [JsonPropertyName("v")] public int V { get; init; } = ProtocolJson.ProtocolVersion;
    [JsonPropertyName("id")] public required long Id { get; init; }

    /// <summary>
    /// Serialize to a single NDJSON line (no trailing newline). Uses the runtime type so the concrete
    /// command's fields and its <c>type</c> discriminator are written.
    /// </summary>
    public string ToNdjson() => JsonSerializer.Serialize(this, GetType(), ProtocolJson.Options);
}

public sealed record StartCommand : Command
{
    [JsonPropertyName("type")] public string Type => "start";
    [JsonPropertyName("target")] public required TargetSpec Target { get; init; }
    [JsonPropertyName("time")] public required TimeSpec Time { get; init; }

    /// <summary>Run even when the opening verdict says the substitution did not take effect. Without it
    /// the core stops the target and refuses, because an application that looks time-shifted but is not
    /// produces evidence about a session that never happened.</summary>
    [JsonPropertyName("force")] public bool Force { get; init; }
}

public sealed record QueryCommand : Command
{
    [JsonPropertyName("type")] public string Type => "query";

    /// <summary>Stable key for what to report, e.g. "state".</summary>
    [JsonPropertyName("what")] public required string What { get; init; }
}

public sealed record SetMultiplierCommand : Command
{
    [JsonPropertyName("type")] public string Type => "set_multiplier";
    [JsonPropertyName("multiplier")] public required long Multiplier { get; init; }
}

public sealed record JumpCommand : Command
{
    [JsonPropertyName("type")] public string Type => "jump";
    [JsonPropertyName("to")] public required MomentSpec To { get; init; }
}

public sealed record EndCommand : Command
{
    [JsonPropertyName("type")] public string Type => "end";
}

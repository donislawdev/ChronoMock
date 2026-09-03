using System.Text.Json;
using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>
/// Base for events the core sends on its stdout (mirrors <c>chrono_proto::Event</c>). The core emits
/// stable translation KEYS and structured data, never translated prose (untouchable rules 15/16); the
/// consumer renders keys in the user's language.
/// </summary>
public abstract record ChronoEvent
{
    [JsonPropertyName("v")] public int V { get; init; }
}

public sealed record ReadyEvent : ChronoEvent
{
    [JsonPropertyName("protocol")] public int Protocol { get; init; }
    [JsonPropertyName("core_version")] public required string CoreVersion { get; init; }
    [JsonPropertyName("bitness")] public required string Bitness { get; init; }
    [JsonPropertyName("capabilities")] public IReadOnlyList<string> Capabilities { get; init; } = [];
}

public sealed record CoverageEvent : ChronoEvent
{
    [JsonPropertyName("pid")] public int Pid { get; init; }
    [JsonPropertyName("covered")] public IReadOnlyList<CoveredChannel> Covered { get; init; } = [];
    [JsonPropertyName("observed")] public IReadOnlyList<CoveredChannel> Observed { get; init; } = [];
    [JsonPropertyName("uncovered")] public IReadOnlyList<string> Uncovered { get; init; } = [];
    [JsonPropertyName("warning_keys")] public IReadOnlyList<string> WarningKeys { get; init; } = [];
}

public sealed record VerdictEvent : ChronoEvent
{
    [JsonPropertyName("id")] public long? Id { get; init; }
    [JsonPropertyName("verdict")] public required string Verdict { get; init; }
    [JsonPropertyName("refuse_start")] public bool RefuseStart { get; init; }
    [JsonPropertyName("reason_key")] public required string ReasonKey { get; init; }
}

public sealed record AckEvent : ChronoEvent
{
    [JsonPropertyName("id")] public long Id { get; init; }
}

public sealed record StateEvent : ChronoEvent
{
    [JsonPropertyName("fake")] public required Clock Fake { get; init; }
    [JsonPropertyName("real")] public required Clock Real { get; init; }
    [JsonPropertyName("multiplier")] public long Multiplier { get; init; }
    [JsonPropertyName("elapsed_fake_ms")] public long ElapsedFakeMs { get; init; }
    [JsonPropertyName("elapsed_real_ms")] public long ElapsedRealMs { get; init; }
}

public sealed record VanishedEvent : ChronoEvent
{
    [JsonPropertyName("pid")] public int Pid { get; init; }
    [JsonPropertyName("reason_key")] public required string ReasonKey { get; init; }
    [JsonPropertyName("lived_ms")] public long LivedMs { get; init; }
}

public sealed record SessionVerdictEvent : ChronoEvent
{
    [JsonPropertyName("verdict")] public required string Verdict { get; init; }
    [JsonPropertyName("reason_key")] public required string ReasonKey { get; init; }
    [JsonPropertyName("process_count")] public int ProcessCount { get; init; }

    /// <summary>
    /// Warnings about the SESSION rather than about one process (R2-S9). The first of them, a full PID
    /// registry, is precisely about processes that never got a coverage slot - so there is no pid to
    /// hang it on, and a per-process <c>coverage</c> event cannot carry it. Absent in messages from a
    /// core built before the field existed, which deserializes to the empty default.
    /// </summary>
    [JsonPropertyName("warning_keys")] public IReadOnlyList<string> WarningKeys { get; init; } = [];
}

public sealed record EndedEvent : ChronoEvent
{
    [JsonPropertyName("clean")] public bool Clean { get; init; }
    [JsonPropertyName("residue_keys")] public IReadOnlyList<string> ResidueKeys { get; init; } = [];
    [JsonPropertyName("target_exit_code")] public int? TargetExitCode { get; init; }
    [JsonPropertyName("elapsed_real_ms")] public long ElapsedRealMs { get; init; }
    [JsonPropertyName("elapsed_fake_ms")] public long ElapsedFakeMs { get; init; }
    [JsonPropertyName("fake_end_wall")] public string? FakeEndWall { get; init; }
}

public sealed record ErrorEvent : ChronoEvent
{
    [JsonPropertyName("id")] public long? Id { get; init; }
    [JsonPropertyName("code")] public int Code { get; init; }
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("origin")] public required string Origin { get; init; }
}

/// <summary>
/// Parses one NDJSON line into a typed event. Mirrors the core's flat wire shape: a <c>type</c>
/// discriminator with snake_case tags. Malformed JSON throws (the caller records it as a diagnostic and
/// continues); a valid line with an unknown <c>type</c> returns null and is ignored (forward
/// compatibility, docs/08 section 2 - the same rule the core applies to unknown commands/fields).
/// </summary>
public static class EventParser
{
    public static ChronoEvent? Parse(string line)
    {
        using var doc = JsonDocument.Parse(line);
        if (doc.RootElement.ValueKind != JsonValueKind.Object
            || !doc.RootElement.TryGetProperty("type", out var typeProp)
            || typeProp.ValueKind != JsonValueKind.String)
        {
            return null;
        }

        return typeProp.GetString() switch
        {
            "ready" => Deserialize<ReadyEvent>(line),
            "coverage" => Deserialize<CoverageEvent>(line),
            "verdict" => Deserialize<VerdictEvent>(line),
            "ack" => Deserialize<AckEvent>(line),
            "state" => Deserialize<StateEvent>(line),
            "vanished" => Deserialize<VanishedEvent>(line),
            "session_verdict" => Deserialize<SessionVerdictEvent>(line),
            "ended" => Deserialize<EndedEvent>(line),
            "error" => Deserialize<ErrorEvent>(line),
            _ => null,
        };
    }

    private static ChronoEvent Deserialize<T>(string line) where T : ChronoEvent
        => JsonSerializer.Deserialize<T>(line, ProtocolJson.Options)
           ?? throw new JsonException($"null deserializing {typeof(T).Name}");
}

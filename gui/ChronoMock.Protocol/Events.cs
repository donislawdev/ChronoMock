using System.Text.Json;
using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>
/// Base for events the core sends on its stdout (mirrors <c>chrono_proto::Event</c>). The core emits
/// stable translation KEYS and structured data, never translated prose (untouchable rules 15/16) - the
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
    // u32 on the wire (the core sends a Windows process id), so it is u32 here: an int could not hold a
    // high pid, and the mismatch would fail the whole event's deserialization - dropping a process's
    // evidence entirely, with only a diagnostic line to show for it (R2-N20, rules 4 and 6).
    [JsonPropertyName("pid")] public uint Pid { get; init; }

    /// <summary>What <c>Pid</c> names: <see cref="UnitProcess"/> (an operating-system process the hook is
    /// inside) or <see cref="UnitContext"/> (a JS context reached over the DevTools protocol, and the
    /// number is its index in this session). Two namespaces a reader must not confuse: pid 8 and context 8
    /// are different units, and one native session can hold both (docs/09 section 12.4). Absent in
    /// messages from an older core, which came from processes.</summary>
    [JsonPropertyName("kind")] public string Kind { get; init; } = UnitProcess;

    public const string UnitProcess = "process";
    public const string UnitContext = "context";

    [JsonPropertyName("covered")] public IReadOnlyList<CoveredChannel> Covered { get; init; } = [];
    [JsonPropertyName("observed")] public IReadOnlyList<CoveredChannel> Observed { get; init; } = [];
    [JsonPropertyName("uncovered")] public IReadOnlyList<string> Uncovered { get; init; } = [];

    /// <summary>Channels the session meant to WATCH and could not hook. Not the same thing as an
    /// uncovered channel and not a verdict input: a watch that never started says nothing about whether
    /// the substitution took effect. It exists because the alternative was no line at all.</summary>
    [JsonPropertyName("unobserved")] public IReadOnlyList<string> Unobserved { get; init; } = [];

    /// <summary>Channels that came under the fake clock only once their module loaded, by name. Each is also
    /// in Covered or Observed with its count - this says that count is a floor, and names what the warning
    /// coverage.channel_installed_late is about. Absent from a core older than the field, and then empty.</summary>
    [JsonPropertyName("installed_late")] public IReadOnlyList<string> InstalledLate { get; init; } = [];
    [JsonPropertyName("warning_keys")] public IReadOnlyList<string> WarningKeys { get; init; } = [];
}

public sealed record VerdictEvent : ChronoEvent
{
    [JsonPropertyName("id")] public long? Id { get; init; }
    [JsonPropertyName("verdict")] public required string Verdict { get; init; }
    [JsonPropertyName("refuse_start")] public bool RefuseStart { get; init; }
    [JsonPropertyName("reason_key")] public required string ReasonKey { get; init; }

    /// <summary>With <see cref="RefuseStart"/>, the processes of the application the core could not end, so
    /// they still run on the real clock (R4-S5). Empty on every other verdict, and absent from the message
    /// then, or from a core older than the field.</summary>
    [JsonPropertyName("left_running")] public IReadOnlyList<FollowedProcess> LeftRunning { get; init; } = [];

    /// <summary>With <see cref="RefuseStart"/>, whether the core could not look for all of the application's
    /// processes, so some may run on the real clock without being named in <see cref="LeftRunning"/>. False on
    /// every other verdict, and when the field is absent.</summary>
    [JsonPropertyName("family_search_incomplete")] public bool FamilySearchIncomplete { get; init; }
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
    // u32 on the wire (the core sends a Windows process id), so it is u32 here: an int could not hold a
    // high pid, and the mismatch would fail the whole event's deserialization - dropping a process's
    // evidence entirely, with only a diagnostic line to show for it (R2-N20, rules 4 and 6).
    [JsonPropertyName("pid")] public uint Pid { get; init; }
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

    /// <summary>
    /// Processes the family spawned and the hook did not follow into - each ran on the real clock
    /// (SLOWNIK <c>uncoveredChild</c>). Named on this event because none of them has a <c>coverage</c>
    /// event of its own. The core names at most 64 and carries the true total beside them, so the
    /// two can differ and the total is the number to show. Absent in messages from an older core,
    /// which deserializes to empty and zero.
    /// </summary>
    [JsonPropertyName("uncovered_children")] public IReadOnlyList<UncoveredChild> UncoveredChildren { get; init; } = [];
    [JsonPropertyName("uncovered_children_total")] public int UncoveredChildrenTotal { get; init; }

    /// <summary>How many JS contexts the session covered beside its processes - the pages and workers of
    /// an embedded web engine (docs/09). <c>ProcessCount</c> stays the number of processes. Absent in
    /// messages from an older core, which deserializes to zero.</summary>
    [JsonPropertyName("context_count")] public int ContextCount { get; init; }

    /// <summary>The DevTools endpoints the session reached inside the application, one per engine. Empty
    /// for a session that found none, and for a Chromium session, which opened its own.</summary>
    [JsonPropertyName("engines")] public IReadOnlyList<ReachedEngine> Engines { get; init; } = [];

    /// <summary>The processes the session went on for after the target closed (ADR-16) - a launcher's
    /// application, or a helper that kept the session open. Empty when nothing outlived the target, for a
    /// session that ended while the target ran, and in messages from an older core.</summary>
    [JsonPropertyName("followed")] public IReadOnlyList<FollowedProcess> Followed { get; init; } = [];

    /// <summary>Processes outside the session running the same program file as a windowed process of it,
    /// found when the application ended on its own without ever showing a window: a single-instance
    /// application most likely passed its work to one of them, and it runs on the real clock (ADR-4,
    /// 2026-11-11). Empty for every other session, and in messages from an older core.</summary>
    [JsonPropertyName("copies_outside")] public IReadOnlyList<FollowedProcess> CopiesOutside { get; init; } = [];

    /// <summary>How long the machine slept during the session, in milliseconds (R5-D34) - the session clock
    /// stood still meanwhile. Zero when it did not sleep, and in messages from an older core.</summary>
    [JsonPropertyName("host_slept_ms")] public long HostSleptMs { get; init; }

    /// <summary>What the browser of a Chromium session calls itself (R5-D44), from its version answer. Absent
    /// for a native session, for a browser that did not say, and in messages from an older core.</summary>
    [JsonPropertyName("browser")] public string? Browser { get; init; }
}

/// <summary>
/// The core noticed that the machine slept (R5-D34, docs/01 section 3.0a): the session clock stood still
/// for <see cref="SleptMs"/>, as during a pause, and the session has slept <see cref="TotalSleptMs"/> in all
/// so far. Sent when the core notices, which is on its first heartbeat after the machine wakes.
/// </summary>
public sealed record HostSleptEvent : ChronoEvent
{
    [JsonPropertyName("slept_ms")] public long SleptMs { get; init; }
    [JsonPropertyName("total_slept_ms")] public long TotalSleptMs { get; init; }
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
/// continues) - a valid line with an unknown <c>type</c> returns null and is ignored (forward
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
            "host_slept" => Deserialize<HostSleptEvent>(line),
            _ => null,
        };
    }

    private static ChronoEvent Deserialize<T>(string line) where T : ChronoEvent
        => JsonSerializer.Deserialize<T>(line, ProtocolJson.Options)
           ?? throw new JsonException($"null deserializing {typeof(T).Name}");
}

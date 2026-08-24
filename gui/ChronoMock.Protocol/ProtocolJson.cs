using System.Text.Json;
using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>
/// Wire constants and the shared JSON options for the machine protocol (ADR-6, docs/08).
/// Mirrors the authoritative Rust contract in <c>crates/proto</c>, not docs/08 (which drifts:
/// no session_id, no standalone warning event, empty capabilities today).
/// </summary>
public static class ProtocolJson
{
    /// <summary>Wire protocol version carried in every message (mirrors <c>chrono_proto::PROTOCOL_VERSION</c>).</summary>
    public const int ProtocolVersion = 1;

    /// <summary>
    /// Shared options: omit null fields on write (the core omits <c>None</c> via serde
    /// <c>skip_serializing_if</c>), and ignore unknown fields on read (additive evolution, docs/08 section 2).
    /// </summary>
    public static JsonSerializerOptions Options { get; } = new()
    {
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };
}

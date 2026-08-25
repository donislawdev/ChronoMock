using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.Json.Serialization;

namespace ChronoMock.App;

/// <summary>
/// One past session, as the local history file records it (docs/04 section 6). History DESCRIBES what
/// happened, so - unlike a preset or the paste-into-ticket summary - it DOES carry the target path. It is
/// local-only, never an exchange format and never exported (docs/04 row 27). Repeating a record fills the
/// setup form and never starts a session (untouchable rule 7, docs/04 section 6).
/// </summary>
public sealed record SessionRecord
{
    [JsonPropertyName("target_path")] public required string TargetPath { get; init; }

    [JsonPropertyName("moment_local")] public required string MomentLocal { get; init; }

    [JsonPropertyName("tz_bias_min")] public int TzBiasMin { get; init; }

    [JsonPropertyName("mode")] public required string Mode { get; init; }

    [JsonPropertyName("multiplier")] public long? Multiplier { get; init; }

    [JsonPropertyName("verdict")] public required string Verdict { get; init; }

    [JsonPropertyName("ended_at_utc")] public required string EndedAtUtc { get; init; }

    /// <summary>The target's file name for display; the full path stays in <see cref="TargetPath"/>.</summary>
    [JsonIgnore] public string TargetName => Path.GetFileName(TargetPath);

    /// <summary>The verdict as a kind, so a row reuses the panel's glyph and colour converters.</summary>
    [JsonIgnore] public VerdictKind VerdictKind => VerdictKinds.Parse(Verdict);
}

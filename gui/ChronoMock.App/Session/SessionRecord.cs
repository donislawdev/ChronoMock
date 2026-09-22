using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.Json.Serialization;

namespace ChronoMock.App;

/// <summary>
/// One past session, as the local history file records it (docs/04 section 6). History DESCRIBES what
/// happened, so - unlike a preset or the paste-into-ticket summary - it DOES carry the target path. It is
/// local-only, never an exchange format and never exported (docs/04 row 27). Repeating a record fills the
/// setup form and never starts a session (untouchable rule 7, docs/04 section 6).
/// <para>
/// 🔴 It has to carry EVERY input that changes what a session does, and for a while it did not. Five of
/// them were missing - the target's arguments, its working folder, and the three switches
/// (scale-duration, scale-QPC, force) - so repeating a session filled four fields and left the other five
/// holding whatever the form happened to have. The result was a third setup that was never run and never
/// recorded, and nothing said so (rule 6). The switches are the expensive half: scale-QPC decides whether
/// a target's elapsed counters move at all.
/// </para>
/// <para>
/// The five are ADDITIVE and optional, so the schema stays 1: a file written before they existed still
/// loads, with the defaults below, and a file written now still loads in a build that predates them
/// (System.Text.Json drops what it does not know). History is local and marked unstable, so this is not
/// an exchange contract - but it evolves like one anyway, because that costs nothing here.
/// </para>
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

    /// <summary>The target's command-line arguments, as the one raw string the form holds (it is split the
    /// same way on every replay, by <c>TargetArguments.Split</c>). Empty when none were given.</summary>
    [JsonPropertyName("target_args")] public string TargetArgs { get; init; } = string.Empty;

    /// <summary>The working folder the target was started in, or empty for the default.</summary>
    [JsonPropertyName("working_folder")] public string WorkingFolder { get; init; } = string.Empty;

    /// <summary>Whether the duration axis was scaled too (the scale-duration opt-in).</summary>
    [JsonPropertyName("scale_duration")] public bool ScaleDuration { get; init; }

    /// <summary>Whether QueryPerformanceCounter was scaled too (the scale-QPC opt-in, ADR-2 reversal).</summary>
    [JsonPropertyName("scale_qpc")] public bool ScaleQpc { get; init; }

    /// <summary>Whether the session was started with force - run on even when the opening verdict says the
    /// substitution did not take effect.</summary>
    [JsonPropertyName("force")] public bool Force { get; init; }

    /// <summary>Whether the web pages inside the application were reached through its embedded web
    /// engine (docs/09). A record from before this field existed reads as the default, on.</summary>
    [JsonPropertyName("embedded")] public bool Embedded { get; init; } = true;

    /// <summary>The target's file name for display - the full path stays in <see cref="TargetPath"/>.</summary>
    [JsonIgnore] public string TargetName => Path.GetFileName(TargetPath);

    /// <summary>The verdict as a kind, so a row reuses the panel's glyph and colour converters.</summary>
    [JsonIgnore] public VerdictKind VerdictKind => VerdictKinds.Parse(Verdict);

    /// <summary>The session zone spelled out ("UTC+02:00"), because a recorded moment without its zone is the
    /// ambiguity untouchable rule 2 exists to remove - the file keeps the bias, the row shows the label.</summary>
    [JsonIgnore] public string ZoneText => ZoneLabel.FromBiasMinutes(TzBiasMin);

    /// <summary>The recorded moment as the interface states every moment in full, zone included.</summary>
    [JsonIgnore] public string MomentText => ClockView.FormatMoment(MomentLocal, ZoneText);

    /// <summary>When the session ended, stated the same way. The file keeps UTC and the row says so - a
    /// conversion to the reader's own clock is a later decision, and until then the zone is never implied.</summary>
    [JsonIgnore] public string EndedText => ClockView.FormatMoment(EndedAtUtc.TrimEnd('Z'), "UTC");
}

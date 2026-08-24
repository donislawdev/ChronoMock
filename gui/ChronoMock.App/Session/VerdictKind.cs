namespace ChronoMock.App;

/// <summary>
/// The verifier's verdict, as the panel shows it (chrono-mock 7.1). The core sends the verdict as a string
/// on the wire; this enum drives the indicator's glyph and colour, while the label and reason stay as
/// translation keys (untouchable rules 15/16).
/// </summary>
public enum VerdictKind
{
    /// <summary>No verdict yet.</summary>
    Unknown,

    /// <summary>The swap covered the channels - the session is trustworthy.</summary>
    Works,

    /// <summary>Some channels were covered, some not - the panel must show which, and what it means.</summary>
    Partial,

    /// <summary>The swap did not cover the key channels - the session was refused as unreliable.</summary>
    Fails,

    /// <summary>Coverage could not be determined - honest "unproven", never a faked "works".</summary>
    Undetermined,
}

/// <summary>Maps between the wire verdict string and <see cref="VerdictKind"/>, and to its label key.</summary>
public static class VerdictKinds
{
    public static VerdictKind Parse(string verdict) => verdict switch
    {
        "works" => VerdictKind.Works,
        "partial" => VerdictKind.Partial,
        "fails" => VerdictKind.Fails,
        "undetermined" => VerdictKind.Undetermined,
        // An unrecognised verdict is treated as "unproven", never as working (untouchable rule 4).
        _ => VerdictKind.Undetermined,
    };

    public static string LabelKey(VerdictKind kind) => kind switch
    {
        VerdictKind.Works => "verdict.works",
        VerdictKind.Partial => "verdict.partial",
        VerdictKind.Fails => "verdict.fails",
        VerdictKind.Undetermined => "verdict.undetermined",
        _ => "verdict.unknown",
    };

    /// <summary>The one-sentence "what this means for the test" key, empty for a clean "works".</summary>
    public static string MeaningKey(VerdictKind kind) => kind switch
    {
        VerdictKind.Partial => "verdict.partial.meaning",
        VerdictKind.Fails => "verdict.fails.meaning",
        VerdictKind.Undetermined => "verdict.undetermined.meaning",
        _ => string.Empty,
    };
}

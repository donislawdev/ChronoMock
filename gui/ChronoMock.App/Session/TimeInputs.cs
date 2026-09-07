namespace ChronoMock.App;

/// <summary>A session-zone option: a fixed UTC offset (the session zone carries no DST, rule 2),
/// labelled by its offset so the label never drifts from the bias, plus a translation-key hint naming the
/// market the offset belongs to (shown beside the offset in the dropdown so it is not just a bare number).</summary>
/// <param name="IsHost">True for the one option that means "say nothing about the zone": the engine then
/// falls back to the host's zone, which is what the calculator did implicitly before this option existed.
/// Kept as a flag rather than a magic bias, because the point is the ABSENCE of <c>--zone</c> on the
/// command line, not a particular offset - an explicit pick that happens to equal the host's offset is
/// still an explicit pick.</param>
public sealed record ZoneOption(int BiasMinutes, string Label, string HintKey, bool IsHost = false);

/// <summary>A time-mode option: flowing, frozen, or an xN multiplier, named by a translation key.</summary>
public sealed record ModeOption(string LabelKey, string Mode, long? Multiplier);

/// <summary>
/// The fixed input catalogs - closed lists, not free axes (zasady/13 section 2.3). Zones cover the MVP
/// markets (US + PL) plus UTC - modes are flowing, frozen, and the xN presets from chrono-mock 7.1.
/// </summary>
public static class TimeInputs
{
    public static IReadOnlyList<ZoneOption> Zones { get; } =
    [
        Zone(0, "zone.utc"),
        Zone(-60, "zone.pl_standard"),  // UTC+01:00 - Poland, standard time
        Zone(-120, "zone.pl_summer"),   // UTC+02:00 - Poland, summer time
        Zone(300, "zone.us_eastern"),   // UTC-05:00
        Zone(360, "zone.us_central"),   // UTC-06:00
        Zone(420, "zone.us_mountain"),  // UTC-07:00
        Zone(480, "zone.us_pacific"),   // UTC-08:00
    ];

    public static IReadOnlyList<ModeOption> Modes { get; } =
    [
        new("mode.flow", "flow", null),
        new("mode.frozen", "frozen", null),
        new("mode.x10", "multiplier", 10),
        new("mode.x60", "multiplier", 60),
        new("mode.x1440", "multiplier", 1440),
    ];

    /// <summary>
    /// The zone catalogue for the CALCULATOR's start point: the host entry first, then the same closed list
    /// the substitution panel offers. The host entry exists so the default keeps today's behaviour exactly -
    /// the calculator has never sent <c>--zone</c>, so its base has always been read in the host's zone, and
    /// defaulting to any fixed offset would silently change every result for anyone not sitting on it.
    /// Naming the host's offset in the label is the rule-2 half: what used to be implicit is now on screen.
    /// <para>
    /// Built per call rather than cached, so a caller that rebuilds it after a DST change sees the new host
    /// offset. A list already bound to a control keeps the label it was built with - the RESULT stays right
    /// either way, because the host entry sends no offset and the engine reads the host's zone itself.
    /// </para>
    /// </summary>
    public static IReadOnlyList<ZoneOption> CalcZones() => [HostZone(), .. Zones];

    /// <summary>The host's current UTC offset as a zone option that sends no <c>--zone</c>. Windows bias
    /// convention (UTC = local + bias), so the bias is the negation of the offset - the same arithmetic
    /// <see cref="ZoneLabel"/> undoes for display, kept in one direction here so label and bias cannot
    /// disagree (untouchable rule 2).</summary>
    private static ZoneOption HostZone()
    {
        int bias = -(int)TimeZoneInfo.Local.GetUtcOffset(DateTimeOffset.Now).TotalMinutes;
        return new(bias, ZoneLabel.FromBiasMinutes(bias), "zone.host", IsHost: true);
    }

    private static ZoneOption Zone(int biasMinutes, string hintKey) =>
        new(biasMinutes, ZoneLabel.FromBiasMinutes(biasMinutes), hintKey);
}

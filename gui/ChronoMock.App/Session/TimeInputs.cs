namespace ChronoMock.App;

/// <summary>A session-zone option: a fixed UTC offset (the session zone carries no DST, rule 2),
/// labelled by its offset so the label never drifts from the bias, plus a translation-key hint naming the
/// market the offset belongs to (shown beside the offset in the dropdown so it is not just a bare number).</summary>
public sealed record ZoneOption(int BiasMinutes, string Label, string HintKey);

/// <summary>A time-mode option: flowing, frozen, or an xN multiplier, named by a translation key.</summary>
public sealed record ModeOption(string LabelKey, string Mode, long? Multiplier);

/// <summary>
/// The fixed input catalogs - closed lists, not free axes (zasady/13 section 2.3). Zones cover the MVP
/// markets (US + PL) plus UTC; modes are flowing, frozen, and the xN presets from chrono-mock 7.1.
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

    private static ZoneOption Zone(int biasMinutes, string hintKey) =>
        new(biasMinutes, ZoneLabel.FromBiasMinutes(biasMinutes), hintKey);
}

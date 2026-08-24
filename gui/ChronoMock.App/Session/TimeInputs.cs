namespace ChronoMock.App;

/// <summary>A session-zone option: a fixed UTC offset (the session zone carries no DST, rule 2),
/// labelled by its offset so the label never drifts from the bias.</summary>
public sealed record ZoneOption(int BiasMinutes, string Label);

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
        Zone(0),      // UTC
        Zone(-60),    // UTC+01:00 - Poland, standard time
        Zone(-120),   // UTC+02:00 - Poland, summer time
        Zone(300),    // UTC-05:00 - US Eastern
        Zone(360),    // UTC-06:00 - US Central
        Zone(420),    // UTC-07:00 - US Mountain
        Zone(480),    // UTC-08:00 - US Pacific
    ];

    public static IReadOnlyList<ModeOption> Modes { get; } =
    [
        new("mode.flow", "flow", null),
        new("mode.frozen", "frozen", null),
        new("mode.x10", "multiplier", 10),
        new("mode.x60", "multiplier", 60),
        new("mode.x1440", "multiplier", 1440),
    ];

    private static ZoneOption Zone(int biasMinutes) => new(biasMinutes, ZoneLabel.FromBiasMinutes(biasMinutes));
}

using System.Text.Json.Serialization;

namespace ChronoMock.Protocol;

/// <summary>
/// Parsed <c>chrono calc --json</c> output (schema <c>chronomock.calc/1</c>, docs/08 section 9a). The
/// calculator GUI is a thin client of the same engine the human render uses (ADR-6): this mirrors the
/// CLI's serialized DTOs, so the GUI never re-implements the date engine. Optional fields are nullable
/// and unknown fields are ignored (additive evolution). Exactly one of <see cref="Moment"/> /
/// <see cref="Analysis"/> is present, per the request mode (build/preset vs --analyze).
/// </summary>
public sealed record CalcResult(
    [property: JsonPropertyName("schema")] string Schema,
    [property: JsonPropertyName("moment")] CalcMoment? Moment,
    [property: JsonPropertyName("analysis")] CalcAnalysis? Analysis);

/// <summary>A computed moment: the result plus every output format, metadata, and its landmarks.</summary>
public sealed record CalcMoment(
    [property: JsonPropertyName("iso")] string Iso,
    [property: JsonPropertyName("zone_bias_min")] int ZoneBiasMin,
    [property: JsonPropertyName("base")] string Base,
    [property: JsonPropertyName("steps")] IReadOnlyList<string> Steps,
    [property: JsonPropertyName("formats")] CalcFormats Formats,
    [property: JsonPropertyName("metadata")] CalcMetadata Metadata,
    [property: JsonPropertyName("significance")] IReadOnlyList<string> Significance,
    [property: JsonPropertyName("custom_format")] string? CustomFormat,
    [property: JsonPropertyName("preset")] CalcPreset? Preset);

/// <summary>Every fixed output format at once (docs/02 section 8). Instant-based fields are null outside
/// the representable FILETIME range - never a wrong number.</summary>
public sealed record CalcFormats(
    [property: JsonPropertyName("iso_date")] string IsoDate,
    [property: JsonPropertyName("iso_datetime")] string IsoDatetime,
    [property: JsonPropertyName("us")] string Us,
    [property: JsonPropertyName("pl")] string Pl,
    [property: JsonPropertyName("epoch_seconds")] long? EpochSeconds,
    [property: JsonPropertyName("epoch_millis")] long? EpochMillis,
    [property: JsonPropertyName("filetime")] long? Filetime,
    [property: JsonPropertyName("rfc1123")] string? Rfc1123);

/// <summary>Calendar-independent metadata, plus business-day/holiday which are null without a calendar
/// (<see cref="BusinessDay"/> non-null means a calendar was applied - it disambiguates "not a holiday"
/// from "no calendar").</summary>
public sealed record CalcMetadata(
    [property: JsonPropertyName("weekday")] string Weekday,
    [property: JsonPropertyName("iso_week_year")] long IsoWeekYear,
    [property: JsonPropertyName("iso_week")] int IsoWeek,
    [property: JsonPropertyName("us_week")] int UsWeek,
    [property: JsonPropertyName("day_of_year")] int DayOfYear,
    [property: JsonPropertyName("quarter")] int Quarter,
    [property: JsonPropertyName("is_leap_year")] bool IsLeapYear,
    [property: JsonPropertyName("days_from_today")] long DaysFromToday,
    [property: JsonPropertyName("business_day")] bool? BusinessDay,
    [property: JsonPropertyName("holiday")] string? Holiday);

/// <summary>The preset's authored framing when the moment came from one (docs/04 4.2).</summary>
public sealed record CalcPreset(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("explains")] string Explains);

/// <summary>Reverse analysis of a pasted date (7.3): the reading(s) it resolves to, both shown when the
/// numeric order is ambiguous (04/08 is April 8 in the US, August 4 in Poland).</summary>
public sealed record CalcAnalysis(
    [property: JsonPropertyName("input")] string Input,
    [property: JsonPropertyName("ambiguous")] bool Ambiguous,
    [property: JsonPropertyName("readings")] IReadOnlyList<CalcReading> Readings);

/// <summary>One reading of an analyzed date: its interpretation, the resolved date, and its landmarks.</summary>
public sealed record CalcReading(
    [property: JsonPropertyName("reading")] string Reading,
    [property: JsonPropertyName("iso")] string Iso,
    [property: JsonPropertyName("significance")] IReadOnlyList<string> Significance,
    [property: JsonPropertyName("metadata")] CalcMetadata Metadata);

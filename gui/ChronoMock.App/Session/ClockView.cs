using System.Globalization;
using ChronoMock.App.Localization;

namespace ChronoMock.App;

/// <summary>
/// One clock tile's live data: which clock it is (a fixed translation key), and the wall time and zone it
/// currently reads. Both the fake and the real clock render from the SAME tile template, so they cannot
/// drift apart (zasady/13 section 2.4 - consistency comes from a default, not from discipline).
/// <para>
/// Every reading carries its zone explicitly (untouchable rule 2). A wall time without its zone is exactly
/// the silent "N hours short" bug the rule forbids, so <see cref="Zone"/> is never optional.
/// </para>
/// </summary>
public sealed class ClockView : ObservableObject
{
    // A dash reads as "no reading yet" before the first heartbeat - honest, not a faked time.
    private const string Placeholder = "-";

    private string _wall = Placeholder;
    private string _date = Placeholder;
    private string _time = string.Empty;
    private string _zone = string.Empty;
    private string _elapsed = string.Empty;

    public ClockView(string roleKey) => RoleKey = roleKey;

    /// <summary>Stable translation key naming this clock ("clock.fake" / "clock.real").</summary>
    public string RoleKey { get; }

    /// <summary>
    /// The wall-clock text as the core reports it (ISO "date T time", session-zone semantics). Setting it
    /// splits into <see cref="Date"/> and <see cref="Time"/>, which the tile shows on two deliberate lines -
    /// a full ISO timestamp at clock size would otherwise wrap mid-value in a narrow card (zasady/13 2.1).
    /// </summary>
    public string Wall
    {
        get => _wall;
        set
        {
            if (!Set(ref _wall, value))
            {
                return;
            }

            var t = value.IndexOf('T', StringComparison.Ordinal);
            Date = t >= 0 ? value[..t] : value;
            Time = t >= 0 ? value[(t + 1)..] : string.Empty;
        }
    }

    /// <summary>The date half of <see cref="Wall"/> (the whole string if there is no time part).</summary>
    public string Date { get => _date; private set => Set(ref _date, value); }

    /// <summary>The time half of <see cref="Wall"/> (empty if there is no time part).</summary>
    public string Time { get => _time; private set => Set(ref _time, value); }

    /// <summary>The session zone this reading is expressed in, e.g. "UTC+02:00".</summary>
    public string Zone { get => _zone; set => Set(ref _zone, value); }

    /// <summary>
    /// How much time has PASSED on this clock since the session began, as "h:mm:ss" or "d.hh:mm:ss".
    /// </summary>
    /// <remarks>
    /// 🔴 THE MOST DIRECT PROOF THE TOOL IS DOING ANYTHING, and it reached the copied report and never the
    /// screen. Two clocks showing different dates prove a substitution happened. The pair of durations
    /// beside them - two minutes here, two days there - is the MULTIPLIER, stated as a fact rather than as
    /// a number somebody has to trust.
    ///
    /// It is time that PASSED, not the difference between two readings: the core banks fake time per rate
    /// segment (mech::fake_elapsed_ticks), so changing the rate mid-session keeps it honest, and a jump
    /// does not inflate it - a jump moves a clock, it does not make time pass. That is also why it can be
    /// shown next to a wall reading without the two contradicting each other.
    ///
    /// Empty until the first heartbeat, so the tile simply has one fewer line rather than printing a zero
    /// that looks like a measurement.
    /// </remarks>
    public string Elapsed
    {
        get => _elapsed;
        set
        {
            if (Set(ref _elapsed, value))
            {
                RaisePropertyChanged(nameof(HasElapsed));
            }
        }
    }

    /// <summary>True once there is a duration to show. The tile's line disappears rather than reading
    /// "elapsed" beside nothing.</summary>
    public bool HasElapsed => _elapsed.Length > 0;

    /// <summary>
    /// A duration as the clocks above it are written: digits and colons, no words.
    /// </summary>
    /// <remarks>
    /// 🔴 NO HUMANISED UNITS, on purpose. "1 h 2 min" needs a unit vocabulary in every language and Polish
    /// needs three plural forms for each of them, which is a translation surface bought for nothing here -
    /// this sits directly under a clock reading "03:14:07", so the same notation is the one a reader has
    /// already parsed. Days are prefixed rather than folded into hours, because a fake session at ×1440
    /// reaches "60.00:00:00" within an hour and "1440:00:00" would be a number nobody can read.
    /// </remarks>
    /// <summary>
    /// One formatting for every moment the interface states in full: the contract sentence above Start, the
    /// facts of a finished session, a row of the history. Weekday, long month, seconds and the zone spelled
    /// out - the weekday because it is often the whole point of the test, the zone because a bare moment is
    /// the ambiguity untouchable rule 2 exists to remove. Empty for anything that is not a canonical moment,
    /// the "-" placeholder before the first heartbeat for one.
    /// </summary>
    public static string FormatMoment(string canonical, string zoneLabel)
    {
        if (!DateTime.TryParseExact(
                canonical,
                "yyyy-MM-dd'T'HH:mm:ss",
                CultureInfo.InvariantCulture,
                DateTimeStyles.None,
                out var moment))
        {
            return string.Empty;
        }

        return string.Create(
            LocalizationService.CurrentFormatCulture,
            $"{moment:dddd, d MMMM yyyy, HH:mm:ss} ({zoneLabel})");
    }

    public static string FormatDuration(long milliseconds)
    {
        var span = TimeSpan.FromMilliseconds(Math.Max(milliseconds, 0));
        return span.Days > 0
            ? span.ToString(@"d\.hh\:mm\:ss", CultureInfo.InvariantCulture)
            : span.ToString(@"h\:mm\:ss", CultureInfo.InvariantCulture);
    }
}

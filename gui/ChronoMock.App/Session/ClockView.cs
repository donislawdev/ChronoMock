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
}

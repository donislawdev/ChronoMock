using System.Globalization;

namespace ChronoMock.App;

/// <summary>
/// The editable moment: a date and an optional time, held as raw text so the field shows exactly what the
/// user typed, and composed into a canonical yyyy-MM-ddTHH:mm:ss by <see cref="MomentParse"/> (always
/// InvariantCulture, so the OS locale never changes its meaning, rule 2). The shared MomentInput control
/// binds to one of these, so the substitution panel and the calculator base get the same behaviour.
/// </summary>
public sealed class MomentField : ObservableObject
{
    private string _dateText = string.Empty;
    private string _timeText = string.Empty;
    private MomentPart _errorPart = MomentPart.None;

    /// <summary>The ISO date text (yyyy-MM-dd). Typing here or picking from the calendar both land here.</summary>
    public string DateText
    {
        get => _dateText;
        set
        {
            if (Set(ref _dateText, value))
            {
                Recompute();
            }
        }
    }

    /// <summary>The 24-hour time text (HH:mm or HH:mm:ss). Empty means midnight.</summary>
    public string TimeText
    {
        get => _timeText;
        set
        {
            if (Set(ref _timeText, value))
            {
                Recompute();
            }
        }
    }

    /// <summary>The calendar popup binds here: a picked day writes the ISO date text, and a typed valid ISO
    /// date moves the calendar. Culture-invariant either way (rule 2).</summary>
    public DateTime? SelectedDate
    {
        get => DateTime.TryParseExact(
            _dateText, "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var d)
            ? d
            : null;
        set
        {
            if (value is { } picked)
            {
                DateText = picked.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
            }
        }
    }

    /// <summary>True when the date and time compose into a well-formed moment. The core still does the deep
    /// validation (DST gap, range) and reports its own reason (docs/08 section 5).</summary>
    public bool IsValid { get; private set; }

    /// <summary>The composed yyyy-MM-ddTHH:mm:ss, or empty when invalid.</summary>
    public string Canonical { get; private set; } = string.Empty;

    private string ErrorKey { get; set; } = string.Empty;

    public bool HasDateError => !IsValid && _errorPart == MomentPart.Date;

    public bool HasTimeError => !IsValid && _errorPart == MomentPart.Time;

    /// <summary>The date-field error message key, or empty when the date part is fine.</summary>
    public string DateErrorKey => HasDateError ? ErrorKey : string.Empty;

    /// <summary>The time-field error message key, or empty when the time part is fine.</summary>
    public string TimeErrorKey => HasTimeError ? ErrorKey : string.Empty;

    /// <summary>True when the composed moment is malformed (either part, or an empty date). Consumers show
    /// <see cref="ActiveErrorKey"/> BELOW the whole input row rather than inside the control, so a message
    /// appearing never changes the row height or knocks the inputs out of line with their neighbours.</summary>
    public bool HasError => !IsValid;

    /// <summary>The active error message key (the failing part's message), empty when the moment is valid.</summary>
    public string ActiveErrorKey => IsValid ? string.Empty : ErrorKey;

    /// <summary>Raised after every recompute, so an owner can re-raise its own derived state (e.g. CanStart).</summary>
    public event EventHandler? Changed;

    /// <summary>Load a canonical moment (from history or the calculator bridge) into the two fields.</summary>
    public void LoadCanonical(string? canonical)
    {
        var (date, time) = MomentParse.Split(canonical);
        Fill(date, time);
    }

    /// <summary>Fill the field with midnight today in the SESSION zone (a fixed offset, rule 2 - relative to
    /// the session zone, never the OS clock's local time). The bias comes from the panel's selected zone.</summary>
    public void SetToday(int biasMinutes) => SetToday(biasMinutes, DateTime.UtcNow);

    /// <summary>Fill the field with the current wall time in the SESSION zone (rule 2). The bias comes from
    /// the panel's selected zone; the OS locale never enters, so a Polish box and a US VM produce the same text.</summary>
    public void SetNow(int biasMinutes) => SetNow(biasMinutes, DateTime.UtcNow);

    // Testable cores: "now in the session zone" is UTC shifted by the zone offset (UTC = local + bias, so
    // local = UTC - bias). InvariantCulture throughout, so the result never follows the OS date format. The
    // injected clock keeps the tests deterministic across the midnight and second boundaries.
    internal void SetToday(int biasMinutes, DateTime utcNow)
    {
        var local = utcNow.AddMinutes(-biasMinutes);
        Fill(local.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture), string.Empty);
    }

    internal void SetNow(int biasMinutes, DateTime utcNow)
    {
        var local = utcNow.AddMinutes(-biasMinutes);
        // Keep seconds only when non-zero, matching Split's tidy convention (a plain HH:mm most of the time).
        var time = local.Second == 0
            ? local.ToString("HH:mm", CultureInfo.InvariantCulture)
            : local.ToString("HH:mm:ss", CultureInfo.InvariantCulture);
        Fill(local.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture), time);
    }

    // Set both fields at once and recompute once, raising the two text properties so a bound control
    // refreshes. Shared by LoadCanonical, SetToday and SetNow.
    private void Fill(string date, string time)
    {
        _dateText = date;
        _timeText = time;
        RaisePropertyChanged(nameof(DateText));
        RaisePropertyChanged(nameof(TimeText));
        Recompute();
    }

    private void Recompute()
    {
        var result = MomentParse.Compose(_dateText, _timeText);
        IsValid = result.Ok;
        Canonical = result.Canonical;
        _errorPart = result.ErrorPart;
        ErrorKey = result.ErrorKey;

        RaisePropertyChanged(nameof(IsValid));
        RaisePropertyChanged(nameof(Canonical));
        RaisePropertyChanged(nameof(HasDateError));
        RaisePropertyChanged(nameof(HasTimeError));
        RaisePropertyChanged(nameof(DateErrorKey));
        RaisePropertyChanged(nameof(TimeErrorKey));
        RaisePropertyChanged(nameof(HasError));
        RaisePropertyChanged(nameof(ActiveErrorKey));
        RaisePropertyChanged(nameof(SelectedDate));
        Changed?.Invoke(this, EventArgs.Empty);
    }
}

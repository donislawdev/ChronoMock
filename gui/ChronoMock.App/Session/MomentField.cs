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

    /// <summary>Raised after every recompute, so an owner can re-raise its own derived state (e.g. CanStart).</summary>
    public event EventHandler? Changed;

    /// <summary>Load a canonical moment (from history or the calculator bridge) into the two fields.</summary>
    public void LoadCanonical(string? canonical)
    {
        var (date, time) = MomentParse.Split(canonical);
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
        RaisePropertyChanged(nameof(SelectedDate));
        Changed?.Invoke(this, EventArgs.Empty);
    }
}

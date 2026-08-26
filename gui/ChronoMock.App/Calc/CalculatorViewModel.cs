using System.Collections.ObjectModel;
using System.ComponentModel;
using ChronoMock.Protocol;

namespace ChronoMock.App.Calc;

/// <summary>Where an expression starts (mirrors <c>calc::Base</c>): today / now / an explicit date.</summary>
public enum BaseKind
{
    Today,
    Now,
    Specific,
}

/// <summary>Which kind of step a builder row is (mirrors calc's typed steps). Slices G3c-1..3 wire
/// <see cref="Shift"/>, <see cref="Snap"/>, <see cref="Nearest"/> and <see cref="SetTime"/>; zone follows.</summary>
public enum StepKind
{
    Shift,
    Snap,
    Nearest,
    SetTime,
}

/// <summary>A base option for the dropdown: the kind plus its translation key (rule 15/16 - the view
/// renders the key, the view model stays language-agnostic).</summary>
public sealed record BaseKindOption(BaseKind Kind, string LabelKey);

/// <summary>A step-kind option for a row's kind dropdown: the kind plus its translation key.</summary>
public sealed record StepKindOption(StepKind Kind, string LabelKey);

/// <summary>A shift unit option: the CLI token (<c>y</c>, <c>mo</c>, <c>bd</c>, ...) plus its translation key.</summary>
public sealed record UnitOption(string Token, string LabelKey);

/// <summary>A snap-target option: the CLI token (<c>som</c>/<c>eom</c>/<c>soq</c>/<c>eoq</c>/<c>soy</c>/<c>eoy</c>)
/// plus its translation key (mirrors <c>parse_snap</c>).</summary>
public sealed record SnapTargetOption(string Token, string LabelKey);

/// <summary>A nearest-target option: the CLI token (<c>nbd</c>/<c>pbd</c>) plus its translation key
/// (mirrors <c>parse_nearest</c>). Needs a calendar - without one the engine returns a calendar error.</summary>
public sealed record NearestTargetOption(string Token, string LabelKey);

/// <summary>A calendar option: the id passed to <c>--calendar</c> (null = omit it) plus its translation key.</summary>
public sealed record CalendarOption(string? Id, string LabelKey);

/// <summary>One output-format row: a technical format label (not translated, like the coverage channel
/// names) and the value the engine produced (or the out-of-range marker).</summary>
public sealed record FormatRow(string Label, string Value);

/// <summary>
/// One step in the builder. A step carries a kind (shift / snap / ...) and the fields that kind needs;
/// only the selected kind's editor is visible, and <see cref="ToArgs"/> emits that kind's calc flag. Editing
/// any field raises PropertyChanged, which the parent turns into a live recompute ("result after each step",
/// 7.3). One unified view model (not a class per kind) keeps the hand-rolled INPC style and lets the kind
/// switch in place without swapping the item in the collection.
/// </summary>
public sealed class StepViewModel : ObservableObject
{
    public static IReadOnlyList<string> Signs { get; } = ["+", "-"];

    private StepKindOption _kind;
    private string _sign = "+";
    private string _amount = "1";
    private UnitOption _unit;
    private SnapTargetOption _snapTarget;
    private NearestTargetOption _nearestTarget;
    private string _setTimeText = "23:59:59";

    public StepViewModel(
        IReadOnlyList<StepKindOption> kinds,
        IReadOnlyList<UnitOption> units,
        IReadOnlyList<SnapTargetOption> snapTargets,
        IReadOnlyList<NearestTargetOption> nearestTargets)
    {
        Kinds = kinds;
        Units = units;
        SnapTargets = snapTargets;
        NearestTargets = nearestTargets;
        _kind = kinds[0];                   // shift - the common default
        _unit = units[3];                   // days
        _snapTarget = snapTargets[1];       // end-of-month
        _nearestTarget = nearestTargets[0]; // next business day
    }

    public IReadOnlyList<StepKindOption> Kinds { get; }
    public IReadOnlyList<UnitOption> Units { get; }
    public IReadOnlyList<SnapTargetOption> SnapTargets { get; }
    public IReadOnlyList<NearestTargetOption> NearestTargets { get; }

    public StepKindOption SelectedKind
    {
        get => _kind;
        set
        {
            if (Set(ref _kind, value))
            {
                RaisePropertyChanged(nameof(IsShift));
                RaisePropertyChanged(nameof(IsSnap));
                RaisePropertyChanged(nameof(IsNearest));
                RaisePropertyChanged(nameof(IsSetTime));
            }
        }
    }

    /// <summary>Whether the shift editor applies (the row's kind is Shift).</summary>
    public bool IsShift => _kind.Kind == StepKind.Shift;

    /// <summary>Whether the snap editor applies (the row's kind is Snap).</summary>
    public bool IsSnap => _kind.Kind == StepKind.Snap;

    /// <summary>Whether the nearest editor applies (the row's kind is Nearest).</summary>
    public bool IsNearest => _kind.Kind == StepKind.Nearest;

    /// <summary>Whether the set-time editor applies (the row's kind is SetTime).</summary>
    public bool IsSetTime => _kind.Kind == StepKind.SetTime;

    // Shift fields.
    public string Sign { get => _sign; set => Set(ref _sign, value); }
    public string Amount { get => _amount; set => Set(ref _amount, value); }
    public UnitOption Unit { get => _unit; set => Set(ref _unit, value); }

    // Snap field.
    public SnapTargetOption SnapTarget { get => _snapTarget; set => Set(ref _snapTarget, value); }

    // Nearest field.
    public NearestTargetOption NearestTarget { get => _nearestTarget; set => Set(ref _nearestTarget, value); }

    // Set-time field: a wall-clock time HH:MM:SS. Range is validated by the engine (BadSetTime), so an
    // out-of-range time surfaces as an honest result error rather than being clamped here.
    public string SetTimeText { get => _setTimeText; set => Set(ref _setTimeText, value); }

    /// <summary>The calc flag pair for this step, e.g. <c>--shift +18y</c>, <c>--snap eoq</c>,
    /// <c>--nearest nbd</c> or <c>--set-time 23:59:59</c>.</summary>
    public IReadOnlyList<string> ToArgs() => _kind.Kind switch
    {
        StepKind.Shift => ["--shift", $"{Sign}{Amount.Trim()}{Unit.Token}"],
        StepKind.Snap => ["--snap", _snapTarget.Token],
        StepKind.Nearest => ["--nearest", _nearestTarget.Token],
        StepKind.SetTime => ["--set-time", _setTimeText.Trim()],
        _ => [],
    };
}

/// <summary>
/// The date-calculator screen's live state (Stage 4, GUI slice G3b/G3c). Holds the builder inputs (base,
/// steps, calendar) and the result of evaluating them through <see cref="CalcClient"/> - the same engine
/// the CLI and substitution core use (ADR-6). Any input change recomputes; overlapping computes cancel the
/// previous one. Manual INPC, no MVVM package (gui-and-cli-constraints), like the session panel.
/// </summary>
public sealed class CalculatorViewModel : ObservableObject
{
    private readonly CalcClient _client;

    private BaseKindOption _baseKind;
    private string _baseText = "2026-01-01T00:00:00";
    private CalendarOption _calendar;
    private string _resultWeekday = string.Empty;
    private string _resultDate = "-";
    private string _resultTime = string.Empty;
    private string _resultZone = string.Empty;
    private string _metadataLine = string.Empty;
    private string _error = string.Empty;
    private bool _hasError;
    private bool _hasResult;
    private bool _hasSignificance;
    private bool _computedOnce;
    private CancellationTokenSource? _cts;

    public CalculatorViewModel(CalcClient client)
    {
        _client = client ?? throw new ArgumentNullException(nameof(client));

        BaseKinds =
        [
            new BaseKindOption(BaseKind.Today, "calc.base.today"),
            new BaseKindOption(BaseKind.Now, "calc.base.now"),
            new BaseKindOption(BaseKind.Specific, "calc.base.specific"),
        ];
        _baseKind = BaseKinds[0];

        StepKinds =
        [
            new StepKindOption(StepKind.Shift, "calc.kind_shift"),
            new StepKindOption(StepKind.Snap, "calc.kind_snap"),
            new StepKindOption(StepKind.Nearest, "calc.kind_nearest"),
            new StepKindOption(StepKind.SetTime, "calc.kind_settime"),
        ];

        Units =
        [
            new UnitOption("s", "calc.unit.seconds"),
            new UnitOption("m", "calc.unit.minutes"),
            new UnitOption("h", "calc.unit.hours"),
            new UnitOption("d", "calc.unit.days"),
            new UnitOption("w", "calc.unit.weeks"),
            new UnitOption("mo", "calc.unit.months"),
            new UnitOption("q", "calc.unit.quarters"),
            new UnitOption("y", "calc.unit.years"),
            new UnitOption("bd", "calc.unit.business_days"),
        ];

        SnapTargets =
        [
            new SnapTargetOption("som", "calc.snap.som"),
            new SnapTargetOption("eom", "calc.snap.eom"),
            new SnapTargetOption("soq", "calc.snap.soq"),
            new SnapTargetOption("eoq", "calc.snap.eoq"),
            new SnapTargetOption("soy", "calc.snap.soy"),
            new SnapTargetOption("eoy", "calc.snap.eoy"),
        ];

        NearestTargets =
        [
            new NearestTargetOption("nbd", "calc.nearest.nbd"),
            new NearestTargetOption("pbd", "calc.nearest.pbd"),
        ];

        Calendars =
        [
            new CalendarOption(null, "calc.cal.none"),
            new CalendarOption("us-banking", "calc.cal.us_banking"),
            new CalendarOption("us-federal", "calc.cal.us_federal"),
            new CalendarOption("pl", "calc.cal.pl"),
        ];
        _calendar = Calendars[0];

        Steps.CollectionChanged += (_, _) => TriggerRecompute();
    }

    public IReadOnlyList<BaseKindOption> BaseKinds { get; }
    public IReadOnlyList<StepKindOption> StepKinds { get; }
    public IReadOnlyList<UnitOption> Units { get; }
    public IReadOnlyList<SnapTargetOption> SnapTargets { get; }
    public IReadOnlyList<NearestTargetOption> NearestTargets { get; }
    public IReadOnlyList<CalendarOption> Calendars { get; }

    public ObservableCollection<StepViewModel> Steps { get; } = [];

    /// <summary>Translation keys of the result's significance markers ("calc.sig.&lt;key&gt;"), rendered by
    /// the view through the shared key-to-text converter (rule 15/16).</summary>
    public ObservableCollection<string> Significance { get; } = [];

    /// <summary>The output formats, each a label plus value, with a copy affordance in the view.</summary>
    public ObservableCollection<FormatRow> Formats { get; } = [];

    public BaseKindOption SelectedBase
    {
        get => _baseKind;
        set
        {
            if (Set(ref _baseKind, value))
            {
                RaisePropertyChanged(nameof(IsSpecificBase));
                TriggerRecompute();
            }
        }
    }

    /// <summary>Whether the "specific date" text box applies (the base is an explicit date).</summary>
    public bool IsSpecificBase => _baseKind.Kind == BaseKind.Specific;

    public string BaseText
    {
        get => _baseText;
        set
        {
            if (Set(ref _baseText, value))
            {
                TriggerRecompute();
            }
        }
    }

    public CalendarOption SelectedCalendar
    {
        get => _calendar;
        set
        {
            if (Set(ref _calendar, value))
            {
                TriggerRecompute();
            }
        }
    }

    public string ResultWeekday { get => _resultWeekday; private set => Set(ref _resultWeekday, value); }
    public string ResultDate { get => _resultDate; private set => Set(ref _resultDate, value); }
    public string ResultTime { get => _resultTime; private set => Set(ref _resultTime, value); }
    public string ResultZone { get => _resultZone; private set => Set(ref _resultZone, value); }
    public string MetadataLine { get => _metadataLine; private set => Set(ref _metadataLine, value); }
    public string Error { get => _error; private set => Set(ref _error, value); }
    public bool HasError { get => _hasError; private set => Set(ref _hasError, value); }
    public bool HasResult { get => _hasResult; private set => Set(ref _hasResult, value); }
    public bool HasSignificance { get => _hasSignificance; private set => Set(ref _hasSignificance, value); }

    /// <summary>Add a step (defaults to shift) and wire its edits to a recompute.</summary>
    public void AddStep()
    {
        var step = new StepViewModel(StepKinds, Units, SnapTargets, NearestTargets);
        step.PropertyChanged += OnStepChanged;
        Steps.Add(step); // CollectionChanged triggers the recompute
    }

    /// <summary>Remove a step.</summary>
    public void RemoveStep(StepViewModel step)
    {
        step.PropertyChanged -= OnStepChanged;
        Steps.Remove(step); // CollectionChanged triggers the recompute
    }

    private void OnStepChanged(object? sender, PropertyChangedEventArgs e) => TriggerRecompute();

    /// <summary>Compute once when the screen is first shown (never from the constructor, so building the
    /// window in a test spawns no process).</summary>
    public Task EnsureComputedAsync()
    {
        if (_computedOnce)
        {
            return Task.CompletedTask;
        }

        _computedOnce = true;
        return RecomputeAsync();
    }

    private void TriggerRecompute()
    {
        if (_computedOnce)
        {
            _ = RecomputeAsync();
        }
    }

    /// <summary>Build the calc arguments for the current builder state (pure; unit-tested). Each step
    /// contributes its own flag pair, so the grammar is not shift-specific.</summary>
    public static IReadOnlyList<string> BuildCalcArgs(
        BaseKind baseKind, string baseText, IEnumerable<IReadOnlyList<string>> stepArgLists, string? calendarId)
    {
        var args = new List<string> { "--base", baseKind switch
        {
            BaseKind.Today => "today",
            BaseKind.Now => "now",
            _ => baseText.Trim(),
        } };
        foreach (var stepArgs in stepArgLists)
        {
            args.AddRange(stepArgs);
        }

        if (calendarId is not null)
        {
            args.Add("--calendar");
            args.Add(calendarId);
        }

        return args;
    }

    private async Task RecomputeAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;

        var args = BuildCalcArgs(_baseKind.Kind, _baseText, Steps.Select(s => s.ToArgs()), _calendar.Id);
        try
        {
            var result = await _client.EvaluateAsync(args, cts.Token);
            if (!cts.IsCancellationRequested)
            {
                ApplyResult(result);
            }
        }
        catch (OperationCanceledException)
        {
            // A newer recompute superseded this one - drop it silently.
        }
        catch (CalcException e)
        {
            if (!cts.IsCancellationRequested)
            {
                Error = e.Message;
                HasError = true;
            }
        }
    }

    private void ApplyResult(CalcResult result)
    {
        if (result.Moment is not { } moment)
        {
            Error = "calc returned no moment";
            HasError = true;
            return;
        }

        HasError = false;
        Error = string.Empty;

        var t = moment.Iso.IndexOf('T', StringComparison.Ordinal);
        ResultDate = t >= 0 ? moment.Iso[..t] : moment.Iso;
        ResultTime = t >= 0 ? moment.Iso[(t + 1)..] : string.Empty;
        ResultWeekday = moment.Metadata.Weekday;
        ResultZone = OffsetLabel(moment.ZoneBiasMin);

        Significance.Clear();
        foreach (var key in moment.Significance)
        {
            Significance.Add($"calc.sig.{key}");
        }

        HasSignificance = Significance.Count > 0;

        Formats.Clear();
        var f = moment.Formats;
        Formats.Add(new FormatRow("ISO date", f.IsoDate));
        Formats.Add(new FormatRow("ISO datetime", f.IsoDatetime));
        Formats.Add(new FormatRow("US", f.Us));
        Formats.Add(new FormatRow("PL", f.Pl));
        Formats.Add(new FormatRow("epoch (s)", OutOfRange(f.EpochSeconds)));
        Formats.Add(new FormatRow("epoch (ms)", OutOfRange(f.EpochMillis)));
        Formats.Add(new FormatRow("FILETIME", OutOfRange(f.Filetime)));
        Formats.Add(new FormatRow("RFC 1123", f.Rfc1123 ?? "(out of range)"));

        MetadataLine = BuildMetadataLine(moment.Metadata);
        HasResult = true;
    }

    private static string BuildMetadataLine(CalcMetadata m)
    {
        var parts = new List<string>
        {
            m.Weekday,
            $"ISO {m.IsoWeekYear}-W{m.IsoWeek:D2}",
            $"US week {m.UsWeek}",
            $"Q{m.Quarter}",
            $"day {m.DayOfYear}",
            m.IsLeapYear ? "leap year" : "common year",
        };
        if (m.BusinessDay is { } business)
        {
            parts.Add(business ? "business day" : "not a business day");
        }

        if (m.Holiday is { } holiday)
        {
            parts.Add(holiday);
        }

        return string.Join(" · ", parts);
    }

    private static string OutOfRange(long? value)
        => value?.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? "(out of range)";

    /// <summary>A "+HH:MM" / "-HH:MM" offset for a session bias (UTC = local + bias, so the offset is -bias).</summary>
    private static string OffsetLabel(int biasMin)
    {
        var off = -biasMin;
        var sign = off < 0 ? '-' : '+';
        return $"UTC{sign}{Math.Abs(off) / 60:D2}:{Math.Abs(off) % 60:D2}";
    }
}

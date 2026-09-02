using System.Collections.ObjectModel;
using System.ComponentModel;
using ChronoMock.App.Localization;
using ChronoMock.Protocol;

namespace ChronoMock.App.Calc;

/// <summary>Where an expression starts (mirrors <c>calc::Base</c>): today / now / an explicit date.</summary>
public enum BaseKind
{
    Today,
    Now,
    Specific,
}

/// <summary>Which kind of step a builder row is (mirrors calc's five typed steps): shift, snap, nearest,
/// set-time and zone (a fixed-offset re-expression of the same instant, <c>--to-zone</c>).</summary>
public enum StepKind
{
    Shift,
    Snap,
    Nearest,
    SetTime,
    Zone,
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
    private string _zoneText = "+00:00";

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
                RaisePropertyChanged(nameof(IsZone));
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

    /// <summary>Whether the zone editor applies (the row's kind is Zone).</summary>
    public bool IsZone => _kind.Kind == StepKind.Zone;

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

    // Zone field: a fixed offset +HH:MM for --to-zone (re-express the same instant, not the session zone).
    // The engine validates the shape, so a bad offset surfaces as an honest result error.
    public string ZoneText { get => _zoneText; set => Set(ref _zoneText, value); }

    /// <summary>The calc flag pair for this step, e.g. <c>--shift +18y</c>, <c>--snap eoq</c>,
    /// <c>--nearest nbd</c>, <c>--set-time 23:59:59</c> or <c>--to-zone +05:45</c>.</summary>
    public IReadOnlyList<string> ToArgs() => _kind.Kind switch
    {
        StepKind.Shift => ["--shift", $"{Sign}{Amount.Trim()}{Unit.Token}"],
        StepKind.Snap => ["--snap", _snapTarget.Token],
        StepKind.Nearest => ["--nearest", _nearestTarget.Token],
        StepKind.SetTime => ["--set-time", _setTimeText.Trim()],
        StepKind.Zone => ["--to-zone", _zoneText.Trim()],
        _ => [],
    };
}

/// <summary>A preset as the left-column list shows it (7.3): the localized name and "what this date tests"
/// subtitle, whether it needs parameters (marked - its unpack is a later slice), and the underlying
/// <see cref="PresetInfo"/> for unpacking into the builder.</summary>
public sealed class PresetItemViewModel(PresetInfo info, string culture)
{
    public PresetInfo Info { get; } = info;
    public string DisplayName { get; } = info.LocalizedName(culture);
    public string DisplayExplains { get; } = info.LocalizedExplains(culture);
    public bool IsParametric => Info.IsParametric;
}

/// <summary>One preset-parameter input in the active-preset panel (7.3, docs/04 4.2). A <c>date</c>
/// parameter is a text box (a bare date is midnight); a <c>duration</c> is an amount plus a unit, seeded
/// from the file default. The label is the parameter id as a technical name (like the format labels) - the
/// preset schema carries no localized label. Editing raises PropertyChanged so the parent re-resolves.</summary>
public sealed class ParamInputViewModel : ObservableObject
{
    private string _dateText = string.Empty;
    private string _amount;
    private UnitOption _unit;

    public ParamInputViewModel(PresetParameter param, IReadOnlyList<UnitOption> units)
    {
        Param = param;
        Units = units;
        IsDate = param.Type == "date";
        IsDuration = param.Type == "duration";
        Label = param.Id.Replace('_', ' ');
        _amount = param.DefaultAmount?.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? "1";
        _unit = FindUnit(param.DefaultUnit, units);
    }

    public PresetParameter Param { get; }
    public IReadOnlyList<UnitOption> Units { get; }
    public string Label { get; }
    public bool IsDate { get; }
    public bool IsDuration { get; }

    public string DateText { get => _dateText; set => Set(ref _dateText, value); }
    public string Amount { get => _amount; set => Set(ref _amount, value); }
    public UnitOption Unit { get => _unit; set => Set(ref _unit, value); }

    /// <summary>The parameter id, used to build the value map.</summary>
    public string Id => Param.Id;

    /// <summary>The resolved value, or null if a date has not been entered yet (so the preset stays unfilled).</summary>
    public ParamValue? ToValue()
        => IsDate
            ? string.IsNullOrWhiteSpace(_dateText) ? null : new DateValue(_dateText.Trim())
            : new DurationValue(_amount.Trim(), _unit.Token);

    private static UnitOption FindUnit(string? unit, IReadOnlyList<UnitOption> units)
    {
        if (unit is null)
        {
            return units[3]; // days
        }

        try
        {
            var token = PresetUnpack.NormalizeUnit(unit);
            return units.FirstOrDefault(u => u.Token == token) ?? units[3];
        }
        catch (NotSupportedException)
        {
            return units[3];
        }
    }
}

/// <summary>One reading of an analyzed date (7.3): its interpretation label (a key), the resolved date with
/// its weekday, and any significance markers. Built from the engine's <see cref="CalcReading"/>.</summary>
public sealed class ReadingRow
{
    public ReadingRow(CalcReading reading)
    {
        ReadingLabelKey = $"calc.reading.{reading.Reading}";
        var t = reading.Iso.IndexOf('T', StringComparison.Ordinal);
        Date = t >= 0 ? reading.Iso[..t] : reading.Iso;
        // The engine sends the weekday name in English; map it to a key so the view renders it in the
        // current language (rule 15), rather than baking an English literal into a bound string. Kept as a
        // key (not resolved here) so this row stays language-neutral and unit-testable without a WPF host.
        WeekdayKey = CalculatorViewModel.WeekdayKey(reading.Metadata.Weekday);
        Significance = new ObservableCollection<string>(reading.Significance.Select(key => $"calc.sig.{key}"));
    }

    public string ReadingLabelKey { get; }

    /// <summary>Translation key for the weekday name, rendered via KeyToText. Paired with <see cref="Date"/>.</summary>
    public string WeekdayKey { get; }

    /// <summary>The resolved date (no time), shown next to the weekday.</summary>
    public string Date { get; }

    public ObservableCollection<string> Significance { get; }
}

/// <summary>
/// The date-calculator screen's live state (Stage 4, GUI slice G3b/G3c/G4). Holds the builder inputs (base,
/// steps, calendar) and the result of evaluating them through <see cref="CalcClient"/> - the same engine
/// the CLI and substitution core use (ADR-6). Any input change recomputes; overlapping computes cancel the
/// previous one. Manual INPC, no MVVM package (gui-and-cli-constraints), like the session panel.
/// </summary>
public sealed class CalculatorViewModel : ObservableObject
{
    private readonly CalcClient _client;
    private readonly string? _presetsDir;
    private IReadOnlyList<PresetItemViewModel> _allPresets = [];
    private string _presetFilter = string.Empty;
    private bool _unpacking;
    private bool _hasActivePreset;
    private bool _activeNeedsParameters;
    private bool _hasParamInputs;
    private string _activePresetName = string.Empty;
    private string _activePresetExplains = string.Empty;
    private PresetInfo? _activePreset;

    private BaseKindOption _baseKind;
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
    private bool _canUseInSubstitution;
    private string _customFormatMask = string.Empty;
    private string _customFormatResult = string.Empty;
    private bool _hasCustomFormat;
    private string _resultMomentLocal = string.Empty;
    private int _resultZoneBias;
    private bool _computedOnce;
    private CancellationTokenSource? _cts;

    private string _analyzeText = "04/08/2008";
    private bool _hasAnalysis;
    private bool _analyzeAmbiguous;
    private bool _analyzeHasError;
    private string _analyzeError = string.Empty;
    private CancellationTokenSource? _analyzeCts;

    public CalculatorViewModel(CalcClient client, string? presetsDir = null)
    {
        _client = client ?? throw new ArgumentNullException(nameof(client));
        _presetsDir = presetsDir;

        BaseKinds =
        [
            new BaseKindOption(BaseKind.Today, "calc.base.today"),
            new BaseKindOption(BaseKind.Now, "calc.base.now"),
            new BaseKindOption(BaseKind.Specific, "calc.base.specific"),
        ];
        _baseKind = BaseKinds[0];

        // The Specific-date base is the shared MomentInput over a MomentField (locale-safe ISO, rule 2),
        // seeded to the default the calculator first shows. Editing it recomputes like the old text box did.
        Base.LoadCanonical("2026-01-01T00:00:00");
        Base.Changed += (_, _) =>
        {
            RaisePropertyChanged(nameof(ShowBaseError));
            TriggerRecompute();
        };

        StepKinds =
        [
            new StepKindOption(StepKind.Shift, "calc.kind_shift"),
            new StepKindOption(StepKind.Snap, "calc.kind_snap"),
            new StepKindOption(StepKind.Nearest, "calc.kind_nearest"),
            new StepKindOption(StepKind.SetTime, "calc.kind_settime"),
            new StepKindOption(StepKind.Zone, "calc.kind_zone"),
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

    /// <summary>The calculator presets, filtered to this module and the current text filter (7.3).</summary>
    public ObservableCollection<PresetItemViewModel> Presets { get; } = [];

    /// <summary>Whether a preset is currently the source of the builder (its name and framing show, and it
    /// clears the moment a field is edited by hand).</summary>
    public bool HasActivePreset { get => _hasActivePreset; private set => Set(ref _hasActivePreset, value); }

    /// <summary>Whether the active preset needs parameters and so could not fill the builder (honest note,
    /// its inputs are a later slice).</summary>
    public bool ActiveNeedsParameters { get => _activeNeedsParameters; private set => Set(ref _activeNeedsParameters, value); }

    /// <summary>The active preset's localized name (data locale).</summary>
    public string ActivePresetName { get => _activePresetName; private set => Set(ref _activePresetName, value); }

    /// <summary>The active preset's "what this date tests" line (data locale).</summary>
    public string ActivePresetExplains { get => _activePresetExplains; private set => Set(ref _activePresetExplains, value); }

    /// <summary>The active preset's parameter inputs (empty for a non-parametric preset).</summary>
    public ObservableCollection<ParamInputViewModel> ParamInputs { get; } = [];

    /// <summary>Whether the active preset shows parameter inputs.</summary>
    public bool HasParamInputs { get => _hasParamInputs; private set => Set(ref _hasParamInputs, value); }

    /// <summary>Reverse analysis (7.3): the reading(s) a pasted date resolves to, both shown when ambiguous.</summary>
    public ObservableCollection<ReadingRow> Readings { get; } = [];

    /// <summary>The pasted date to interpret (reverse analysis).</summary>
    public string AnalyzeText
    {
        get => _analyzeText;
        set
        {
            if (Set(ref _analyzeText, value))
            {
                TriggerAnalyze();
            }
        }
    }

    public bool HasAnalysis { get => _hasAnalysis; private set => Set(ref _hasAnalysis, value); }
    public bool AnalyzeAmbiguous { get => _analyzeAmbiguous; private set => Set(ref _analyzeAmbiguous, value); }
    public bool AnalyzeHasError { get => _analyzeHasError; private set => Set(ref _analyzeHasError, value); }
    public string AnalyzeError { get => _analyzeError; private set => Set(ref _analyzeError, value); }

    public BaseKindOption SelectedBase
    {
        get => _baseKind;
        set
        {
            if (Set(ref _baseKind, value))
            {
                RaisePropertyChanged(nameof(IsSpecificBase));
                RaisePropertyChanged(nameof(ShowBaseError));
                TriggerRecompute();
            }
        }
    }

    /// <summary>Whether the "specific date" text box applies (the base is an explicit date).</summary>
    public bool IsSpecificBase => _baseKind.Kind == BaseKind.Specific;

    /// <summary>Whether to show the base's validation message: only for a Specific base that is malformed.
    /// Shown BELOW the start-point row so it never shifts the row (mirrors the substitution At row).</summary>
    public bool ShowBaseError => IsSpecificBase && Base.HasError;

    /// <summary>The Specific-date base as a MomentField, edited through the shared MomentInput control (an
    /// ISO date box plus a calendar popup, locale-safe, rule 2). Used only when the base kind is Specific;
    /// Today/Now need no text. Editing it recomputes and drops any active preset framing (via Changed).</summary>
    public ChronoMock.App.MomentField Base { get; } = new();

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

    /// <summary>Free-text filter over the preset list (matches the name or the "what it tests" line).</summary>
    public string PresetFilter
    {
        get => _presetFilter;
        set
        {
            if (Set(ref _presetFilter, value))
            {
                ApplyPresetFilter();
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

    /// <summary>An optional custom output-format mask (.NET/Java tokens, case-sensitive - M is month, m is
    /// minute), so the tester can hit the exact string the tested app's field expects (7.3). Empty means no
    /// custom-format row. Editing it recomputes but keeps any active preset (it reformats the same moment, it
    /// does not change it - unlike a builder edit, which drops the preset framing).</summary>
    public string CustomFormatMask
    {
        get => _customFormatMask;
        set
        {
            if (Set(ref _customFormatMask, value) && _computedOnce)
            {
                _ = RecomputeAsync();
            }
        }
    }

    /// <summary>The result rendered through <see cref="CustomFormatMask"/> (the engine's <c>custom_format</c>).
    /// Built from the civil date, so it renders even when epoch/FILETIME are out of range.</summary>
    public string CustomFormatResult { get => _customFormatResult; private set => Set(ref _customFormatResult, value); }

    /// <summary>Whether a custom-format result is present (a non-empty mask produced a value), gating its row.</summary>
    public bool HasCustomFormat { get => _hasCustomFormat; private set => Set(ref _hasCustomFormat, value); }

    /// <summary>Whether the current result can go to the substitution panel: a valid moment whose zone the
    /// substitution offers, so it transfers with its zone and never as a bare local date (rule 2).</summary>
    public bool CanUseInSubstitution { get => _canUseInSubstitution; private set => Set(ref _canUseInSubstitution, value); }

    /// <summary>Raised when the user sends the result to substitution: the local moment and its zone bias.
    /// The host window (which knows both modules) fills the substitution panel and switches to it.</summary>
    public event Action<string, int>? UseInSubstitutionRequested;

    /// <summary>Whether a zone bias is one the substitution panel offers (so a moment can transfer faithfully).</summary>
    public static bool CanTransferZone(int biasMinutes)
        => ChronoMock.App.TimeInputs.Zones.Any(zone => zone.BiasMinutes == biasMinutes);

    /// <summary>Send the current result to the substitution panel (7.3, 6.3): the moment with its zone.</summary>
    public void RequestUseInSubstitution()
    {
        if (CanUseInSubstitution)
        {
            UseInSubstitutionRequested?.Invoke(_resultMomentLocal, _resultZoneBias);
        }
    }

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
        LoadPresets();
        _ = AnalyzeAsync(); // the reverse-analysis strip is live from the start (its default example)
        return RecomputeAsync();
    }

    /// <summary>Build the calc arguments for reverse analysis (pure; unit-tested).</summary>
    public static IReadOnlyList<string> BuildAnalyzeArgs(string text) => ["--analyze", text.Trim()];

    private void TriggerAnalyze()
    {
        if (_computedOnce)
        {
            _ = AnalyzeAsync();
        }
    }

    private async Task AnalyzeAsync()
    {
        _analyzeCts?.Cancel();
        var cts = new CancellationTokenSource();
        _analyzeCts = cts;

        try
        {
            var result = await _client.EvaluateAsync(BuildAnalyzeArgs(_analyzeText), cts.Token);
            if (!cts.IsCancellationRequested)
            {
                ApplyAnalysis(result);
            }
        }
        catch (OperationCanceledException)
        {
            // A newer analysis superseded this one - drop it.
        }
        catch (CalcException e)
        {
            if (!cts.IsCancellationRequested)
            {
                AnalyzeError = e.Message;
                AnalyzeHasError = true;
                HasAnalysis = false;
                AnalyzeAmbiguous = false;
                Readings.Clear();
            }
        }
        catch (Exception e)
        {
            // Any other failure (an incomplete reading dereferenced in ApplyAnalysis / ReadingRow) is
            // surfaced honestly rather than swallowed by this fire-and-forget task (M-11, rule 6).
            if (!cts.IsCancellationRequested)
            {
                AnalyzeError = e.Message;
                AnalyzeHasError = true;
                HasAnalysis = false;
                AnalyzeAmbiguous = false;
                Readings.Clear();
            }
        }
    }

    private void ApplyAnalysis(CalcResult result)
    {
        if (result.Analysis is not { } analysis)
        {
            AnalyzeError = "analyze returned no readings";
            AnalyzeHasError = true;
            HasAnalysis = false;
            return;
        }

        AnalyzeHasError = false;
        AnalyzeError = string.Empty;
        AnalyzeAmbiguous = analysis.Ambiguous;
        Readings.Clear();
        foreach (var reading in analysis.Readings)
        {
            Readings.Add(new ReadingRow(reading));
        }

        HasAnalysis = Readings.Count > 0;
    }

    /// <summary>Load the shared preset catalogue once, keep the ones this module offers, and show them
    /// (7.3). Reading happens on first reveal, never in the constructor, so building the window in a test
    /// touches no files.</summary>
    private void LoadPresets()
    {
        if (_presetsDir is null)
        {
            return;
        }

        var culture = LocalizationService.CurrentCulture;
        _allPresets = PresetCatalog.Load(_presetsDir)
            .Where(p => p.ForCalculator)
            .Select(p => new PresetItemViewModel(p, culture))
            .OrderBy(p => p.DisplayName, StringComparer.CurrentCulture)
            .ToList();
        ApplyPresetFilter();
    }

    private void ApplyPresetFilter()
    {
        Presets.Clear();
        foreach (var preset in _allPresets)
        {
            if (PresetMatchesFilter(preset.DisplayName, preset.DisplayExplains, _presetFilter))
            {
                Presets.Add(preset);
            }
        }
    }

    /// <summary>Whether a preset with this name and "what it tests" line survives the filter: an empty
    /// filter keeps everything, otherwise it matches either field case-insensitively. Pure and unit-tested.</summary>
    public static bool PresetMatchesFilter(string name, string explains, string filter)
    {
        var f = filter.Trim();
        return f.Length == 0
            || name.Contains(f, StringComparison.CurrentCultureIgnoreCase)
            || explains.Contains(f, StringComparison.CurrentCultureIgnoreCase);
    }

    private void TriggerRecompute()
    {
        if (_computedOnce)
        {
            // A by-hand edit means the builder is no longer "the preset", so drop its framing (rule 6).
            // Edits made while unpacking a preset are exempt.
            if (!_unpacking)
            {
                ClearActivePreset();
            }

            _ = RecomputeAsync();
        }
    }

    /// <summary>Apply a preset by filling the builder from its moment, so the result recomputes through the
    /// normal pipeline (7.3, one source of truth). A parametric preset can't be filled without values, so it
    /// shows an honest "needs parameters" note instead of a wrong date (rule 6) - its inputs are slice G4-2.</summary>
    public void ApplyPreset(PresetInfo preset)
    {
        ArgumentNullException.ThrowIfNull(preset);
        _activePreset = preset;
        PopulateParamInputs(preset);
        ResolveActivePreset();
    }

    // Build the parameter inputs for a preset (empty for a non-parametric one), seeded from the file
    // defaults, and wire each to a re-resolve so filling a value recomputes.
    private void PopulateParamInputs(PresetInfo preset)
    {
        ClearParamInputsOnly();
        foreach (var param in preset.Parameters)
        {
            var input = new ParamInputViewModel(param, Units);
            input.PropertyChanged += OnParamChanged;
            ParamInputs.Add(input);
        }

        HasParamInputs = ParamInputs.Count > 0;
    }

    private void OnParamChanged(object? sender, PropertyChangedEventArgs e) => ResolveActivePreset();

    // Gather the parameter inputs and fill the builder from the preset's moment. A missing date leaves the
    // inputs shown with the honest note and does not fill a wrong moment (rule 6).
    private void ResolveActivePreset()
    {
        if (_activePreset is not { } preset)
        {
            return;
        }

        var culture = LocalizationService.CurrentCulture;
        var values = new Dictionary<string, ParamValue>();
        foreach (var input in ParamInputs)
        {
            if (input.ToValue() is not { } value)
            {
                ShowActivePreset(preset, culture, needsParameters: true);
                return;
            }

            values[input.Id] = value;
        }

        try
        {
            var unpacked = PresetUnpack.UnpackMoment(preset.Moment, values);
            _unpacking = true;
            while (Steps.Count > 0)
            {
                RemoveStep(Steps[0]);
            }

            SelectedBase = BaseKinds.First(b => b.Kind == unpacked.Base);
            if (unpacked.Base == BaseKind.Specific)
            {
                Base.LoadCanonical(unpacked.BaseText);
            }

            foreach (var step in unpacked.Steps)
            {
                AddUnpackedStep(step);
            }

            ApplyMarketCalendar(preset.Market);
        }
        catch (Exception ex) when (ex is NotSupportedException or InvalidOperationException
                                       or KeyNotFoundException or FormatException)
        {
            // A shape the builder cannot represent, OR a malformed preset moment (missing base/steps, an
            // empty step, a shift without an amount, a non-string where a token is expected) - PresetUnpack
            // throws these on a hand-edited or user-supplied preset file. Be honest with the "needs
            // parameters" note rather than crash the dispatcher (M-8, rule 6).
            _unpacking = false;
            ShowActivePreset(preset, culture, needsParameters: true);
            return;
        }

        _unpacking = false;
        ShowActivePreset(preset, culture, needsParameters: false);
        _ = RecomputeAsync();
    }

    private void ClearParamInputsOnly()
    {
        foreach (var input in ParamInputs)
        {
            input.PropertyChanged -= OnParamChanged;
        }

        ParamInputs.Clear();
        HasParamInputs = false;
    }

    /// <summary>Select the calendar a regional preset implies (docs/05 3.5), so a market preset computes
    /// without the user first picking one. A preset with no market leaves the calendar as it is.</summary>
    private void ApplyMarketCalendar(string? market)
    {
        var id = market switch
        {
            "us" => "us-banking",
            "pl" => "pl",
            _ => null,
        };
        if (id is not null)
        {
            SelectedCalendar = Calendars.FirstOrDefault(c => c.Id == id) ?? _calendar;
        }
    }

    private void AddUnpackedStep(UnpackedStep spec)
    {
        var step = new StepViewModel(StepKinds, Units, SnapTargets, NearestTargets);
        step.PropertyChanged += OnStepChanged;
        step.SelectedKind = StepKinds.First(k => k.Kind == spec.Kind);
        switch (spec.Kind)
        {
            case StepKind.Shift:
                step.Sign = spec.Sign;
                step.Amount = spec.Amount;
                step.Unit = Units.FirstOrDefault(u => u.Token == spec.UnitToken) ?? step.Unit;
                break;
            case StepKind.Snap:
                step.SnapTarget = SnapTargets.FirstOrDefault(t => t.Token == spec.SnapToken) ?? step.SnapTarget;
                break;
            case StepKind.Nearest:
                step.NearestTarget = NearestTargets.FirstOrDefault(t => t.Token == spec.NearestToken) ?? step.NearestTarget;
                break;
            case StepKind.SetTime:
                step.SetTimeText = spec.SetTime;
                break;
            case StepKind.Zone:
                step.ZoneText = spec.ZoneOffset;
                break;
        }

        Steps.Add(step);
    }

    private void ShowActivePreset(PresetInfo preset, string culture, bool needsParameters)
    {
        ActivePresetName = preset.LocalizedName(culture);
        ActivePresetExplains = preset.LocalizedExplains(culture);
        ActiveNeedsParameters = needsParameters;
        HasActivePreset = true;
    }

    private void ClearActivePreset()
    {
        HasActivePreset = false;
        ActiveNeedsParameters = false;
        ActivePresetName = string.Empty;
        ActivePresetExplains = string.Empty;
        _activePreset = null;
        ClearParamInputsOnly();
    }

    /// <summary>Build the calc arguments for the current builder state (pure; unit-tested). Each step
    /// contributes its own flag pair, so the grammar is not shift-specific.</summary>
    public static IReadOnlyList<string> BuildCalcArgs(
        BaseKind baseKind,
        string baseText,
        IEnumerable<IReadOnlyList<string>> stepArgLists,
        string? calendarId,
        string? customFormatMask = null)
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

        // A blank mask means no custom format (an empty --format argument is a usage error), so it is only
        // appended when the tester actually typed a mask. Trimmed like the other builder fields.
        if (!string.IsNullOrWhiteSpace(customFormatMask))
        {
            args.Add("--format");
            args.Add(customFormatMask.Trim());
        }

        return args;
    }

    private async Task RecomputeAsync()
    {
        _cts?.Cancel();

        // A Specific base that is not a well-formed moment yet must not spawn a broken --base: the MomentInput
        // shows the precise inline reason, and the result is cleared rather than left stale (rule 6). Today
        // and Now carry no base text, so they always compute.
        if (_baseKind.Kind == BaseKind.Specific && !Base.IsValid)
        {
            ClearResult();
            return;
        }

        var cts = new CancellationTokenSource();
        _cts = cts;

        var args = BuildCalcArgs(
            _baseKind.Kind, Base.Canonical, Steps.Select(s => s.ToArgs()), _calendar.Id, _customFormatMask);
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
                CanUseInSubstitution = false;
            }
        }
        catch (Exception e)
        {
            // Any other failure (a malformed/incomplete calc result dereferenced in ApplyResult) is
            // surfaced as an honest error, never silently swallowed by this fire-and-forget task (M-11, rule 6).
            if (!cts.IsCancellationRequested)
            {
                Error = e.Message;
                HasError = true;
                CanUseInSubstitution = false;
            }
        }
    }

    private void ApplyResult(CalcResult result)
    {
        // Guard the nested non-nullable pieces too, not just Moment (M-11, tied to L-11): System.Text.Json
        // does not enforce non-nullability, so an incomplete/other-schema payload can leave Formats or
        // Metadata null and NRE below (moment.Formats.IsoDate, moment.Metadata.Weekday). Honest error instead.
        if (result.Moment is not { } moment || moment.Formats is null || moment.Metadata is null)
        {
            Error = "calc returned an incomplete moment";
            HasError = true;
            CanUseInSubstitution = false;
            return;
        }

        HasError = false;
        Error = string.Empty;

        var t = moment.Iso.IndexOf('T', StringComparison.Ordinal);
        ResultDate = t >= 0 ? moment.Iso[..t] : moment.Iso;
        ResultTime = t >= 0 ? moment.Iso[(t + 1)..] : string.Empty;
        // The engine sends the weekday in English; render it in the current language (rule 15). Resolved
        // here (not exposed as a key) because it is one part of the result column the view binds as text.
        ResultWeekday = Tr(WeekdayKey(moment.Metadata.Weekday));
        ResultZone = OffsetLabel(moment.ZoneBiasMin);

        Significance.Clear();
        foreach (var key in moment.Significance)
        {
            Significance.Add($"calc.sig.{key}");
        }

        HasSignificance = Significance.Count > 0;

        Formats.Clear();
        var f = moment.Formats;
        // Format labels are translated (rule 15); the values are data. Format NAMES (US, PL, FILETIME,
        // RFC 1123) are proper nouns, so their PL text keeps them as-is.
        Formats.Add(new FormatRow(Tr("calc.fmt.iso_date"), f.IsoDate));
        Formats.Add(new FormatRow(Tr("calc.fmt.iso_datetime"), f.IsoDatetime));
        Formats.Add(new FormatRow(Tr("calc.fmt.us"), f.Us));
        Formats.Add(new FormatRow(Tr("calc.fmt.pl"), f.Pl));
        Formats.Add(new FormatRow(Tr("calc.fmt.epoch_s"), OutOfRange(f.EpochSeconds)));
        Formats.Add(new FormatRow(Tr("calc.fmt.epoch_ms"), OutOfRange(f.EpochMillis)));
        Formats.Add(new FormatRow(Tr("calc.fmt.filetime"), OutOfRange(f.Filetime)));
        Formats.Add(new FormatRow(Tr("calc.fmt.rfc1123"), f.Rfc1123 ?? Tr("calc.out_of_range")));

        // The custom format is present only when a mask was passed (--format). It comes from the civil date,
        // so unlike epoch/FILETIME it never falls out of range.
        var custom = moment.CustomFormat ?? string.Empty;
        CustomFormatResult = custom;
        HasCustomFormat = custom.Length > 0;

        MetadataLine = BuildMetadataLine(moment.Metadata);
        HasResult = true;

        // Remember the moment with its zone for the bridge to substitution (rule 2 - never a bare date).
        _resultMomentLocal = moment.Iso;
        _resultZoneBias = moment.ZoneBiasMin;
        CanUseInSubstitution = CanTransferZone(moment.ZoneBiasMin);
    }

    /// <summary>Clear the result column to an honest empty state, used when a Specific base is not a valid
    /// moment yet: the MomentInput already shows why, so the result must not keep a stale value (rule 6).</summary>
    private void ClearResult()
    {
        HasError = false;
        Error = string.Empty;
        HasResult = false;
        CanUseInSubstitution = false;
        ResultWeekday = string.Empty;
        ResultDate = "-";
        ResultTime = string.Empty;
        ResultZone = string.Empty;
        MetadataLine = string.Empty;
        Significance.Clear();
        HasSignificance = false;
        Formats.Clear();
        CustomFormatResult = string.Empty;
        HasCustomFormat = false;
    }

    private static string BuildMetadataLine(CalcMetadata m)
    {
        // Labels are translated (rule 15); ISO and Q are universal notation, and the numbers and holiday
        // name are data. The weekday comes from the engine in English, mapped to a key and resolved.
        var parts = new List<string>
        {
            Tr(WeekdayKey(m.Weekday)),
            $"ISO {m.IsoWeekYear}-W{m.IsoWeek:D2}",
            $"{Tr("calc.md.us_week")} {m.UsWeek}",
            $"Q{m.Quarter}",
            $"{Tr("calc.md.day")} {m.DayOfYear}",
            Tr(m.IsLeapYear ? "calc.md.leap_year" : "calc.md.common_year"),
        };
        if (m.BusinessDay is { } business)
        {
            parts.Add(Tr(business ? "calc.md.business_day" : "calc.md.not_business_day"));
        }

        if (m.Holiday is { } holiday)
        {
            parts.Add(holiday); // the holiday name follows the DATA locale (the calendar), not the UI (rule 15)
        }

        return string.Join(" · ", parts);
    }

    /// <summary>Translation key for a weekday name the engine emits in English (e.g. "Monday" ->
    /// "calc.weekday.monday"). An unrecognised name falls through to KeyToText's honest key-as-text.</summary>
    internal static string WeekdayKey(string engineWeekday)
        => $"calc.weekday.{engineWeekday.ToLowerInvariant()}";

    /// <summary>Resolve a translation key to text in the current language, for the composed result strings
    /// (metadata line, weekday, format labels) that the view binds as text rather than as a key.</summary>
    private static string Tr(string key) => TranslationKeyConverter.Resolve(key);

    private static string OutOfRange(long? value)
        => value?.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? Tr("calc.out_of_range");

    /// <summary>A "+HH:MM" / "-HH:MM" offset for a session bias (UTC = local + bias, so the offset is -bias).</summary>
    private static string OffsetLabel(int biasMin)
    {
        var off = -biasMin;
        var sign = off < 0 ? '-' : '+';
        return $"UTC{sign}{Math.Abs(off) / 60:D2}:{Math.Abs(off) % 60:D2}";
    }
}

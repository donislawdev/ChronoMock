using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// The "relative to now" line under the moment field: a sign, an amount and a unit, plus the one action
/// that turns them into the moment above. The panel's equivalent of <c>chrono run --at +30d</c>.
///
/// <para>
/// Its own view model rather than four more properties on <see cref="SessionViewModel"/>, for the same
/// reason CoreSession and ScenarioMoment were split off it: four properties inline measured CA1506 = 86
/// against a ceiling of 81. Behind this seam the panel gains a single type, which is the one the owner
/// raised the ceiling by (gui/CodeMetricsConfig.txt, 2026-09-07) so the panel's state could live on the
/// panel's view model instead of on the window.
/// </para>
/// </summary>
public sealed class RelativeMomentViewModel : ObservableObject
{
    private readonly MomentField _target;
    private readonly CalcClient? _engine;
    private readonly Func<int> _zoneBias;

    // "+ 1 day" - the commonest relative start, and the same default a fresh calculator shift step opens
    // on, so the two surfaces do not disagree about what a new delta looks like.
    private string _sign = StepViewModel.Signs[0];
    private string _amount = "1";
    private UnitOption _unit = RelativeMoment.Units.First(u => u.Token == "d");
    private string _errorKey = string.Empty;

    /// <param name="target">The moment field this line fills - the same one the At row edits.</param>
    /// <param name="engine">The calculator engine, or null when it could not be resolved.</param>
    /// <param name="zoneBias">The session zone's bias, read at the moment of use rather than captured as a
    /// value, so changing the zone changes what "now plus one day" means with no wiring between the two.
    /// </param>
    public RelativeMomentViewModel(MomentField target, CalcClient? engine, Func<int> zoneBias)
    {
        _target = target ?? throw new ArgumentNullException(nameof(target));
        _engine = engine;
        _zoneBias = zoneBias ?? throw new ArgumentNullException(nameof(zoneBias));
    }

    /// <summary>The signs offered - the same pair the calculator's shift step uses.</summary>
    public IReadOnlyList<string> Signs => StepViewModel.Signs;

    /// <summary>The units offered - the calculator's, minus business days (a session has no calendar).</summary>
    public IReadOnlyList<UnitOption> Units => RelativeMoment.Units;

    public string Sign { get => _sign; set => Set(ref _sign, value); }

    public string Amount { get => _amount; set => Set(ref _amount, value); }

    public UnitOption Unit { get => _unit; set => Set(ref _unit, value); }

    /// <summary>Why the last attempt did not fill the field, or empty when it did.</summary>
    public string ErrorKey
    {
        get => _errorKey;
        private set
        {
            if (Set(ref _errorKey, value))
            {
                RaisePropertyChanged(nameof(HasError));
            }
        }
    }

    public bool HasError => !string.IsNullOrEmpty(_errorKey);

    /// <summary>The arguments this line would send, or null when the amount is not usable - the one place
    /// the controls become a question. <see cref="ApplyAsync"/> sends exactly this, so a test reading it
    /// reads what the panel asks, not a rebuild of it: a test over
    /// <see cref="RelativeMoment.BuildArgs"/> alone would pass over controls bound to nothing.</summary>
    internal IReadOnlyList<string>? CurrentArgs()
    {
        var token = RelativeMoment.ShiftToken(_sign, _amount, _unit.Token);
        return token is null ? null : RelativeMoment.BuildArgs(token, _zoneBias());
    }

    /// <summary>
    /// Fill the moment field with now, shifted by this delta. The arithmetic belongs to the engine (months,
    /// quarters and years fold onto the civil date), and the session zone travels with the question, because
    /// "now plus one day" is a different civil date read from another zone (untouchable rule 2).
    /// </summary>
    public async Task ApplyAsync()
    {
        ErrorKey = string.Empty;

        var args = CurrentArgs();
        if (args is null)
        {
            ErrorKey = "moment.relative_bad_amount";
            return;
        }

        var resolved = await RelativeMoment.ResolveAsync(_engine, args);
        if (resolved.Iso is null)
        {
            ErrorKey = resolved.ErrorKey!;
            return;
        }

        _target.LoadCanonical(resolved.Iso);
    }
}

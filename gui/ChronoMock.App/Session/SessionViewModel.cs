using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Drives one live session and projects it onto the two-clock panel. It gates on <c>ready</c> (checks the
/// protocol version and bitness with <see cref="HandshakeGate"/>) BEFORE it lets the core launch the target,
/// then relays every <c>state</c> heartbeat into the two clocks until the session ends (docs/07 GUI-over-CLI,
/// docs/08 sections 3, 6, 7).
/// <para>
/// The event-to-view mapping lives in <see cref="Apply"/>, which is pure and synchronous, so it is unit
/// tested without a core process. <see cref="StartAsync"/> is entered on the UI thread and never configures
/// its awaits off it, so every <see cref="Apply"/> call lands on the UI thread and property changes are
/// raised there. The core stays the single source of truth for the clocks (untouchable rule 2); the panel
/// only renders what the heartbeat reports and never derives time itself.
/// </para>
/// </summary>
public sealed class SessionViewModel : ObservableObject, IAsyncDisposable
{
    private static readonly TimeSpan ReadyTimeout = TimeSpan.FromSeconds(10);

    private CoreClient? _client;
    private long _nextCommandId = 10; // start commands use low ids; in-flight commands take the rest
    private string _statusKey = "status.idle";
    private SessionStatusKind _statusKind = SessionStatusKind.Idle;
    private string _multiplierText = string.Empty;
    private string _lastError = string.Empty;
    private bool _idle = true;
    private string? _targetPath;
    private string _momentText = "2038-01-19T03:14:07";
    private ZoneOption _selectedZone;
    private ModeOption _selectedMode;

    public SessionViewModel()
    {
        // Defaults match the moment/mode the panel shipped with before these inputs existed.
        _selectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == -120); // UTC+02:00
        _selectedMode = TimeInputs.Modes.First(m => m.Multiplier == 60);    // x60
    }
    private bool _verdictKnown;
    private VerdictKind _verdictKind = VerdictKind.Unknown;
    private string _verdictLabelKey = "verdict.unknown";
    private string _verdictReasonKey = string.Empty;
    private string _verdictMeaningKey = string.Empty;
    private bool _verdictHasReason;
    private bool _verdictHasMeaning;
    private int _processCount;
    private bool _coverageKnown;
    private bool _coverageCaptured;
    private IReadOnlyList<string> _covered = [];
    private IReadOnlyList<string> _observed = [];
    private IReadOnlyList<string> _uncovered = [];
    private IReadOnlyList<string> _warnings = [];

    public ClockView Fake { get; } = new("clock.fake");

    public ClockView Real { get; } = new("clock.real");

    /// <summary>Translation key for the current status label (the view renders it in the user's language).</summary>
    public string StatusKey { get => _statusKey; private set => Set(ref _statusKey, value); }

    public SessionStatusKind StatusKind
    {
        get => _statusKind;
        private set { if (Set(ref _statusKind, value)) { RaisePropertyChanged(nameof(IsRunning)); } }
    }

    /// <summary>True while the session is live - the in-flight controls bind their visibility to this.</summary>
    public bool IsRunning => _statusKind == SessionStatusKind.Running;

    /// <summary>The current rate as data, e.g. "x60" - empty until the first heartbeat.</summary>
    public string MultiplierText { get => _multiplierText; private set => Set(ref _multiplierText, value); }

    /// <summary>The raw failure detail when something went wrong - shown verbatim so a failure is never silent.</summary>
    public string LastError { get => _lastError; private set => Set(ref _lastError, value); }

    /// <summary>Path to the target executable to run, chosen by the user (or a bundled default in dev).</summary>
    public string? TargetPath
    {
        get => _targetPath;
        private set
        {
            if (Set(ref _targetPath, value))
            {
                RaisePropertyChanged(nameof(TargetName));
                RaisePropertyChanged(nameof(HasTarget));
                RaisePropertyChanged(nameof(CanStart));
            }
        }
    }

    /// <summary>The chosen target's file name for display, empty when none is chosen.</summary>
    public string TargetName => _targetPath is null ? string.Empty : Path.GetFileName(_targetPath);

    /// <summary>True once a target has been chosen - Start stays disabled until then.</summary>
    public bool HasTarget => _targetPath is not null;

    /// <summary>The moment the target should see, entered in the session zone (rule 2, chrono-mock 9.5).</summary>
    public string MomentText
    {
        get => _momentText;
        set
        {
            if (Set(ref _momentText, value))
            {
                RaisePropertyChanged(nameof(MomentValid));
                RaisePropertyChanged(nameof(MomentInvalid));
                RaisePropertyChanged(nameof(CanStart));
            }
        }
    }

    /// <summary>True when the entered moment parses as a well-formed date and time. The core does the deep
    /// validation (DST gap, non-leap Feb 29, range) and reports it as an error (docs/08 section 5).</summary>
    public bool MomentValid => TryParseMoment(_momentText, out _);

    /// <summary>Convenience for showing the input hint - the inverse of <see cref="MomentValid"/>.</summary>
    public bool MomentInvalid => !MomentValid;

    /// <summary>The session-zone options (fixed offsets, MVP markets).</summary>
    public IReadOnlyList<ZoneOption> Zones => TimeInputs.Zones;

    /// <summary>The time-mode options (flowing, frozen, xN).</summary>
    public IReadOnlyList<ModeOption> Modes => TimeInputs.Modes;

    public ZoneOption SelectedZone { get => _selectedZone; set => Set(ref _selectedZone, value); }

    public ModeOption SelectedMode { get => _selectedMode; set => Set(ref _selectedMode, value); }

    /// <summary>True when a session may be started: nothing is running, a target is chosen, moment is valid.</summary>
    public bool CanStart => _idle && HasTarget && MomentValid;

    /// <summary>Choose the target executable to run (from the picker, or the dev default).</summary>
    public void SetTarget(string path) => TargetPath = path;

    /// <summary>
    /// Change the multiplier in flight (0 freezes, N resumes at N times). The core re-anchors from the
    /// current clock so the fake time is continuous across the change (ADR-5). No-op unless a session is
    /// running - the controls are only shown then, but the guard keeps a stray call safe.
    /// </summary>
    public void SendMultiplier(long multiplier)
    {
        var client = _client;
        if (client is null || !IsRunning)
        {
            return;
        }

        try
        {
            client.Send(new SetMultiplierCommand { Id = _nextCommandId++, Multiplier = multiplier });
        }
        catch (IOException)
        {
            // The core is already gone - the read loop will surface the end; nothing to do here.
        }
    }

    /// <summary>
    /// Jump the fake clock by a relative delta (e.g. "+1d", "-2h" - units s/m/h/d/w). The core adds it to
    /// the current fake time and re-anchors; a backward jump never rewinds the duration axis (rule 3). The
    /// core validates the delta and reports a bad one as an error. No-op unless a session is running.
    /// </summary>
    public void SendJump(string delta)
    {
        var client = _client;
        if (client is null || !IsRunning)
        {
            return;
        }

        try
        {
            client.Send(new JumpCommand
            {
                Id = _nextCommandId++,
                To = new MomentSpec { Kind = "relative", Delta = delta },
            });
        }
        catch (IOException)
        {
            // The core is already gone - the read loop will surface the end; nothing to do here.
        }
    }

    /// <summary>True when no session is running - the setup inputs bind their enabled state to this, so the
    /// user can still fix an invalid moment (which disables Start but not the fields).</summary>
    public bool IsIdle => _idle;

    /// <summary>Backs <see cref="CanStart"/> and <see cref="IsIdle"/>: true when no session is running.</summary>
    private bool Idle
    {
        get => _idle;
        set
        {
            if (Set(ref _idle, value))
            {
                RaisePropertyChanged(nameof(CanStart));
                RaisePropertyChanged(nameof(IsIdle));
            }
        }
    }

    /// <summary>True once a verdict has arrived - the indicator stays hidden until then.</summary>
    public bool VerdictKnown { get => _verdictKnown; private set => Set(ref _verdictKnown, value); }

    /// <summary>The verdict kind, driving the indicator's glyph and colour (chrono-mock 7.1).</summary>
    public VerdictKind VerdictKind { get => _verdictKind; private set => Set(ref _verdictKind, value); }

    /// <summary>Translation key for the verdict label ("verdict.works" etc.).</summary>
    public string VerdictLabelKey { get => _verdictLabelKey; private set => Set(ref _verdictLabelKey, value); }

    /// <summary>The core's specific reason key (rendered raw if untranslated), shown for a non-works verdict.</summary>
    public string VerdictReasonKey { get => _verdictReasonKey; private set => Set(ref _verdictReasonKey, value); }

    /// <summary>The plain-language "what this means for the test" key, shown for a non-works verdict.</summary>
    public string VerdictMeaningKey { get => _verdictMeaningKey; private set => Set(ref _verdictMeaningKey, value); }

    public bool VerdictHasReason { get => _verdictHasReason; private set => Set(ref _verdictHasReason, value); }

    public bool VerdictHasMeaning { get => _verdictHasMeaning; private set => Set(ref _verdictHasMeaning, value); }

    /// <summary>Size of the process family the session verdict covers (parent plus children).</summary>
    public int ProcessCount
    {
        get => _processCount;
        private set
        {
            if (Set(ref _processCount, value))
            {
                RaisePropertyChanged(nameof(IsFamily));
            }
        }
    }

    /// <summary>True when the session spanned more than one process (the verdict is the family aggregate).</summary>
    public bool IsFamily => _processCount > 1;

    /// <summary>True once a coverage report has arrived - the audit block stays hidden until then.</summary>
    public bool CoverageKnown { get => _coverageKnown; private set => Set(ref _coverageKnown, value); }

    /// <summary>Covered channels, formatted "channel  xN", from the parent process (never summed, rule 4).</summary>
    public IReadOnlyList<string> Covered
    {
        get => _covered;
        private set { if (Set(ref _covered, value)) { RaisePropertyChanged(nameof(HasCovered)); } }
    }

    /// <summary>Channels hooked but deliberately left real (e.g. QPC-based waits, ADR-2), formatted "channel  xN".</summary>
    public IReadOnlyList<string> Observed
    {
        get => _observed;
        private set { if (Set(ref _observed, value)) { RaisePropertyChanged(nameof(HasObserved)); } }
    }

    /// <summary>Uncovered channel identifiers (raw API names, not translation keys) - the partial verdict's evidence.</summary>
    public IReadOnlyList<string> Uncovered
    {
        get => _uncovered;
        private set { if (Set(ref _uncovered, value)) { RaisePropertyChanged(nameof(HasUncovered)); } }
    }

    /// <summary>Warning translation keys the core raised for this process (rendered in the current language).</summary>
    public IReadOnlyList<string> Warnings
    {
        get => _warnings;
        private set { if (Set(ref _warnings, value)) { RaisePropertyChanged(nameof(HasWarnings)); } }
    }

    public bool HasCovered => _covered.Count > 0;

    public bool HasObserved => _observed.Count > 0;

    public bool HasUncovered => _uncovered.Count > 0;

    public bool HasWarnings => _warnings.Count > 0;

    /// <summary>
    /// Fold one event into the view state. Pure and synchronous (no I/O, no threading) so the mapping is
    /// unit-testable; the live loop marshals each call onto the UI thread. A late <c>state</c> after a
    /// terminal outcome is ignored, so a finished session is never resurrected as "running".
    /// </summary>
    public void Apply(ChronoEvent evt)
    {
        switch (evt)
        {
            case StateEvent s when !IsTerminal(StatusKind):
                Fake.Wall = s.Fake.Wall;
                Fake.Zone = ZoneLabel.FromBiasMinutes(s.Fake.ZoneBiasMin);
                Real.Wall = s.Real.Wall;
                Real.Zone = ZoneLabel.FromBiasMinutes(s.Real.ZoneBiasMin);
                MultiplierText = $"x{s.Multiplier}";
                SetStatus("status.running", SessionStatusKind.Running);
                break;
            case VanishedEvent:
                SetStatus("status.did_not_take_effect", SessionStatusKind.DidNotTakeEffect);
                break;
            case EndedEvent:
                SetStatus("status.ended", SessionStatusKind.Ended);
                break;
            case ErrorEvent:
                SetStatus("status.error", SessionStatusKind.Error);
                break;
            case VerdictEvent v:
                // The per-process verdict, at start. It gates refuse_start and is the first indicator shown.
                SetVerdict(VerdictKinds.Parse(v.Verdict), v.ReasonKey);
                break;
            case SessionVerdictEvent sv:
                // The family aggregate, at end - it overrides the per-process verdict on the indicator.
                ProcessCount = sv.ProcessCount;
                SetVerdict(VerdictKinds.Parse(sv.Verdict), sv.ReasonKey);
                break;
            case CoverageEvent c when !_coverageCaptured:
                // Show the PARENT's coverage (the first event). Children's coverage is never summed into it
                // (untouchable rule 4); a per-process family breakdown is a later slice.
                _coverageCaptured = true;
                Covered = c.Covered.Select(FormatChannel).ToList();
                Observed = c.Observed.Select(FormatChannel).ToList();
                Uncovered = [.. c.Uncovered];
                Warnings = [.. c.WarningKeys];
                CoverageKnown = true;
                break;
            default:
                // ready and ack do not change the panel.
                break;
        }
    }

    /// <summary>
    /// Spawn the core, gate on <c>ready</c>, launch the demo target, and relay its state until the core
    /// closes the stream. Safe to call from a UI click handler - it never throws for the expected failures,
    /// it turns them into an honest status instead.
    /// </summary>
    public async Task StartAsync()
    {
        if (!CanStart)
        {
            return;
        }

        Idle = false;
        LastError = string.Empty;
        ResetSession();
        SetStatus("status.connecting", SessionStatusKind.Connecting);

        CoreClient? client = null;
        try
        {
            var plan = SessionPlan.Build(TargetPath!, BuildTime());
            client = CoreClient.Connect(plan.CorePath);
            _client = client;

            var ready = await ReadReadyAsync(client, ReadyTimeout);
            if (ready is null)
            {
                SetStatus("status.no_ready", SessionStatusKind.Error);
                return;
            }

            var gate = HandshakeGate.Check(ready, ProtocolJson.ProtocolVersion, plan.Machine);
            if (!gate.IsOk)
            {
                // Refuse before the target is ever launched (docs/08 section 3, zasady/13 section 11).
                SetStatus(gate.ReasonKey!, SessionStatusKind.Error);
                return;
            }

            client.Send(plan.Start);
            SetStatus("status.running", SessionStatusKind.Running);

            await foreach (var evt in client.Events.ReadAllAsync())
            {
                Apply(evt);
            }

            // The core closed its stdout. If we did not already reach a terminal outcome, the core stopped
            // mid-session and the target's time is now frozen (docs/08 section 7) - say so, do not hang.
            if (!IsTerminal(StatusKind))
            {
                SetStatus("status.core_stopped", SessionStatusKind.CoreStopped);
            }
        }
        catch (Exception ex) when (ex is FileNotFoundException or InvalidOperationException
                                       or System.ComponentModel.Win32Exception)
        {
            // Setup failed: a build artifact is missing, the repo root was not found, or the core would not spawn.
            LastError = ex.Message;
            SetStatus("status.core_missing", SessionStatusKind.Error);
        }
        catch (IOException ex)
        {
            // The protocol pipe broke mid-session.
            LastError = ex.Message;
            SetStatus("status.error", SessionStatusKind.Error);
        }
        finally
        {
            if (client is not null)
            {
                // DisposeAsync blocks briefly (it waits for the core to exit), so keep it off the UI thread.
                await Task.Run(() => client.DisposeAsync().AsTask());
                if (ReferenceEquals(_client, client))
                {
                    _client = null;
                }
            }

            Idle = true;
        }
    }

    public async ValueTask DisposeAsync()
    {
        var client = _client;
        _client = null;
        if (client is not null)
        {
            await Task.Run(() => client.DisposeAsync().AsTask());
        }
    }

    /// <summary>Read events until the <c>ready</c> handshake, or null on timeout or an early end of stream.</summary>
    private static async Task<ReadyEvent?> ReadReadyAsync(CoreClient client, TimeSpan timeout)
    {
        using var cts = new CancellationTokenSource(timeout);
        try
        {
            await foreach (var evt in client.Events.ReadAllAsync(cts.Token))
            {
                if (evt is ReadyEvent ready)
                {
                    return ready;
                }
            }
        }
        catch (OperationCanceledException)
        {
            return null; // timed out waiting for ready - the caller reports it, never hangs
        }

        return null; // the stream ended before ready arrived
    }

    private void ResetSession()
    {
        Fake.Wall = "-";
        Fake.Zone = string.Empty;
        Real.Wall = "-";
        Real.Zone = string.Empty;
        MultiplierText = string.Empty;

        VerdictKnown = false;
        VerdictKind = VerdictKind.Unknown;
        VerdictLabelKey = "verdict.unknown";
        VerdictReasonKey = string.Empty;
        VerdictHasReason = false;
        VerdictMeaningKey = string.Empty;
        VerdictHasMeaning = false;
        ProcessCount = 0;

        CoverageKnown = false;
        _coverageCaptured = false;
        Covered = [];
        Observed = [];
        Uncovered = [];
        Warnings = [];
    }

    private static string FormatChannel(CoveredChannel channel) => $"{channel.Channel}  ×{channel.Calls}";

    /// <summary>Build the wire time from the inputs. The moment is the local time in the session zone
    /// (rule 2, chrono-mock 9.5); the core turns it into UTC and validates it (docs/08 section 5).</summary>
    internal TimeSpec BuildTime()
    {
        TryParseMoment(_momentText, out var canonical);
        return new TimeSpec
        {
            Moment = new MomentSpec { Kind = "absolute", Local = canonical, TzBiasMin = SelectedZone.BiasMinutes },
            Mode = SelectedMode.Mode,
            Multiplier = SelectedMode.Multiplier,
            ScaleDuration = false,
        };
    }

    // Accept an ISO moment with a 'T' or a space separator. This is a well-formed-ness check only - the
    // deep validation (DST gap, non-leap Feb 29, range) belongs to the core (docs/08 section 5).
    private static readonly string[] MomentFormats = ["yyyy-MM-ddTHH:mm:ss", "yyyy-MM-dd HH:mm:ss"];

    private static bool TryParseMoment(string? text, out string canonical)
    {
        canonical = string.Empty;
        if (DateTime.TryParseExact(
                text?.Trim(), MomentFormats, CultureInfo.InvariantCulture, DateTimeStyles.None, out var moment))
        {
            canonical = moment.ToString("yyyy-MM-ddTHH:mm:ss", CultureInfo.InvariantCulture);
            return true;
        }

        return false;
    }

    private void SetStatus(string key, SessionStatusKind kind)
    {
        StatusKey = key;
        StatusKind = kind;
    }

    private void SetVerdict(VerdictKind kind, string reasonKey)
    {
        VerdictKind = kind;
        VerdictLabelKey = VerdictKinds.LabelKey(kind);

        // The specific reason and the plain-language meaning are shown only for a non-works verdict - a
        // clean "works" needs no caveat. The core stays authoritative for the reason key (rules 15/16).
        var nonWorks = kind != VerdictKind.Works;
        VerdictReasonKey = reasonKey;
        VerdictHasReason = nonWorks && !string.IsNullOrEmpty(reasonKey);
        VerdictMeaningKey = VerdictKinds.MeaningKey(kind);
        VerdictHasMeaning = VerdictMeaningKey.Length > 0;
        VerdictKnown = true;
    }

    private static bool IsTerminal(SessionStatusKind kind) => kind
        is SessionStatusKind.Ended
        or SessionStatusKind.DidNotTakeEffect
        or SessionStatusKind.CoreStopped
        or SessionStatusKind.Error;
}

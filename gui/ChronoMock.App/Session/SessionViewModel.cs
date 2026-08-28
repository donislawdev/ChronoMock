using System.Collections.ObjectModel;
using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text;
using System.Threading.Channels;
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

    /// <summary>Idle watchdog window (M-10): with no core event for this long, the session is treated as
    /// hung. The core emits a <c>state</c> heartbeat every ~1 s of REAL time regardless of mode
    /// (flow/frozen/xN), so 15 s is 15 missed heartbeats - comfortably above the noise, and above the
    /// worst-case latency to the FIRST event (the core's prepare: launch + inject, itself bounded by
    /// INJECT_TIMEOUT), so a slow start never trips it.</summary>
    private static readonly TimeSpan IdleTimeout = TimeSpan.FromSeconds(15);

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
    private bool _scaleDuration;
    private readonly ISessionHistoryStore _store;
    private bool _launched;
    private bool _stopRequested;
    private string _historyError = string.Empty;

    /// <summary>Bare view-model: history is in-memory, so a default construction and unit tests touch no files.</summary>
    public SessionViewModel() : this(new InMemorySessionHistoryStore())
    {
    }

    public SessionViewModel(ISessionHistoryStore history)
    {
        _store = history;

        // Defaults match the moment/mode the panel shipped with before these inputs existed.
        _selectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == -120); // UTC+02:00
        _selectedMode = TimeInputs.Modes.First(m => m.Multiplier == 60);    // x60

        History.CollectionChanged += (_, _) => RaisePropertyChanged(nameof(HasHistory));
        foreach (var record in _store.Load())
        {
            History.Insert(0, record); // newest first for display
        }
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

    private bool _hasTiming;
    private long _elapsedRealMs;
    private long _elapsedFakeMs;
    private string _fakeEndWall = string.Empty;
    private string _vanishReasonKey = string.Empty;
    private long _livedMs;
    private string _copyFeedbackKey = string.Empty;

    public ClockView Fake { get; } = new("clock.fake");

    public ClockView Real { get; } = new("clock.real");

    /// <summary>Translation key for the current status label (the view renders it in the user's language).</summary>
    public string StatusKey { get => _statusKey; private set => Set(ref _statusKey, value); }

    public SessionStatusKind StatusKind
    {
        get => _statusKind;
        private set
        {
            if (Set(ref _statusKind, value))
            {
                RaisePropertyChanged(nameof(IsRunning));
                RaisePropertyChanged(nameof(CanCopySummary));
            }
        }
    }

    /// <summary>True while the session is live - the in-flight controls bind their visibility to this.</summary>
    public bool IsRunning => _statusKind == SessionStatusKind.Running;

    /// <summary>True once a session has started (running or finished) - there is then something to copy.
    /// The Copy summary button binds its visibility to this (chrono-mock 7.2, 8.8).</summary>
    public bool CanCopySummary => _statusKind is not (SessionStatusKind.Idle or SessionStatusKind.Connecting);

    /// <summary>The current rate as data, e.g. "x60" - empty until the first heartbeat.</summary>
    public string MultiplierText { get => _multiplierText; private set => Set(ref _multiplierText, value); }

    /// <summary>The raw failure detail when something went wrong - shown verbatim so a failure is never silent.</summary>
    public string LastError { get => _lastError; private set => Set(ref _lastError, value); }

    /// <summary>Translation key for the copy-summary feedback ("copy.done" / "copy.failed"), empty until a
    /// copy is attempted. A clipboard failure is surfaced, never swallowed (rule 6).</summary>
    public string CopyFeedbackKey { get => _copyFeedbackKey; private set => Set(ref _copyFeedbackKey, value); }

    /// <summary>Record the outcome of a clipboard copy so the panel can confirm it or report a failure.</summary>
    public void NoteCopy(bool ok) => CopyFeedbackKey = ok ? "copy.done" : "copy.failed";

    /// <summary>Past sessions, newest first, for the History panel (docs/04 section 6). Loaded from the
    /// injected store on construction and prepended as each session ends.</summary>
    public ObservableCollection<SessionRecord> History { get; } = [];

    /// <summary>True when there is at least one recorded session - the History panel binds its visibility here.</summary>
    public bool HasHistory => History.Count > 0;

    /// <summary>Raw detail when a session could not be written to history (e.g. a read-only drive), empty
    /// otherwise. Surfaced, never swallowed (rule 6).</summary>
    public string HistoryError
    {
        get => _historyError;
        private set { if (Set(ref _historyError, value)) { RaisePropertyChanged(nameof(HasHistoryError)); } }
    }

    /// <summary>True when a history write failed - the panel shows the reason (rule 6).</summary>
    public bool HasHistoryError => _historyError.Length > 0;

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

    /// <summary>Scale the duration axis too (chrono-mock 11.1 pt 4): with a multiplier, timers, sleeps and
    /// tick counts advance N times as well, so a countdown or animation runs N times faster - not just the
    /// wall clock. Off by default (the wall clock alone covers date-dependent behaviour); a duration-based
    /// target like a countdown needs it. Maps to the wire <c>scale_duration</c> the core already accepts.</summary>
    public bool ScaleDuration { get => _scaleDuration; set => Set(ref _scaleDuration, value); }

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
                _elapsedRealMs = s.ElapsedRealMs;
                _elapsedFakeMs = s.ElapsedFakeMs;
                _hasTiming = true;
                SetStatus("status.running", SessionStatusKind.Running);
                break;
            case VanishedEvent vd:
                _vanishReasonKey = vd.ReasonKey;
                _livedMs = vd.LivedMs;
                SetStatus("status.did_not_take_effect", SessionStatusKind.DidNotTakeEffect);
                break;
            // Guarded on the terminal state like `state` above (M-9): a late `ended`/`error` after a
            // terminal outcome (e.g. `ended` arriving right after `vanished` -> DidNotTakeEffect) must not
            // overwrite the honest "did not take effect" verdict in the summary and history.
            case EndedEvent e when !IsTerminal(StatusKind):
                if (e.FakeEndWall is not null)
                {
                    // The core's authoritative end timing (docs/08 section 6) - prefer it over the last heartbeat.
                    _fakeEndWall = e.FakeEndWall;
                    _elapsedRealMs = e.ElapsedRealMs;
                    _elapsedFakeMs = e.ElapsedFakeMs;
                    _hasTiming = true;
                }

                SetStatus("status.ended", SessionStatusKind.Ended);
                break;
            case ErrorEvent when !IsTerminal(StatusKind):
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

        // An Electron/Chromium target is not driven by the native core: its timer often lives in a
        // sandboxed renderer the hook cannot reach (ADR-8). Rather than start a native session that
        // would look like it worked without accelerating (untouchable rule 4), refuse honestly and hand
        // off the CLI command that does run it in Chromium mode. The GUI panel does not drive CDP yet.
        if (ChromiumTarget.IsChromium(TargetPath!))
        {
            LastError = ChromiumTarget.CliCommand(TargetPath!);
            SetStatus("status.chromium_use_cli", SessionStatusKind.Error);
            return;
        }

        Idle = false;
        _launched = false;
        _stopRequested = false;
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
            _launched = true; // the target is now running - this session will be recorded in history on exit
            SetStatus("status.running", SessionStatusKind.Running);

            var watchdogFired = await ConsumeEventsAsync(client.Events, IdleTimeout);

            // The event stream ended. Decide the final status by WHY it ended (M-10):
            if (_stopRequested)
            {
                // The user pressed Stop. Show that plainly even if the core managed a clean `ended` first -
                // the verdict and coverage already captured still stand and are shown separately.
                SetStatus("status.stopped", SessionStatusKind.Stopped);
            }
            else if (!IsTerminal(StatusKind))
            {
                // No terminal event arrived. Either the idle watchdog fired (the core stopped heartbeating)
                // or the core just closed its stdout on its own (docs/08 section 7). Either way the finally
                // stops the core, the hook self-detaches, and the target returns to real time - do not hang.
                SetStatus(
                    watchdogFired ? "status.core_unresponsive" : "status.core_stopped",
                    watchdogFired ? SessionStatusKind.CoreUnresponsive : SessionStatusKind.CoreStopped);
            }
        }
        catch (Exception ex) when (ex is FileNotFoundException or InvalidOperationException
                                       or UnauthorizedAccessException
                                       or System.ComponentModel.Win32Exception)
        {
            // Setup failed: a build artifact is missing, the repo root was not found, the target file is
            // unreadable (UnauthorizedAccessException from PeReader.File.OpenRead, M-5), or the core would
            // not spawn.
            LastError = ex.Message;
            SetStatus("status.core_missing", SessionStatusKind.Error);
        }
        catch (IOException ex)
        {
            // The protocol pipe broke mid-session.
            LastError = ex.Message;
            SetStatus("status.error", SessionStatusKind.Error);
        }
        catch (Exception ex)
        {
            // Anything else - most likely a malformed core event dereferenced in Apply (M-11). Surface it
            // as an honest error rather than let it escape `async void OnStartClick` and crash the UI thread.
            LastError = ex.Message;
            SetStatus("status.error", SessionStatusKind.Error);
        }
        finally
        {
            if (_launched)
            {
                // A session actually ran (the target launched) - record it with its final verdict.
                RecordSession();
            }

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

    /// <summary>
    /// Stop a running session on the user's request (M-10). Disposes the core client OFF the UI thread (it
    /// blocks up to ~2 s waiting for the core to end, then kills it): a healthy core ends cleanly and emits
    /// its final verdict, a hung one is killed. In both cases the hook self-detaches when the core dies, so
    /// the target returns to real time - we never kill the application under test. The read loop then
    /// completes and <see cref="StartAsync"/> records the session and sets the Stopped status. No-op unless a
    /// session is running - the Stop control is only shown then, but the guard keeps a stray call safe.
    /// </summary>
    public void RequestStop()
    {
        var client = _client;
        if (client is null || !IsRunning)
        {
            return;
        }

        _stopRequested = true;
        _ = Task.Run(() => client.DisposeAsync().AsTask());
    }

    /// <summary>
    /// Relay events until the stream ends, resetting an idle watchdog on each one. Returns <c>true</c> if the
    /// watchdog fired - no event for <paramref name="idleTimeout"/>, i.e. the core stopped emitting its ~1 s
    /// heartbeat and is treated as hung (M-10) - and <c>false</c> if the stream completed normally (the core
    /// exited or was disposed). Kept separate and <c>internal</c> so the watchdog is unit-tested with a fake
    /// channel and a short timeout, no core process needed. No ConfigureAwait, so <see cref="Apply"/> stays on
    /// the caller's (UI) thread.
    /// </summary>
    internal async Task<bool> ConsumeEventsAsync(ChannelReader<ChronoEvent> events, TimeSpan idleTimeout)
    {
        using var idleCts = new CancellationTokenSource();
        try
        {
            while (true)
            {
                idleCts.CancelAfter(idleTimeout); // (re)arm the idle window before each wait
                if (!await events.WaitToReadAsync(idleCts.Token))
                {
                    return false; // the stream completed - the core exited or was disposed (e.g. by Stop)
                }

                while (events.TryRead(out var evt))
                {
                    Apply(evt);
                }
            }
        }
        catch (OperationCanceledException)
        {
            return true; // the idle watchdog fired - no event within the window
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

        _hasTiming = false;
        _elapsedRealMs = 0;
        _elapsedFakeMs = 0;
        _fakeEndWall = string.Empty;
        _vanishReasonKey = string.Empty;
        _livedMs = 0;
        CopyFeedbackKey = string.Empty;
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
            ScaleDuration = _scaleDuration,
        };
    }

    /// <summary>
    /// Compose the paste-into-ticket session summary (chrono-mock 7.2, 8.8) in the interface language. It
    /// mirrors the CLI evidence export (crates/cli render_evidence): a session that is anything other than a
    /// clean "works" ALWAYS leads with an unreliable-evidence banner - evidence that hides doubt is worse
    /// than none. Pure over the view state, so it is unit tested with a fake translator; the caller supplies
    /// the key resolver (rule 15), never Application.Current directly, so this stays testable without WPF.
    /// </summary>
    public string BuildSummary(Func<string, string> translate)
    {
        ArgumentNullException.ThrowIfNull(translate);
        var sb = new StringBuilder();

        if (!IsReliable)
        {
            sb.Append(translate("report.unreliable_banner")).Append("\n\n");
        }

        sb.Append(translate("report.title")).Append('\n');
        sb.Append("  ").Append(translate("report.target")).Append(": ").Append(TargetName).Append('\n');

        // Verdict headline: a vanish is an honest non-effect first, then the family/parent verdict, else none.
        if (_statusKind == SessionStatusKind.DidNotTakeEffect)
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ")
              .Append(translate("report.did_not_take_effect")).Append('\n');
            if (_vanishReasonKey.Length > 0)
            {
                sb.Append("    ")
                  .Append(Fmt(translate("report.vanish_detail"), translate(_vanishReasonKey), _livedMs))
                  .Append('\n');
            }
        }
        else if (_verdictKnown)
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ").Append(translate(VerdictLabelKey));
            if (IsFamily)
            {
                sb.Append("  ").Append(Fmt(translate("report.processes"), _processCount));
            }

            sb.Append('\n');
            if (_verdictHasReason)
            {
                sb.Append("    ").Append(translate(VerdictReasonKey)).Append('\n');
            }

            if (_verdictHasMeaning)
            {
                sb.Append("    ").Append(translate(VerdictMeaningKey)).Append('\n');
            }
        }
        else
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ")
              .Append(translate("report.no_verdict")).Append('\n');
        }

        if (_hasTiming)
        {
            // Prefer the authoritative end wall from `ended`; fall back to the last heartbeat's fake clock.
            var fakeWall = _fakeEndWall.Length > 0 ? _fakeEndWall : Fake.Wall;
            sb.Append("  ").Append(translate("report.session")).Append(": ")
              .Append(Fmt(translate("report.session_reached"), fakeWall)).Append('\n');
            sb.Append("    ")
              .Append(Fmt(translate("report.elapsed"), Seconds(_elapsedRealMs), Seconds(_elapsedFakeMs)))
              .Append('\n');
        }

        // Channel names are raw API identifiers (not translated); warnings are keys the core raised.
        AppendList(sb, translate, "coverage.covered", _covered, translateItems: false);
        AppendList(sb, translate, "coverage.observed", _observed, translateItems: false);
        AppendList(sb, translate, "coverage.uncovered", _uncovered, translateItems: false);
        AppendList(sb, translate, "coverage.warnings", _warnings, translateItems: true);

        sb.Append("  ")
          .Append(Fmt(translate("report.requested"), _momentText, SelectedZone.Label, translate(SelectedMode.LabelKey)))
          .Append('\n');

        return sb.ToString();
    }

    /// <summary>A clean "works" session is the only reliable one; anything else must carry the unreliable
    /// banner in an export (chrono-mock 8.8), mirroring the CLI's session_is_reliable.</summary>
    private bool IsReliable => _verdictKind == VerdictKind.Works
                              && _statusKind != SessionStatusKind.DidNotTakeEffect;

    private static string Seconds(long ms) => (ms / 1000.0).ToString("0.0", CultureInfo.InvariantCulture);

    // Format a possibly-missing template safely: a resolver that returns the raw key (no placeholders)
    // leaves it unchanged, because string.Format ignores extra arguments when there are no holes to fill.
    private static string Fmt(string template, params object[] args)
        => string.Format(CultureInfo.InvariantCulture, template, args);

    private static void AppendList(
        StringBuilder sb, Func<string, string> translate, string headerKey,
        IReadOnlyList<string> items, bool translateItems)
    {
        if (items.Count == 0)
        {
            return;
        }

        sb.Append("  ").Append(translate(headerKey)).Append(" (").Append(items.Count).Append("):\n");
        foreach (var item in items)
        {
            sb.Append("    - ").Append(translateItems ? translate(item) : item).Append('\n');
        }
    }

    /// <summary>Build a history record from the current setup and the session's final verdict (docs/04
    /// section 6). Pure over the view state, so it is unit tested; the GUI's own clock is real (only the
    /// target is faked), so DateTime.UtcNow is the true end time.</summary>
    internal SessionRecord BuildRecord()
    {
        TryParseMoment(_momentText, out var canonical);
        return new SessionRecord
        {
            TargetPath = _targetPath ?? string.Empty,
            MomentLocal = canonical.Length > 0 ? canonical : _momentText,
            TzBiasMin = SelectedZone.BiasMinutes,
            Mode = SelectedMode.Mode,
            Multiplier = SelectedMode.Multiplier,
            Verdict = RecordedVerdict(),
            EndedAtUtc = DateTime.UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture),
        };
    }

    // A vanished session could not be audited, so it is recorded as "undetermined" - honest, never a faked
    // verdict (untouchable rule 4). Otherwise the per-session/family verdict kind maps to its wire string.
    private string RecordedVerdict() => _statusKind == SessionStatusKind.DidNotTakeEffect
        ? "undetermined"
        : _verdictKind switch
        {
            VerdictKind.Works => "works",
            VerdictKind.Partial => "partial",
            VerdictKind.Fails => "fails",
            _ => "undetermined",
        };

    /// <summary>Record the just-ended session: prepend it to the panel and persist it. A write failure is
    /// surfaced, never swallowed (rule 6, docs/04 section 7).</summary>
    internal void RecordSession()
    {
        var record = BuildRecord();
        History.Insert(0, record);
        while (History.Count > SessionHistoryLimits.Max)
        {
            History.RemoveAt(History.Count - 1); // keep the panel in step with the store's cap
        }

        try
        {
            _store.Append(record);
            HistoryError = string.Empty;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            HistoryError = ex.Message;
        }
    }

    /// <summary>Remove one past session from the panel and the store. Mild and left un-confirmed (zasady/13
    /// section 11) - it is a log entry, and a re-run re-creates one.</summary>
    public void RemoveFromHistory(SessionRecord record)
    {
        ArgumentNullException.ThrowIfNull(record);
        History.Remove(record);
        try
        {
            _store.Remove(record);
            HistoryError = string.Empty;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            HistoryError = ex.Message;
        }
    }

    /// <summary>Remove every past session from the panel and the store. The view confirms first (zasady/13
    /// section 11) - this method just performs it.</summary>
    public void ClearHistory()
    {
        History.Clear();
        try
        {
            _store.Clear();
            HistoryError = string.Empty;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            HistoryError = ex.Message;
        }
    }

    /// <summary>Repeat a past session by filling the setup form with its parameters. It never starts a
    /// session (untouchable rule 7, docs/04 section 6) and is ignored while one is running.</summary>
    public void LoadFromHistory(SessionRecord record)
    {
        ArgumentNullException.ThrowIfNull(record);

        // Ignore while a session is active - filling would clobber a live run's setup. Checked on both the
        // lifecycle flag (set by Start, covers Connecting) and the running status (set by state events), so
        // it holds however the state was reached. The History panel sits outside the disabled setup block.
        if (!_idle || IsRunning)
        {
            return;
        }

        SetTarget(record.TargetPath);
        MomentText = record.MomentLocal;
        SelectedZone = TimeInputs.Zones.FirstOrDefault(z => z.BiasMinutes == record.TzBiasMin) ?? SelectedZone;
        SelectedMode = TimeInputs.Modes.FirstOrDefault(
            m => m.Mode == record.Mode && m.Multiplier == record.Multiplier) ?? SelectedMode;
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
        or SessionStatusKind.Stopped
        or SessionStatusKind.CoreUnresponsive
        or SessionStatusKind.Error;
}

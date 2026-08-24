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
    private string _statusKey = "status.idle";
    private SessionStatusKind _statusKind = SessionStatusKind.Idle;
    private string _multiplierText = string.Empty;
    private string _lastError = string.Empty;
    private bool _canStart = true;
    private bool _verdictKnown;
    private VerdictKind _verdictKind = VerdictKind.Unknown;
    private string _verdictLabelKey = "verdict.unknown";
    private string _verdictReasonKey = string.Empty;
    private string _verdictMeaningKey = string.Empty;
    private bool _verdictHasReason;
    private bool _verdictHasMeaning;
    private int _processCount;

    public ClockView Fake { get; } = new("clock.fake");

    public ClockView Real { get; } = new("clock.real");

    /// <summary>Translation key for the current status label (the view renders it in the user's language).</summary>
    public string StatusKey { get => _statusKey; private set => Set(ref _statusKey, value); }

    public SessionStatusKind StatusKind { get => _statusKind; private set => Set(ref _statusKind, value); }

    /// <summary>The current rate as data, e.g. "x60" - empty until the first heartbeat.</summary>
    public string MultiplierText { get => _multiplierText; private set => Set(ref _multiplierText, value); }

    /// <summary>The raw failure detail when something went wrong - shown verbatim so a failure is never silent.</summary>
    public string LastError { get => _lastError; private set => Set(ref _lastError, value); }

    /// <summary>True when a new session may be started (no session is currently running).</summary>
    public bool CanStart { get => _canStart; private set => Set(ref _canStart, value); }

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
            default:
                // ready, coverage, ack do not change the panel here (coverage detail is slice 3.2-IV).
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

        CanStart = false;
        LastError = string.Empty;
        ResetSession();
        SetStatus("status.connecting", SessionStatusKind.Connecting);

        CoreClient? client = null;
        try
        {
            var demo = DemoSession.Resolve();
            client = CoreClient.Connect(demo.CorePath);
            _client = client;

            var ready = await ReadReadyAsync(client, ReadyTimeout);
            if (ready is null)
            {
                SetStatus("status.no_ready", SessionStatusKind.Error);
                return;
            }

            var gate = HandshakeGate.Check(ready, ProtocolJson.ProtocolVersion, demo.Machine);
            if (!gate.IsOk)
            {
                // Refuse before the target is ever launched (docs/08 section 3, zasady/13 section 11).
                SetStatus(gate.ReasonKey!, SessionStatusKind.Error);
                return;
            }

            client.Send(demo.Start);
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

            CanStart = true;
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

using System.Collections.ObjectModel;
using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text;
using System.Threading.Channels;
using ChronoMock.App.Calc;
using ChronoMock.App.Localization;
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
/// raised there. The core stays the single source of truth for the clocks (untouchable rule 2) - the panel
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
    internal static readonly TimeSpan IdleTimeout = TimeSpan.FromSeconds(15);

    /// <summary>The start command uses id 1 (see <see cref="SessionPlan"/>) - in-flight commands (jump,
    /// set_multiplier) take ids from here up. An error's id tells the two apart (RELEASE-001): an id at or
    /// above this answers one of OUR in-flight commands, while id 1 (or none) is the response to the start
    /// command or an unsolicited failure - a start/fatal error, never an in-flight rejection.</summary>
    private const long FirstInFlightCommandId = 10;

    private CoreSession? _session;
    private long _nextCommandId = FirstInFlightCommandId;
    private string _statusKey = "status.idle";
    private SessionStatusKind _statusKind = SessionStatusKind.Idle;
    private string _multiplierText = string.Empty;
    private string _lastError = string.Empty;
    private bool _idle = true;
    private string? _targetPath;
    private RecentTarget? _selectedTarget;
    private readonly CalcClient? _calcClient;
    private readonly string? _presetsDir;
    private readonly ScenarioPicker _scenarios = new();
    private ScenarioItem? _selectedScenario;
    private string _scenarioExplains = string.Empty;
    private string _scenarioErrorKey = string.Empty;
    private bool _applyingScenario; // guard: filling the moment from a scenario must not clear the selection
    private bool _momentIsDefault = true;
    /// <summary>The editable moment (a date and optional time in the session zone, rule 2). The shared
    /// MomentInput control binds to it, and MomentParse composes it culture-invariantly (locale-safe).</summary>
    public MomentField Moment { get; } = new();
    private ZoneOption _selectedZone;
    private ModeOption _selectedMode;
    private bool _scaleDuration;
    private bool _scaleQpc;
    private bool _forceStart;
    private string _targetArgs = string.Empty;
    private string _workingFolder = string.Empty;
    private readonly ISessionHistoryStore _store;
    private readonly IDiagnosticsLog _diagnosticsLog;
    private bool _launched;
    private bool _stopRequested;
    private string _historyError = string.Empty;
    private string _historyNoteKey = string.Empty;
    private SessionRecord? _selectedRecord;
    // Snapshot of the start setup, taken at Start, so history and the summary record what was REQUESTED
    // even after the form is changed (rule 4 - the record is the start).
    //
    // 🔴 The target and the zone are in here for a REASON that the moment and the mode do not have. Those
    // two can change IN FLIGHT (the moment becomes a jump, the mode a set_multiplier), which is the case the
    // first two fields were written for. The target and the zone cannot - they are start-only, and the drop
    // handler and every setup control gate on IsIdle. What they CAN do is change AFTER the session ends,
    // because the form unlocks while the summary is still copyable (CanCopySummary holds for every status
    // but Idle and Connecting). Reading them live then produced a report that carried one session's verdict
    // under another session's target name and zone - evidence naming the wrong application, in the one
    // artifact that leaves this tool and lands in somebody else's ticket.
    //
    // Null means "no session has started yet", and every reader falls back to the live value for it, so a
    // view model that never ran (a unit test, a fresh window) reads exactly as it did before.
    private string _startMomentText = string.Empty;
    private ModeOption? _startMode;
    private string? _startTargetPath;
    private ZoneOption? _startZone;
    // The remaining five inputs, kept as plain values rather than a setup type: this class sits exactly on
    // its class-coupling ceiling (CA1506 = 82, gui/CodeMetricsConfig.txt), so a new type here would redden
    // the metrics gate. Strings and bools cost nothing there.
    // True once Start has taken the snapshot. An explicit flag rather than a null check on one of the
    // fields: three of them are bools with no null to test, and inferring "was a snapshot taken" from a
    // sibling field is the kind of coupling that stops holding the moment one of them changes type.
    private bool _startCaptured;
    private string _startTargetArgs = string.Empty;
    private string _startWorkingFolder = string.Empty;
    private bool _startScaleDuration;
    private bool _startScaleQpc;
    private bool _startForce;
    private string _inFlightErrorKey = string.Empty;
    private bool _applyingMultiplier; // guard: syncing the Mode dropdown from a state event must not re-send

    /// <summary>Bare view-model: history is in-memory and the diagnostics log is a no-op, so a default
    /// construction and unit tests touch no files.</summary>
    public SessionViewModel() : this(new InMemorySessionHistoryStore())
    {
    }

    public SessionViewModel(
        ISessionHistoryStore history,
        IDiagnosticsLog? diagnosticsLog = null,
        CalcClient? calcClient = null,
        string? presetsDir = null)
    {
        _store = history;
        _diagnosticsLog = diagnosticsLog ?? new NoOpDiagnosticsLog();
        _calcClient = calcClient;
        _presetsDir = presetsDir;

        // 🔴 UTC, changed 2026-09-12, and UTC+02:00 before that. The old default was one market's summer
        // time and read on a first run as somebody's local clock without saying whose - while the list it
        // comes from covers Poland and the United States only, so for most of the world nothing in it is
        // right and an arbitrary default is worse than a neutral one. UTC is deterministic, universally
        // understood, and does not pretend to be anybody's.
        //
        // It also makes the default MOMENT true: 2038-01-19T03:14:07 is the 32-bit boundary in UTC, and at
        // +02:00 it was two hours past it. The two defaults now agree with each other.
        _selectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == 0);

        // 🔴 FLOWING, not x60, changed 2026-09-12 on the owner's decision. This tool's promise is that an
        // application sees a different DATE - running it sixty times faster as well is the second feature,
        // and defaulting to it meant a first run got an effect it never asked for. It was never hidden:
        // the sentence above Start has always named the rate. It was simply a surprise, and a surprise on
        // a first run is a cost paid by everyone once.
        _selectedMode = TimeInputs.Modes.First(m => m.Mode == "flow");

        // The relative line fills the same field the At row edits, and reads the zone at the moment of use,
        // so changing the zone changes what "now plus one day" means without any wiring between the two.
        Relative = new RelativeMomentViewModel(Moment, calcClient, () => SelectedZone.BiasMinutes);

        // Ship with the same default moment the panel had before these inputs existed.
        Moment.LoadCanonical("2038-01-19T03:14:07");
        Moment.Changed += (_, _) =>
        {
            RaisePropertyChanged(nameof(CanStart));
            RaiseStartRefusalChanged();
            RaiseMomentPreviewChanged();

            // 🔴 The shipped date stops explaining itself the moment it is no longer the shipped date.
            // Unconditional, unlike the scenario clearing below: a moment filled FROM a scenario is not
            // the default either, it is that scenario's.
            if (_momentIsDefault)
            {
                _momentIsDefault = false;
                RaisePropertyChanged(nameof(MomentIsDefault));
            }

            // A hand-edited moment is no longer the scenario's moment, so the selection stops claiming it
            // is (the calculator's active-preset banner clears the same way). Guarded, because filling the
            // field FROM a scenario raises this too.
            if (!_applyingScenario)
            {
                ClearScenarioSelection();
            }
        };

        History.CollectionChanged += (_, _) => RaisePropertyChanged(nameof(HasHistory));
        RecentTargets.CollectionChanged += (_, _) => RaisePropertyChanged(nameof(HasRecentTargets));
        foreach (var record in _store.Load())
        {
            History.Insert(0, record); // newest first for display
        }

        SeedRecentTargets();

        // Reading the catalogue is file I/O only - no process is spawned here. Evaluating a scenario does
        // spawn the engine, and that happens on a click, never in a constructor (a window built in a test
        // must start nothing).
        if (_presetsDir is not null)
        {
            _scenarios.Load(_presetsDir);
        }

        // The panel's actions, lifted off this view model so it holds state and they hold behaviour (GUI
        // rule 15). Built last, once Relative and the scenario picker exist for the commands to reach. The
        // window that hosts the phases attaches a shell to these later (SessionCommands.AttachShell) - this
        // view model never sees it (rule 16).
        Commands = new SessionCommands(this);
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
    /// <summary>The pid of the process whose call counts the panel shows - the first one reported,
    /// which is the parent. Later events for it replace its counts - other pids never do (R2-X8).</summary>
    private uint? _parentPid;
    private bool _isCdp;
    private IReadOnlyList<CoveredChannel> _covered = [];
    private IReadOnlyList<CoveredChannel> _observed = [];
    private IReadOnlyList<string> _uncovered = [];
    private IReadOnlyList<string> _unobserved = [];
    private IReadOnlyList<string> _installedLate = [];
    private IReadOnlyList<string> _warnings = [];

    private bool _hasTiming;
    private long _elapsedRealMs;
    private long _elapsedFakeMs;
    private string _fakeEndWall = string.Empty;
    private string _vanishReasonKey = string.Empty;
    private long _livedMs;
    // Final-state facts the core reports in `ended` (RELEASE audit): the target's own exit code (native
    // sessions where the app exited on its own - null for Stop/end and for CDP) and any cleanup residue it
    // could not remove (today only a CDP temp profile). Surfaced honestly, never dropped (rule 6).
    private int? _targetExitCode;
    private IReadOnlyList<string> _residueKeys = [];
    // Diagnostics captured when a session ends in anything but a clean success (RELEASE-012): the core's
    // stderr and parse errors, composed into a block the user can copy and that is also written to a log
    // file beside the exe. Empty on a clean works session, so the button and log stay out of the happy path.
    private string _diagnosticsText = string.Empty;
    private string _diagnosticsSavedPath = string.Empty;
    private string _copyFeedbackKey = string.Empty;

    /// <summary>The panel's actions as bindable commands, so the phase views bind <c>Command</c> and carry
    /// no Click handler of their own (GUI rules 11 and 15). The window-dependent actions (pickers, clipboard,
    /// confirm, the support link) join them in the next slice through a shell service.</summary>
    public SessionCommands Commands { get; }

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
                RaisePropertyChanged(nameof(CanEditTime));
                RaisePropertyChanged(nameof(ShowsSessionControls));
                RaisePropertyChanged(nameof(ShowsInFlightError));
                RaisePropertyChanged(nameof(IsAuditPending));
                RaisePropertyChanged(nameof(AuditNeverArrived));
                RaisePropertyChanged(nameof(AuditNeverStarted));
                RaiseResultChanged();
                RaisePropertyChanged(nameof(HasVanishReason));
                RaisePropertyChanged(nameof(ShowsSetupPhase));
                RaisePropertyChanged(nameof(ShowsSessionPhase));
                RaisePropertyChanged(nameof(ShowsResultPhase));
            }
        }
    }

    /// <summary>The result phase's headline and what hangs under it follow the status and the verdict alike.</summary>
    private void RaiseResultChanged()
    {
        RaisePropertyChanged(nameof(ResultHeadlineKey));
        RaisePropertyChanged(nameof(ResultKind));
        RaisePropertyChanged(nameof(ResultHasReason));
        RaisePropertyChanged(nameof(ResultHasMeaning));
        RaisePropertyChanged(nameof(ResultHasEnding));
        RaisePropertyChanged(nameof(AuditExplainsVerdict));
        RaisePropertyChanged(nameof(AuditExplainsMeaning));
        RaisePropertyChanged(nameof(AuditStartsOpen));
    }

    /// <summary>True while the session is live - the in-flight controls bind their visibility to this.</summary>
    public bool IsRunning => _statusKind == SessionStatusKind.Running;

    /// <summary>
    /// The session phase's controls are still worth showing: the session is live, or on its way in or out.
    /// </summary>
    /// <remarks>
    /// 🔴 NOT <see cref="IsRunning"/>. An ended session kept the whole control card on screen, disabled, with a
    /// Stop that could stop nothing - nine buttons offered to a reader who could press none of them. They go
    /// once the session is over. Stopping is not over: the reader has just pressed Stop, and the card stays,
    /// disabled, until the core confirms, rather than vanishing under the pointer.
    /// </remarks>
    public bool ShowsSessionControls => !IsTerminal(_statusKind);

    /// <summary>
    /// Which of the three phases the window shows. Exactly one is true for any status, and the three are
    /// defined against each other so a status added later still lands somewhere rather than showing nothing.
    /// </summary>
    /// <remarks>
    /// 🔴 THE PHASE IS THE STATUS, NOT <see cref="IsIdle"/>. A finished session sets <see cref="IsIdle"/>
    /// true again - the setup form unlocks so the tester can edit and re-run - so a selector reading IsIdle
    /// would throw the reader back to the setup screen the instant a session ended, losing the verdict the
    /// session was run to produce. The status stays terminal until a new session starts, which is what the
    /// result phase is keyed to.
    /// </remarks>
    public bool ShowsSetupPhase => _statusKind == SessionStatusKind.Idle;

    /// <summary>See <see cref="ShowsSetupPhase"/> - the result phase owns every terminal status.</summary>
    public bool ShowsResultPhase => IsTerminal(_statusKind);

    /// <summary>See <see cref="ShowsSetupPhase"/> - a session on its way in, live, or shutting down.</summary>
    public bool ShowsSessionPhase => !ShowsSetupPhase && !ShowsResultPhase;

    /// <summary>True once a session has started (running or finished) - there is then something to copy.
    /// The Copy summary button binds its visibility to this (chrono-mock 7.2, 8.8).</summary>
    public bool CanCopySummary => _statusKind is not (SessionStatusKind.Idle or SessionStatusKind.Connecting);

    /// <summary>The current rate as data, e.g. "x60" - empty until the first heartbeat.</summary>
    public string MultiplierText { get => _multiplierText; private set => Set(ref _multiplierText, value); }

    /// <summary>The raw failure detail when something went wrong - shown verbatim so a failure is never silent.</summary>
    public string LastError
    {
        get => _lastError;
        private set { if (Set(ref _lastError, value)) { RaisePropertyChanged(nameof(HasLastError)); } }
    }

    /// <summary>Translation key for a per-command in-flight error (e.g. an invalid jump moment), empty when
    /// none. Unlike a fatal error it does NOT end the session - the core rejected one command and kept
    /// running, so the panel surfaces it and stays live (rule 6).</summary>
    public string InFlightErrorKey
    {
        get => _inFlightErrorKey;
        private set
        {
            if (Set(ref _inFlightErrorKey, value))
            {
                RaisePropertyChanged(nameof(HasInFlightError));
                RaisePropertyChanged(nameof(ShowsInFlightError));
            }
        }
    }

    /// <summary>True when a per-command in-flight error is being shown.</summary>
    public bool HasInFlightError => _inFlightErrorKey.Length > 0;

    /// <summary>The in-flight error, for as long as the session it is about can still act on it.</summary>
    /// <remarks>A refused command from a session that has since ended explains nothing on the session phase,
    /// where it would sit under "Session ended" saying the clock did not move. The shipped panel binds
    /// <see cref="HasInFlightError"/> and keeps its own behaviour.</remarks>
    public bool ShowsInFlightError => HasInFlightError && !IsTerminal(_statusKind);

    /// <summary>Translation key for the copy-summary feedback ("copy.done" / "copy.failed"), empty until a
    /// copy is attempted. A clipboard failure is surfaced, never swallowed (rule 6).</summary>
    public string CopyFeedbackKey
    {
        get => _copyFeedbackKey;
        private set { if (Set(ref _copyFeedbackKey, value)) { RaisePropertyChanged(nameof(HasCopyFeedback)); } }
    }

    /// <summary>True while a copy outcome is on show - the line is absent, not blank, until a copy is attempted.</summary>
    public bool HasCopyFeedback => _copyFeedbackKey.Length > 0;

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

    /// <summary>Translation key for what a history LOAD could not fill in, empty when it filled everything.
    /// Separate from <see cref="HistoryError"/>, which is about a failed write and is rendered after a
    /// "could not save" lead-in that would be wrong here.</summary>
    public string HistoryNoteKey
    {
        get => _historyNoteKey;
        private set { if (Set(ref _historyNoteKey, value)) { RaisePropertyChanged(nameof(HasHistoryNote)); } }
    }

    /// <summary>True when the last history load left a field it could not fill.</summary>
    public bool HasHistoryNote => _historyNoteKey.Length > 0;

    /// <summary>The recorded session chosen in the history well, or null. Choosing one does nothing by itself
    /// (untouchable rule 7) - the actions under the well act on it, and setting it up again fills the form and
    /// never starts a session.</summary>
    public SessionRecord? SelectedRecord
    {
        get => _selectedRecord;
        set { if (Set(ref _selectedRecord, value)) { RaisePropertyChanged(nameof(HasSelectedRecord)); } }
    }

    /// <summary>True while a recorded session is chosen - the actions that need one bind their IsEnabled here.</summary>
    public bool HasSelectedRecord => _selectedRecord is not null;

    /// <summary>Path to the target executable to run, chosen by the user (or a bundled default in dev).</summary>
    public string? TargetPath
    {
        get => _targetPath;
        private set
        {
            if (Set(ref _targetPath, value))
            {
                PromoteRecentTarget(value);
                RaisePropertyChanged(nameof(TargetName));
                RaisePropertyChanged(nameof(HasTarget));
                RaisePropertyChanged(nameof(ShowsDropHint));
                RaisePropertyChanged(nameof(CanStart));
                RaiseStartRefusalChanged();
            }
        }
    }

    /// <summary>Targets used before, newest first (chrono-mock 7.1 pt 1). Seeded from the session history
    /// on construction and re-ordered as targets are chosen, so the picker only has to be opened for a
    /// target this machine has never run.
    /// <para>
    /// It is a VIEW over the history, held in memory: deleting or clearing history does not empty it (that
    /// would blank the current selection mid-setup), and a target chosen but never launched lives for this
    /// run only - history records a session, and a session that never started is not one.
    /// </para></summary>
    public ObservableCollection<RecentTarget> RecentTargets { get; } = [];

    /// <summary>True when there is at least one recent target - the dropdown binds its visibility here, so
    /// a first run with no history shows the bare Choose… button instead of an empty control.</summary>
    public bool HasRecentTargets => RecentTargets.Count > 0;

    /// <summary>The dropdown's selection: the current target, always present in <see cref="RecentTargets"/>.
    /// Setting it CHOOSES that target and nothing else - it never starts a session (untouchable rule 7).
    /// A null write is ignored: WPF clears the selection while the list is re-ordered, and that must not
    /// read as "the user unchose the target".</summary>
    public RecentTarget? SelectedTarget
    {
        get => _selectedTarget;
        set
        {
            if (value is not null && !RecentTarget.SamePath(value.FullPath, _targetPath ?? string.Empty))
            {
                SetTarget(value.FullPath); // one path in: promotion and every dependent property follow
            }
        }
    }

    /// <summary>The chosen target's file name for display, empty when none is chosen.</summary>
    public string TargetName => _targetPath is null ? string.Empty : Path.GetFileName(_targetPath);

    /// <summary>True once a target has been chosen - Start stays disabled until then.</summary>
    public bool HasTarget => _targetPath is not null;

    /// <summary>Whether to show the "or drop an application here" hint: only until a target is set, after
    /// which the hint has done its job and would be noise on every later run (chrono-mock 7.1 pt 1). Drag
    /// and drop stays available either way.</summary>
    public bool ShowsDropHint => _targetPath is null;

    /// <summary>
    /// The moment about to be handed to the target, written out the way a person reads a date
    /// (chrono-mock 7.1 pt 3): "Tuesday, 31 December 2027, 23:59:50 (-05:00)".
    /// <para>
    /// The panel's own inputs are deliberately locale-invariant ISO, because a machine in one locale and a
    /// test VM in another must not read the same typed date differently. That safety costs readability -
    /// "2027-12-31" does not tell you it is a Tuesday, and the weekday is often the whole point of the
    /// test. This line pays it back WITHOUT weakening the input: it is output only, and it is where a
    /// mistyped year or an off-by-one month becomes visible before the application starts.
    /// </para>
    /// <para>
    /// Formatted in the interface language (the weekday name is interface text, not data), and always
    /// carries the session zone explicitly - a bare date would be the exact ambiguity rule 2 exists to
    /// prevent.
    /// </para>
    /// </summary>
    public string MomentPreview
        => Moment.IsValid ? ClockView.FormatMoment(Moment.Canonical, _selectedZone.Label) : string.Empty;

    /// <summary>
    /// True while the date field still holds the moment this build ships with, and nobody has touched it.
    /// </summary>
    /// <remarks>
    /// 🔴 It exists so the screen can SAY where that date came from. A field pre-filled with a date in
    /// 2038 looks arbitrary to somebody opening the tool for the first time, and an unexplained value is
    /// a value nobody trusts. It is the 32-bit time boundary and worth suggesting - it just has to admit
    /// as much, and only for as long as it is still true.
    ///
    /// A bool rather than a type: this class sits near its coupling ceiling, and a bool costs nothing
    /// there (gui/CodeMetricsConfig.txt).
    /// </remarks>
    public bool MomentIsDefault => _momentIsDefault;

    /// <summary>Whether the preview line has something to say. False for a moment that does not parse -
    /// the validation message takes that line instead, so the two never appear together and the row
    /// never changes height.</summary>
    public bool HasMomentPreview => MomentPreview.Length > 0;

    private void RaiseMomentPreviewChanged()
    {
        RaisePropertyChanged(nameof(MomentPreview));
        RaisePropertyChanged(nameof(HasMomentPreview));
        // The started-at fact falls back to the form until a session has captured its snapshot.
        RaisePropertyChanged(nameof(StartedAtPreview));
    }


    /// <summary>The session-zone options (fixed offsets, MVP markets).</summary>
    public IReadOnlyList<ZoneOption> Zones => TimeInputs.Zones;

    /// <summary>The time-mode options (flowing, frozen, xN).</summary>
    public IReadOnlyList<ModeOption> Modes => TimeInputs.Modes;

    public ZoneOption SelectedZone
    {
        get => _selectedZone;
        // The preview names the zone, so changing the zone rewrites the line - the same moment in a
        // different zone is a different thing for the target to see (rule 2).
        set { if (Set(ref _selectedZone, value)) { RaiseMomentPreviewChanged(); } }
    }

    public ModeOption SelectedMode
    {
        get => _selectedMode;
        set
        {
            if (Set(ref _selectedMode, value) && IsRunning && !_applyingMultiplier && value is not null)
            {
                // Live: changing the mode while running sends set_multiplier (flow -> x1, frozen -> x0,
                // xN -> N). The guard skips the send when we are only syncing the dropdown from a state
                // event (SyncModeToMultiplier), so a heartbeat never bounces back as a command.
                SendMultiplier(value.Mode switch { "frozen" => 0, "flow" => 1, _ => value.Multiplier ?? 1 });
            }
        }
    }

    /// <summary>Reflect the live multiplier in the Mode dropdown when it matches a preset, so the control
    /// does not drift from reality after a preset button or custom-speed change. A custom value with no
    /// matching preset leaves the dropdown as-is (<see cref="MultiplierText"/> shows the true speed).</summary>
    private void SyncModeToMultiplier(long multiplier)
    {
        var match = TimeInputs.Modes.FirstOrDefault(
            m => (m.Mode switch { "frozen" => 0L, "flow" => 1L, _ => m.Multiplier ?? 1 }) == multiplier);
        if (match is not null && !ReferenceEquals(match, _selectedMode))
        {
            _applyingMultiplier = true;
            SelectedMode = match;
            _applyingMultiplier = false;
        }
    }

    /// <summary>Scale the duration axis too (chrono-mock 11.1 pt 4): with a multiplier, timers, sleeps and
    /// tick counts advance N times as well, so a countdown or animation runs N times faster - not just the
    /// wall clock. Off by default (the wall clock alone covers date-dependent behaviour) - a duration-based
    /// target like a countdown needs it. Maps to the wire <c>scale_duration</c> the core already accepts.</summary>
    public bool ScaleDuration { get => _scaleDuration; set => Set(ref _scaleDuration, value); }

    /// <summary>Also scale QueryPerformanceCounter (ADR-2 reversal, opt-in). QPC backs the monotonic/elapsed
    /// clock of Python 3.13+ (monotonic/perf_counter), .NET (Stopwatch) and Java (nanoTime) - with this on,
    /// a timer built on those accelerates too. SEPARATE from ScaleDuration because scaling QPC can distort a
    /// target that times its rendering off QPC. Off by default (ADR-2). Maps to the wire <c>scale_qpc</c>.</summary>
    public bool ScaleQpc { get => _scaleQpc; set => Set(ref _scaleQpc, value); }

    /// <summary>Run even when the opening verdict says the substitution did not take effect. Off by
    /// default: the core stops the target in that case, because an application that looks time-shifted
    /// but is not produces evidence about a session that never happened. Start-only.</summary>
    public bool ForceStart { get => _forceStart; set => Set(ref _forceStart, value); }

    /// <summary>Command-line arguments for the target (chrono-mock 7.1 pt 1), as one line the tester types.
    /// Split into the wire's argument list by <see cref="TargetArguments"/>, which mirrors the CLI's own
    /// rule so <c>--args</c> and this field cannot launch the same application two different ways.
    /// Start-only: arguments are fixed once the process exists.</summary>
    public string TargetArgs
    {
        get => _targetArgs;
        set
        {
            if (Set(ref _targetArgs, value))
            {
                RaisePropertyChanged(nameof(HasTargetArgs));
            }
        }
    }

    /// <summary>Whether anything was typed in the arguments field.</summary>
    /// <remarks>
    /// 🔴 It exists because the launch fields now live INSIDE the speed section, and a folded section has
    /// to say what is set inside it or folding becomes hiding. The header shows a chip rather than the
    /// text: an argument list is as long as somebody makes it, and a summary that grew with it would push
    /// the rate and the option chips off the right-hand side. A bool, not a type - this class stands on
    /// its coupling ceiling (gui/CodeMetricsConfig.txt).
    /// </remarks>
    public bool HasTargetArgs => _targetArgs.Length > 0;

    /// <summary>Working folder for the target (chrono-mock 7.1 pt 1). Empty means "do not ask for one", and
    /// the target then inherits ours - the behaviour every session had before this field existed. The wire
    /// and the mechanism have carried <c>cwd</c> since the protocol was written - only the two surfaces
    /// never offered it. Start-only.</summary>
    public string WorkingFolder
    {
        get => _workingFolder;
        set
        {
            if (Set(ref _workingFolder, value))
            {
                RaisePropertyChanged(nameof(HasWorkingFolder));
            }
        }
    }

    /// <summary>Whether a working folder was named. See <see cref="HasTargetArgs"/> for why it is a chip
    /// in the folded header rather than the path itself - a path is even longer than an argument list.</summary>
    public bool HasWorkingFolder => _workingFolder.Length > 0;

    /// <summary>True when a session may be started: nothing is running, a target is chosen, moment is valid.</summary>
    public bool CanStart => _idle && HasTarget && Moment.IsValid;

    /// <summary>
    /// Why Start is refusing, as a translation key, or empty when it is not refusing.
    /// </summary>
    /// <remarks>
    /// 🔴 ONE REASON FOR EVERY REFUSAL, because the screen used to have one for exactly one of the three.
    /// The sentence above Start was bound to "no application chosen", so with an application picked and an
    /// unreadable date the footer lost the contract line as well (there is no moment to promise) and
    /// printed nothing at all: a disabled button with no account of itself, which is the one thing this
    /// phase is not allowed. At the window's floor the field's own red message is off-screen, so the
    /// footer was the only place left to say it and it was silent.
    ///
    /// Total by construction - every term of <see cref="CanStart"/> has a branch here, in the order the
    /// reader meets them. A gap would put the hole straight back.
    ///
    /// A string rather than a type: this class stands on its coupling ceiling (gui/CodeMetricsConfig.txt).
    /// </remarks>
    public string StartRefusalKey =>
        !_idle ? "setup.already_running"
        : !HasTarget ? "setup.needs_target"
        : !Moment.IsValid ? "setup.needs_moment"
        : string.Empty;

    /// <summary>Whether <see cref="StartRefusalKey"/> has something to say. Exactly the negation of
    /// <see cref="CanStart"/>, and asserted to be so - the two are read by one footer and must never
    /// disagree about whether a session can begin.</summary>
    public bool HasStartRefusal => StartRefusalKey.Length > 0;

    private void RaiseStartRefusalChanged()
    {
        RaisePropertyChanged(nameof(StartRefusalKey));
        RaisePropertyChanged(nameof(HasStartRefusal));
    }

    /// <summary>Choose the target executable to run (from the picker, the recent list, or the dev default).</summary>
    public void SetTarget(string path) => TargetPath = path;

    /// <summary>Fill the recent list from the session history: distinct target paths, newest first, capped.
    /// A record with no target path is skipped - <see cref="BuildRecord"/> writes an empty string when a
    /// session somehow ended without one, and an entry that cannot be run is not a shortcut.</summary>
    private void SeedRecentTargets()
    {
        foreach (var record in History) // already newest first
        {
            if (RecentTargets.Count >= RecentTargetLimits.Max)
            {
                break;
            }

            var path = record.TargetPath;
            if (!string.IsNullOrWhiteSpace(path) && FindRecentTarget(path) is null)
            {
                RecentTargets.Add(new RecentTarget(path));
            }
        }
    }

    /// <summary>Move the chosen target to the head of the recent list (adding it when it is new) and make
    /// it the dropdown's selection. Ordering matters: the selection is published BEFORE the list is
    /// trimmed, so the entry dropped off the tail can never be the one the control has selected.</summary>
    private void PromoteRecentTarget(string? path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            _selectedTarget = null;
            RaisePropertyChanged(nameof(SelectedTarget));
            return;
        }

        var entry = FindRecentTarget(path);
        if (entry is null)
        {
            entry = new RecentTarget(path);
            RecentTargets.Insert(0, entry);
        }
        else
        {
            var index = RecentTargets.IndexOf(entry);
            if (index > 0)
            {
                RecentTargets.Move(index, 0);
            }
        }

        _selectedTarget = entry;
        RaisePropertyChanged(nameof(SelectedTarget));

        while (RecentTargets.Count > RecentTargetLimits.Max)
        {
            RecentTargets.RemoveAt(RecentTargets.Count - 1); // the oldest, never the selected head
        }
    }

    private RecentTarget? FindRecentTarget(string path)
        => RecentTargets.FirstOrDefault(t => RecentTarget.SamePath(t.FullPath, path));

    /// <summary>
    /// Re-check which recent targets still exist, so a wiped build output is marked instead of silently
    /// offered (rule 6). Called when the dropdown opens.
    /// <para>
    /// The check runs OFF the UI thread on purpose: <see cref="File.Exists"/> on an unreachable network
    /// path blocks for as long as the share takes to fail, which on the UI thread is a frozen window. Only
    /// the flags are written back here, on the UI thread - the collection itself is never touched, so no
    /// selection is disturbed by opening the list.
    /// </para>
    /// </summary>
    public async Task RefreshRecentTargetsAsync()
    {
        var paths = RecentTargets.Select(t => t.FullPath).ToArray();
        if (paths.Length == 0)
        {
            return;
        }

        // File.Exists answers false for any failure (missing, denied, malformed) and never throws.
        var present = await Task.Run(() =>
        {
            var map = new Dictionary<string, bool>(StringComparer.OrdinalIgnoreCase);
            foreach (var path in paths)
            {
                map[path] = File.Exists(path);
            }

            return map;
        });

        foreach (var target in RecentTargets)
        {
            if (present.TryGetValue(target.FullPath, out var exists))
            {
                target.IsMissing = !exists;
            }
        }
    }

    /// <summary>
    /// The scenario list this session offers, and the text that narrows it (chrono-mock 7.1 pt 2).
    /// </summary>
    /// <remarks>
    /// A type of its own so the filter has somewhere to live: this class stood exactly on its coupling
    /// ceiling, and the catalogue leaving took more types out than the picker brought in. See
    /// <see cref="ScenarioPicker"/> for why the SELECTION stayed behind here.
    /// </remarks>
    public ScenarioPicker ScenarioPicker => _scenarios;

    /// <summary>The chosen scenario. Setting it computes its moment and fills the date - and nothing else:
    /// it never starts a session (untouchable rule 7) and never touches the time mode, which is a separate
    /// axis the tester set deliberately.</summary>
    public ScenarioItem? SelectedScenario
    {
        get => _selectedScenario;
        set
        {
            if (Set(ref _selectedScenario, value))
            {
                RaisePropertyChanged(nameof(HasSelectedScenario));
                RaisePropertyChanged(nameof(HasNoSelectedScenario));
                ScenarioExplains = value?.DisplayExplains ?? string.Empty;
                if (value is not null)
                {
                    _ = ApplyScenarioAsync(value);
                }
            }
        }
    }

    public bool HasSelectedScenario => _selectedScenario is not null;

    /// <summary>
    /// The other half of <see cref="HasSelectedScenario"/>, for a slot that must never be empty.
    /// </summary>
    /// <remarks>
    /// The folded catalogue's header shows the chosen scenario OR the number on offer, and those two
    /// TextBlocks share one cell. WPF has no negating converter here and a second one would be a second
    /// thing to keep in step, so the state says both halves out loud. A bool costs nothing at this
    /// class's coupling ceiling (gui/CodeMetricsConfig.txt) - a type would have.
    /// </remarks>
    public bool HasNoSelectedScenario => _selectedScenario is null;

    /// <summary>
    /// Choose the first scenario the filter left standing - the keyboard equivalent of clicking the top row,
    /// which <see cref="Controls.SearchBox"/> binds to Enter.
    /// </summary>
    /// <remarks>
    /// No-op when nothing matched, and deliberately so it never CLEARS a choice: Enter on an empty result
    /// must not undo the scenario already picked, which the filter is allowed to leave standing (see
    /// <see cref="ScenarioPicker"/>). Setting <see cref="SelectedScenario"/> is the same path a click takes,
    /// so the moment is computed once, here as there.
    /// </remarks>
    public void ChooseFirstScenario()
    {
        if (ScenarioPicker.Visible.FirstOrDefault() is { } first)
        {
            SelectedScenario = first;
        }
    }

    /// <summary>The chosen scenario's "what this date tests" line, straight from the catalogue (DATA
    /// locales, not interface keys - the author wrote it, we do not translate it).</summary>
    public string ScenarioExplains { get => _scenarioExplains; private set => Set(ref _scenarioExplains, value); }

    /// <summary>Translation key naming why a scenario could not be turned into a date, empty when fine. A
    /// scenario that will not compute says so instead of leaving the old date in place (rule 6).</summary>
    public string ScenarioErrorKey
    {
        get => _scenarioErrorKey;
        private set { if (Set(ref _scenarioErrorKey, value)) { RaisePropertyChanged(nameof(HasScenarioError)); } }
    }

    public bool HasScenarioError => _scenarioErrorKey.Length > 0;

    /// <summary>
    /// Turn a scenario into a concrete moment and fill the date with it.
    /// <para>
    /// The preset is unpacked into explicit steps and evaluated through the ordinary calc grammar, NOT
    /// through <c>calc --preset</c>: that path gates on <c>applies_to</c> and refuses a substitution-only
    /// preset, which is exactly the half of the catalogue this panel exists to offer. The engine stays the
    /// single source of the date either way (rule 16) - nothing here computes a calendar.
    /// </para>
    /// </summary>
    internal async Task ApplyScenarioAsync(ScenarioItem scenario)
    {
        ArgumentNullException.ThrowIfNull(scenario);
        ScenarioErrorKey = string.Empty;

        var resolved = await ScenarioMoment.ResolveAsync(_calcClient, scenario, SelectedZone.BiasMinutes);
        if (resolved.Iso is null)
        {
            ScenarioErrorKey = resolved.ErrorKey!;
            return;
        }

        _applyingScenario = true;
        try
        {
            Moment.LoadCanonical(resolved.Iso);
        }
        finally
        {
            _applyingScenario = false;
        }
    }

    /// <summary>The "relative to now" line under the moment field - the panel's <c>--at +30d</c>. A view
    /// model of its own so this class carries one type for it rather than four (see
    /// RelativeMomentViewModel and gui/CodeMetricsConfig.txt).</summary>
    public RelativeMomentViewModel Relative { get; }

    /// <summary>Drop the scenario selection without re-computing anything - the moment no longer came from
    /// it. Writes the field directly, because the property's setter is the "apply this scenario" path.</summary>
    private void ClearScenarioSelection()
    {
        if (_selectedScenario is null)
        {
            return;
        }

        _selectedScenario = null;
        ScenarioExplains = string.Empty;
        ScenarioErrorKey = string.Empty;
        RaisePropertyChanged(nameof(SelectedScenario));
        RaisePropertyChanged(nameof(HasSelectedScenario));
        RaisePropertyChanged(nameof(HasNoSelectedScenario));
    }

    /// <summary>
    /// Change the multiplier in flight (0 freezes, N resumes at N times). The core re-anchors from the
    /// current clock so the fake time is continuous across the change (ADR-5). No-op unless a session is
    /// running - the controls are only shown then, but the guard keeps a stray call safe.
    /// </summary>
    public void SendMultiplier(long multiplier)
    {
        var session = _session;
        if (session is null || !IsRunning)
        {
            return;
        }

        InFlightErrorKey = string.Empty;
        session.SendInFlight(new SetMultiplierCommand { Id = _nextCommandId++, Multiplier = multiplier });
    }

    /// <summary>
    /// Parse a custom speed ("500" or "x500") and apply it in flight. A malformed or negative value
    /// surfaces an in-flight error rather than doing nothing silently (rule 6). Parsing is culture-invariant
    /// (a Polish box and a US one read "500" the same). No-op unless a session is running.
    /// </summary>
    /// <summary>
    /// Largest speed the custom-speed box accepts. Mirrors the core's own bound (chrono_core's
    /// MULTIPLIER_MAX), so the box refuses locally with a readable message instead of sending a value
    /// the core will reject over the wire. The core still validates - this is a courtesy, not the gate,
    /// and the two must be changed together (R2-K2).
    /// </summary>
    internal const long MaxSpeed = 1_000_000;

    public void SetCustomSpeed(string? raw)
    {
        if (!IsRunning)
        {
            return;
        }

        InFlightErrorKey = string.Empty; // a fresh attempt clears any prior in-flight error
        var trimmed = raw?.Trim().TrimStart('x', 'X', '×');
        if (long.TryParse(trimmed, NumberStyles.Integer, CultureInfo.InvariantCulture, out var multiplier)
            && multiplier >= 0
            && multiplier <= MaxSpeed)
        {
            SendMultiplier(multiplier);
        }
        else
        {
            InFlightErrorKey = "speed.invalid";
        }
    }

    /// <summary>
    /// Jump the fake clock by a relative delta (e.g. "+1d", "-2h" - units s/m/h/d/w). The core adds it to
    /// the current fake time and re-anchors - a backward jump never rewinds the duration axis (rule 3). The
    /// core validates the delta and reports a bad one as an error. No-op unless a session is running.
    /// </summary>
    public void SendJump(string delta)
    {
        var session = _session;
        if (session is null || !IsRunning)
        {
            return;
        }

        InFlightErrorKey = string.Empty; // a fresh attempt clears any prior in-flight error
        session.SendInFlight(new JumpCommand
        {
            Id = _nextCommandId++,
            To = new MomentSpec { Kind = "relative", Delta = delta },
        });
    }

    /// <summary>
    /// Jump the fake clock to an ABSOLUTE moment (entered in the session zone, rule 2) while running - the
    /// in-flight counterpart of the start moment. The core re-anchors the wall and validates the moment,
    /// reporting a bad one (DST gap, non-leap Feb 29, out of range) as a per-command error that does NOT
    /// end the session (see <see cref="Apply"/>). No-op unless a session is running.
    /// </summary>
    public void SendJumpAbsolute(string momentLocal, int zoneBiasMinutes)
    {
        var session = _session;
        if (session is null || !IsRunning)
        {
            return;
        }

        InFlightErrorKey = string.Empty;
        session.SendInFlight(new JumpCommand
        {
            Id = _nextCommandId++,
            To = new MomentSpec { Kind = "absolute", Local = momentLocal, TzBiasMin = zoneBiasMinutes },
        });
    }

    /// <summary>Jump the wall to the moment currently in the At field, in the session zone (rule 2). No-op
    /// if the moment is malformed (the Jump button is disabled then) or no session is running.</summary>
    public void JumpToEnteredMoment()
    {
        if (Moment.IsValid)
        {
            SendJumpAbsolute(Moment.Canonical, SelectedZone.BiasMinutes);
        }
    }

    /// <summary>True when no session is running - the setup inputs bind their enabled state to this, so the
    /// user can still fix an invalid moment (which disables Start but not the fields).</summary>
    public bool IsIdle => _idle;

    /// <summary>Whether the time inputs (moment and mode) may be edited. When idle they set the START
    /// config - while a session runs they act live (moment -> jump, mode -> set_multiplier). Locked only
    /// during the brief connecting/ending transitions. Target, zone and scale-duration stay start-only
    /// (zone cannot re-render in flight, scale-duration has no in-flight command).</summary>
    public bool CanEditTime => _idle || IsRunning;

    /// <summary>True once the session has ended, so a finished result can be returned to a fresh setup form
    /// (New session). Only the terminal states qualify - a live or a still-closing session is not a result
    /// to leave yet. Tracks the same set as <see cref="ShowsSessionControls"/>, from the other side.</summary>
    public bool CanBeginNewSession => IsTerminal(_statusKind);

    /// <summary>
    /// Return a finished session to a fresh setup form: clear the result (verdict, coverage, diagnostics)
    /// and go back to idle, KEEPING the target, the moment and the history, so the tester can run again at
    /// once. It never starts a session (rule 7) - it lands on the filled setup form and waits for Start.
    /// A no-op unless the session has ended, so a stray call while one is live cannot wipe a live result.
    /// </summary>
    public void BeginNewSession()
    {
        if (!CanBeginNewSession)
        {
            return;
        }

        ResetSession();
        Idle = true;
        SetStatus("status.idle", SessionStatusKind.Idle);
    }

    /// <summary>Backs <see cref="CanStart"/> and <see cref="IsIdle"/>: true when no session is running.</summary>
    private bool Idle
    {
        get => _idle;
        set
        {
            if (Set(ref _idle, value))
            {
                RaisePropertyChanged(nameof(CanStart));
                RaiseStartRefusalChanged();
                RaisePropertyChanged(nameof(IsIdle));
                RaisePropertyChanged(nameof(CanEditTime));
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

    /// <summary>
    /// Translation key for the one word the result phase leads with: the verdict when the session produced
    /// one, and otherwise what stopped it from producing one.
    /// </summary>
    /// <remarks>
    /// 🔴 THE VERDICT IS NOT THE WHOLE ANSWER. A session that vanished right after injection may carry a
    /// "works" verdict from its first blink, and a start that failed carries none at all - the summary
    /// already leads with DID NOT TAKE EFFECT over any verdict for the first case, and this is the same rule
    /// on screen. An error before the first heartbeat is "did not start": the application never ran under
    /// the fake clock, whatever the core managed to say about it. An error after one keeps the verdict, because
    /// the substitution did work for as long as the session lasted and the status line says how it ended.
    ///
    /// Strings and an enum this class already couples to, so the coupling ceiling is not touched
    /// (gui/CodeMetricsConfig.txt).
    /// </remarks>
    public string ResultHeadlineKey => _statusKind switch
    {
        SessionStatusKind.DidNotTakeEffect => "result.headline_did_not_take_effect",
        SessionStatusKind.Error when !_hasTiming => "result.headline_did_not_start",
        _ => _verdictKnown ? _verdictLabelKey : "result.headline_no_verdict",
    };

    /// <summary>The kind behind <see cref="ResultHeadlineKey"/>, driving its glyph and colour: an outcome
    /// without a verdict word is drawn as a failure when the substitution never took effect and as
    /// undetermined when nothing was judged.</summary>
    public VerdictKind ResultKind => _statusKind switch
    {
        SessionStatusKind.DidNotTakeEffect => VerdictKind.Fails,
        SessionStatusKind.Error when !_hasTiming => VerdictKind.Fails,
        _ => _verdictKnown ? _verdictKind : VerdictKind.Undetermined,
    };

    /// <summary>The headline is the verdict word itself, so the verdict's own reason and meaning belong under it.
    /// Under any other headline they would explain a word that is not on the screen.</summary>
    private bool ResultIsVerdict
        => _verdictKnown && ResultHeadlineKey == _verdictLabelKey;

    /// <summary>
    /// The core's reason for the verdict, for EVERY verdict the result phase leads with.
    /// </summary>
    /// <remarks>
    /// 🔴 UNLIKE <see cref="VerdictHasReason"/>, which hides the reason under a clean "works" because a badge
    /// in a footer needs no caveat. The result phase exists to explain, and to somebody on their first run
    /// "Works" alone says nothing about what worked - the core's sentence ("every process that read the time
    /// saw the fake clock") is the explanation, and it arrives for works as for the rest.
    /// </remarks>
    public bool ResultHasReason => ResultIsVerdict && _verdictReasonKey.Length > 0;

    /// <summary>The plain-language meaning, under the headline it is about (see <see cref="ResultHasReason"/>).</summary>
    public bool ResultHasMeaning => ResultIsVerdict && _verdictHasMeaning;

    /// <summary>The status line tells how the session ended, unless the headline already did: a target that
    /// vanished has "did not take effect" as its headline and how long it lived under it, and the status
    /// sentence would say the same thing a third time.</summary>
    public bool ResultHasEnding => _statusKind != SessionStatusKind.DidNotTakeEffect;

    /// <summary>
    /// The audit block prints the verdict's reason beside its evidence only while the session runs.
    /// </summary>
    /// <remarks>
    /// While a session is live the verdict word is a chip in the footer and its reasoning belongs with the
    /// lists that produced it. Once the session is over the result phase leads with the word and prints the
    /// reason under it, and the same sentence inside the audit as well would put one line on the screen
    /// twice. The block is one component on both screens, so the choice is made here, where the phase is
    /// known, rather than by a switch on the drawing. The shipped panel keeps <see cref="VerdictHasReason"/>.
    /// </remarks>
    public bool AuditExplainsVerdict => _verdictHasReason && !IsTerminal(_statusKind);

    /// <summary>The meaning line of the audit block, on the same terms as <see cref="AuditExplainsVerdict"/>.</summary>
    public bool AuditExplainsMeaning => _verdictHasMeaning && !IsTerminal(_statusKind);

    /// <summary>
    /// The audit section is drawn open when the verdict is worse than a clean works.
    /// </summary>
    /// <remarks>
    /// 🔴 THE EVIDENCE IS THE ANSWER TO A BAD VERDICT. A reader who sees "Partial" or "Fails" has exactly
    /// one question - which clocks went to the real one - and the audit is where it is answered. Folded, they
    /// would have to know to open it - open, the answer is already on the screen. A clean works has nothing to
    /// worry the reader, so it stays folded. A vanish and a failed start carry no verdict at all
    /// (<see cref="VerdictKnown"/> is false), so they keep the section folded and let their own sentence stand.
    /// OneWay in the view, so the reader can still fold it and it stays folded.
    /// </remarks>
    public bool AuditStartsOpen => _coverageKnown && _verdictKnown && _verdictKind != VerdictKind.Works;

    /// <summary>The core's reason key for a target that vanished, shown with <see cref="LivedMs"/>. On screen only
    /// for a session that did not take effect - the summary composes the same two into one line.</summary>
    public string VanishReasonKey => _vanishReasonKey;

    public bool HasVanishReason
        => _statusKind == SessionStatusKind.DidNotTakeEffect && _vanishReasonKey.Length > 0;

    /// <summary>How long the vanished target lived, in milliseconds - what tells a single-instance hand-off
    /// (gone within a blink) from an application that ran and then quit.</summary>
    public long LivedMs => _livedMs;

    /// <summary>True once a heartbeat or the end timing arrived: the facts block of the result phase has
    /// something to say. A start that failed or was refused has no timing and shows no facts.</summary>
    public bool HasTiming => _hasTiming;

    /// <summary>The moment the session STARTED at, formatted like <see cref="MomentPreview"/>, from the start
    /// snapshot rather than the form - the form is unlocked once the session is over.</summary>
    public string StartedAtPreview => ClockView.FormatMoment(RequestedMoment, RequestedZone.Label);

    /// <summary>
    /// Where the fake clock stopped, formatted like <see cref="MomentPreview"/>.
    /// </summary>
    /// <remarks>
    /// 🔴 THE END TIMING FROM <c>ended</c>, NOT THE LAST HEARTBEAT. The heartbeat is up to a second old when
    /// the session ends, which at ×1440 is up to a day of fake time - the summary already prefers the
    /// authoritative end wall for the same reason, and a screen that named a different moment from the
    /// summary copied off it would be two answers to one question. The heartbeat is the fallback for a
    /// session that ended without one (a vanished target).
    /// </remarks>
    public string FakeEndPreview
        => ClockView.FormatMoment(_fakeEndWall.Length > 0 ? _fakeEndWall : Fake.Wall, Fake.Zone);

    /// <summary>True when a raw failure detail is being shown (see <see cref="LastError"/>).</summary>
    public bool HasLastError => _lastError.Length > 0;

    /// <summary>Size of the process family the session verdict covers (parent plus children).</summary>
    public int ProcessCount
    {
        get => _processCount;
        private set
        {
            if (Set(ref _processCount, value))
            {
                RaisePropertyChanged(nameof(IsFamily));
                RaisePropertyChanged(nameof(HasCoverageNote));
            }
        }
    }

    /// <summary>True when the session spanned more than one process (the verdict is the family aggregate).</summary>
    public bool IsFamily => _processCount > 1;

    /// <summary>True once a coverage report has arrived - the audit block stays hidden until then.</summary>
    public bool CoverageKnown
    {
        get => _coverageKnown;
        private set
        {
            if (Set(ref _coverageKnown, value))
            {
                RaisePropertyChanged(nameof(HasCoverageNote));
                RaisePropertyChanged(nameof(IsAuditPending));
                RaisePropertyChanged(nameof(AuditNeverArrived));
                RaisePropertyChanged(nameof(AuditNeverStarted));
                RaisePropertyChanged(nameof(AuditStartsOpen));
            }
        }
    }

    /// <summary>
    /// The audit has not arrived and still can, so the place for it says the report is on its way.
    /// </summary>
    /// <remarks>
    /// 🔴 A PLACE THAT IS EMPTY HAS TO SAY WHY. The session phase hides the audit until a report arrives,
    /// which left 300 px of nothing between the controls and the footer on the first render - a screen
    /// that looks unfinished rather than one that is waiting. This is the state behind the sentence that
    /// fills it. WPF has no negating visibility converter here and a second converter would be a second
    /// thing to keep in step, so the model says each case out loud, as it does for the scenario selection.
    /// A bool costs nothing at this class's coupling ceiling.
    ///
    /// It used to be "coverage not known" alone, which kept promising the report after the session was over.
    /// </remarks>
    public bool IsAuditPending => !_coverageKnown && !IsTerminal(_statusKind);

    /// <summary>
    /// The session is over and no audit ever arrived, so the place for it has to say that it will not.
    /// </summary>
    /// <remarks>
    /// 🔴 A PROMISE THE TOOL CANNOT KEEP. The session phase told an ended session with no report that the list
    /// would appear once the tool had checked - after the application had vanished, with nothing left to
    /// check.
    /// </remarks>
    public bool AuditNeverArrived => !_coverageKnown && IsTerminal(_statusKind) && !AuditNeverStarted;

    /// <summary>
    /// The application was never started under the fake clock, so there was nothing to check.
    /// </summary>
    /// <remarks>
    /// 🔴 "THE SESSION ENDED BEFORE THE TOOL COULD CHECK" IS FALSE FOR A START THAT FAILED. There was no
    /// session to end: the core could not attach, or refused the handshake, and the application never ran
    /// under the fake clock at all. The result phase put that sentence under a "did not start" headline on
    /// its first render. A failure before the first heartbeat is this case, and it gets its own sentence.
    /// </remarks>
    public bool AuditNeverStarted
        => !_coverageKnown && _statusKind == SessionStatusKind.Error && !_hasTiming;

    /// <summary>True when this session is driven over CDP (a Chromium/Electron target, ADR-9). The coverage
    /// unit is then a JS context, not an OS process, so the audit accumulates every context and the note
    /// reflects that. Set once at start from the plan.</summary>
    /// <remarks>The setter is internal rather than private so a test can drive the CDP coverage branch
    /// without a Chromium target and a live debug port. Production still sets it in exactly one place
    /// (from the session plan) - the widening buys a test for how contexts are told apart, which is a
    /// thing the panel gets wrong silently when it is wrong.</remarks>
    public bool IsCdp
    {
        get => _isCdp;
        internal set
        {
            if (Set(ref _isCdp, value))
            {
                RaisePropertyChanged(nameof(CoverageNoteKey));
                RaisePropertyChanged(nameof(HasCoverageNote));
            }
        }
    }

    /// <summary>Translation key for the coverage note: a CDP session shows every JS context, a native
    /// family session shows the parent's call counts with the whole family's warnings and uncovered
    /// channels (rule 4 - counts are never summed across processes).</summary>
    public string CoverageNoteKey => _isCdp ? "coverage.contexts_note" : "coverage.family_note";

    /// <summary>Whether to show the coverage note: for CDP once coverage arrived (it spans contexts), for
    /// native only when the session spanned a process family.</summary>
    public bool HasCoverageNote => _isCdp ? _coverageKnown : IsFamily;

    /// <summary>Covered channels, formatted "channel  xN", from the parent process (never summed, rule 4).</summary>
    public IReadOnlyList<CoveredChannel> Covered
    {
        get => _covered;
        private set { if (Set(ref _covered, value)) { RaisePropertyChanged(nameof(HasCovered)); } }
    }

    /// <summary>Channels hooked but deliberately left real (e.g. QPC-based waits, ADR-2), formatted "channel  xN".</summary>
    public IReadOnlyList<CoveredChannel> Observed
    {
        get => _observed;
        private set { if (Set(ref _observed, value)) { RaisePropertyChanged(nameof(HasObserved)); } }
    }

    /// <summary>Uncovered channel identifiers (raw API names, not translation keys) - the partial verdict's
    /// evidence, unioned over every process of the family (R2-W5).</summary>
    public IReadOnlyList<string> Uncovered
    {
        get => _uncovered;
        private set { if (Set(ref _uncovered, value)) { RaisePropertyChanged(nameof(HasUncovered)); } }
    }

    /// <summary>Channels the session meant to watch and could not hook, unioned over the family. Its own
    /// list rather than part of <see cref="Uncovered"/>, because it is not the partial verdict's evidence
    /// - it is the panel admitting that a watch it promised is not running.</summary>
    public IReadOnlyList<string> Unobserved
    {
        get => _unobserved;
        private set { if (Set(ref _unobserved, value)) { RaisePropertyChanged(nameof(HasUnobserved)); } }
    }

    /// <summary>Channels that came under the fake clock only once their module loaded, unioned over the
    /// family. Every one is also in <see cref="Covered"/> or <see cref="Observed"/> with its count - this
    /// names which of those counts are floors, which the late-hook warning needs and cannot say itself.</summary>
    public IReadOnlyList<string> InstalledLate
    {
        get => _installedLate;
        private set { if (Set(ref _installedLate, value)) { RaisePropertyChanged(nameof(HasInstalledLate)); } }
    }

    /// <summary>Warning translation keys the core raised (rendered in the current language), unioned over
    /// every process of the family and the session aggregate (R2-W5, R2-S9).</summary>
    public IReadOnlyList<string> Warnings
    {
        get => _warnings;
        private set { if (Set(ref _warnings, value)) { RaisePropertyChanged(nameof(HasWarnings)); } }
    }

    /// <summary>The target's own exit code, from <c>ended.target_exit_code</c> - present only for a native
    /// session whose app exited on its own (null for a Stop/end or a CDP session). Informational, never a
    /// verdict - shown so the reader can tell "the app closed itself (code N)" from "the session was stopped".</summary>
    public int? TargetExitCode
    {
        get => _targetExitCode;
        private set { if (Set(ref _targetExitCode, value)) { RaisePropertyChanged(nameof(HasTargetExit)); } }
    }

    public bool HasTargetExit => _targetExitCode.HasValue;

    /// <summary>Cleanup residue translation keys from <c>ended.residue_keys</c> - what a teardown could not
    /// remove (today only a CDP temp profile that stayed locked). Empty on a clean end - rendered as
    /// warnings so a session reports the mess it left rather than hiding it (rule 6).</summary>
    public IReadOnlyList<string> ResidueKeys
    {
        get => _residueKeys;
        private set { if (Set(ref _residueKeys, value)) { RaisePropertyChanged(nameof(HasResidue)); } }
    }

    public bool HasResidue => _residueKeys.Count > 0;

    /// <summary>The captured diagnostics block (core stderr and parse errors, with a header naming the
    /// status, target, and requested moment) for the Copy-diagnostics action. Empty on a clean success, so
    /// the button is hidden on the happy path (RELEASE-012).</summary>
    public string DiagnosticsText
    {
        get => _diagnosticsText;
        private set { if (Set(ref _diagnosticsText, value)) { RaisePropertyChanged(nameof(HasDiagnostics)); } }
    }

    public bool HasDiagnostics => _diagnosticsText.Length > 0;

    /// <summary>Path of the diagnostics log file written for this session, or empty when none was written
    /// (a clean success, or a read-only medium where the in-memory copy still stands). Shown so the user
    /// knows which file to attach to a report.</summary>
    public string DiagnosticsSavedPath
    {
        get => _diagnosticsSavedPath;
        private set { if (Set(ref _diagnosticsSavedPath, value)) { RaisePropertyChanged(nameof(HasDiagnosticsSaved)); } }
    }

    public bool HasDiagnosticsSaved => _diagnosticsSavedPath.Length > 0;

    public bool HasCovered => _covered.Count > 0;

    public bool HasObserved => _observed.Count > 0;

    public bool HasUncovered => _uncovered.Count > 0;

    public bool HasUnobserved => _unobserved.Count > 0;

    public bool HasInstalledLate => _installedLate.Count > 0;

    public bool HasWarnings => _warnings.Count > 0;

    /// <summary>
    /// Fold one event into the view state. Pure and synchronous (no I/O, no threading) so the mapping is
    /// unit-testable - the live loop marshals each call onto the UI thread. A late <c>state</c> after a
    /// terminal outcome is ignored, so a finished session is never resurrected as "running".
    /// </summary>
    public void Apply(ChronoEvent evt)
    {
        switch (evt)
        {
            case StateEvent s when CanReturnToRunning(StatusKind):
                Fake.Wall = s.Fake.Wall;
                Fake.Zone = ZoneLabel.FromBiasMinutes(s.Fake.ZoneBiasMin);
                Real.Wall = s.Real.Wall;
                Real.Zone = ZoneLabel.FromBiasMinutes(s.Real.ZoneBiasMin);
                // 🔴 The same multiplication sign the rate BUTTONS wear. This read "x60" beside a control
                // labelled "×60" - one rate written two ways on one screen, and the session phase puts
                // them within a hundred pixels of each other. The number is the core's, the notation is
                // ours, and there is only one of it.
                MultiplierText = $"×{s.Multiplier}";
                SyncModeToMultiplier(s.Multiplier);
                _elapsedRealMs = s.ElapsedRealMs;
                _elapsedFakeMs = s.ElapsedFakeMs;
                _hasTiming = true;
                PublishElapsed();
                SetStatus("status.running", SessionStatusKind.Running);
                break;
            case VanishedEvent vd:
                _vanishReasonKey = vd.ReasonKey;
                _livedMs = vd.LivedMs;
                RaisePropertyChanged(nameof(VanishReasonKey));
                RaisePropertyChanged(nameof(LivedMs));
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
                    PublishElapsed();
                }

                // Surface the target's own exit code and any cleanup residue the core reported, rather than
                // dropping them (rule 6): the exit code tells a self-closed app from a stopped session, and
                // residue names what a teardown left behind (a CDP temp profile that stayed locked).
                TargetExitCode = e.TargetExitCode;
                ResidueKeys = e.ResidueKeys;
                SetStatus("status.ended", SessionStatusKind.Ended);
                break;
            case ErrorEvent err when StatusKind == SessionStatusKind.Running && IsInFlightError(err):
                // A per-command error (e.g. an invalid in-flight jump moment): the core rejects the one
                // command WE sent - its id echoes our command's - and keeps running, so surface it and STAY
                // live, never end the session (rule 6).
                InFlightErrorKey = InFlightKey(err.Key);
                break;
            case ErrorEvent err when !IsTerminal(StatusKind):
                // A start-time or fatal error (bad start moment, hook DLL missing, launch/inject/attach
                // failed). The session never became live: the status was set to "running" optimistically
                // right after `start`, but this error answers the START command (id 1) or is unsolicited (no
                // id) - NOT an in-flight rejection (RELEASE-001). Land on a failure status carrying the
                // core's specific translated reason as the headline, so a failed start never reads as
                // "Session ended". The core then emits `ended` (clean is true even here), but the terminal
                // status ignores it (the guard above), so the honest failure stands.
                SetStatus(err.Key, SessionStatusKind.Error);
                break;
            case VerdictEvent v:
                // The per-process verdict, at start. It gates refuse_start and is the first indicator shown.
                SetVerdict(VerdictKinds.Parse(v.Verdict), v.ReasonKey);
                if (v.RefuseStart)
                {
                    // The core stopped the target rather than hand back a session whose evidence would be
                    // about the real clock. Say so as a terminal state - the verdict and its reason are
                    // already shown beside it, and IsReliable is false, so nothing here reads as a
                    // successful run (untouchable rule 4).
                    SetStatus("status.refused", SessionStatusKind.Refused);
                }

                break;
            case SessionVerdictEvent sv:
                // The family aggregate, at end - it overrides the per-process verdict on the indicator.
                ProcessCount = sv.ProcessCount;
                // Session-level warnings join the per-process ones (R2-S9). They are about the family,
                // not about any one process, so the panel shows them in the same list - a warning the
                // reader has to attribute to an event type is a warning they will not read.
                Warnings = [.. _warnings, .. sv.WarningKeys.Where(w => !_warnings.Contains(w))];
                SetVerdict(VerdictKinds.Parse(sv.Verdict), sv.ReasonKey);
                break;
            case CoverageEvent c when _isCdp:
                // CDP emits one coverage per JS context - accumulate them all (each context's counts stay
                // its own, never summed across contexts, rule 4) - union the warnings and uncovered lists.
                // Two pages both produce "page Date.now", so two contexts reading one channel become two
                // rows that tell apart only by their read count (rule 4 keeps each context's count its own,
                // never summed). The raw context id used to prefix each row - dropped in P6 as a number that
                // named nothing to the reader.
                Covered = [.. _covered, .. c.Covered];
                Observed = [.. _observed, .. c.Observed];
                Uncovered = [.. _uncovered, .. c.Uncovered.Where(u => !_uncovered.Contains(u))];
                Unobserved = [.. _unobserved, .. c.Unobserved.Where(u => !_unobserved.Contains(u))];
                InstalledLate = [.. _installedLate, .. c.InstalledLate.Where(u => !_installedLate.Contains(u))];
                Warnings = [.. _warnings, .. c.WarningKeys.Where(w => !_warnings.Contains(w))];
                CoverageKnown = true;
                break;
            case CoverageEvent c:
                // Native. Call counts stay the PARENT's - the process the first event names - because
                // summing them across processes would fabricate a per-process picture (untouchable rule
                // 4), and a per-process family breakdown is a later slice. But a LATER event for that
                // same pid replaces the earlier one: the core reports a process when it is discovered
                // and again when the session ends, and the first snapshot is taken inside the guard
                // window, so its counts are the session's first blink (R2-X8) - measured, a probe that
                // read the clock 25 times showed "×2" in this panel.
                _parentPid ??= c.Pid;
                if (c.Pid == _parentPid)
                {
                    Covered = c.Covered.ToList();
                    Observed = c.Observed.ToList();
                }

                // Warnings and uncovered channels are not counts - they are the REASON behind the family
                // verdict, and the core raises them per process (R2-W5). A child that opened a network
                // connection, ran an unscaled wait, or failed to hook a channel says so on its own event,
                // so dropping every event after the first left the panel with the right verdict and no
                // reason for it (rule 6). Union them across the family, exactly like the CLI driver
                // (driver_run) and the CDP branch above. A channel can therefore be covered in the parent
                // and uncovered in a child at once - that is the honest reading, and it is precisely why
                // the family verdict is partial (Verdict::combine).
                Uncovered = [.. _uncovered, .. c.Uncovered.Where(u => !_uncovered.Contains(u))];
                Unobserved = [.. _unobserved, .. c.Unobserved.Where(u => !_unobserved.Contains(u))];
                InstalledLate = [.. _installedLate, .. c.InstalledLate.Where(u => !_installedLate.Contains(u))];
                Warnings = [.. _warnings, .. c.WarningKeys.Where(w => !_warnings.Contains(w))];
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
        _launched = false;
        _stopRequested = false;
        LastError = string.Empty;
        ResetSession();
        SetStatus("status.connecting", SessionStatusKind.Connecting);

        CoreSession? session = null;
        try
        {
            // Snapshot the start setup NOW, so history, the summary and the diagnostics block always report
            // what was REQUESTED (rule 4) - neither a later in-flight change nor an edit made after the
            // session ended, while the form is unlocked and the summary is still copyable.
            _startMomentText = Moment.Canonical;
            _startMode = SelectedMode;
            _startTargetPath = TargetPath;
            _startZone = SelectedZone;
            _startTargetArgs = _targetArgs;
            _startWorkingFolder = _workingFolder;
            _startScaleDuration = _scaleDuration;
            _startScaleQpc = _scaleQpc;
            _startForce = _forceStart;
            _startCaptured = true;
            RaisePropertyChanged(nameof(StartedAtPreview));

            // Build the plan by reading the target's PE header. Classify a TARGET problem here (RELEASE-007)
            // so it is not reported as a broken core install: a non-PE file yields InvalidOperationException
            // ("cannot determine the bitness"), while a missing or unreadable file propagates from PeReader
            // as an IO/access error (it does not swallow those - only a malformed PE becomes Unknown).
            SessionPlan plan;
            try
            {
                plan = SessionPlan.Build(
                    TargetPath!,
                    BuildTime(),
                    _forceStart,
                    TargetArguments.Split(_targetArgs),
                    _workingFolder);
            }
            catch (InvalidOperationException ex)
            {
                LastError = ex.Message;
                SetStatus("status.target_unsupported", SessionStatusKind.Error);
                return;
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                LastError = ex.Message;
                SetStatus("status.target_unreadable", SessionStatusKind.Error);
                return;
            }

            IsCdp = plan.IsCdp;

            // The handshake and its two refusals (no ready, or a gate that says no) come back as ONE
            // translation key, because that is all the difference amounts to here - both refuse before the
            // target is ever launched (docs/08 section 3, zasady/13 section 11). The session comes back
            // either way so the finally below still disposes the core and still captures its stderr.
            var open = await CoreSession.OpenAsync(plan, ReadyTimeout);
            session = open.Session;
            _session = session;
            if (open.RefusalKey is not null)
            {
                SetStatus(open.RefusalKey, SessionStatusKind.Error);
                return;
            }

            session.Send(plan.Start);
            _launched = true; // the target is now running - this session will be recorded in history on exit
            SetStatus("status.running", SessionStatusKind.Running);

            var watchdogFired = await CoreSession.PumpAsync(session.Events, Apply, IdleTimeout);

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
            // The core itself could not be started: its executable is missing or would not spawn (a broken
            // or incomplete install). Target-file problems were already classified above and returned, so
            // reaching here means the core, not the target (RELEASE-007).
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
                await RecordSessionAsync();
            }

            if (session is not null)
            {
                // Dispose blocks briefly (it waits for the core to exit) and keeps that wait off the UI
                // thread itself - see CoreSession.DisposeAsync.
                await session.DisposeAsync();

                // Now that dispose has drained the core's stderr, capture diagnostics for support if the
                // session was anything but a clean success (RELEASE-012). A clean works session captures
                // nothing. (On a Stop the core was already disposed off-thread, so this is best-effort -
                // but a stopped healthy session has no error stderr to lose.)
                CaptureDiagnostics(session.Diagnostics, session.DiagnosticsDropped);

                if (ReferenceEquals(_session, session))
                {
                    _session = null;
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
        var session = _session;
        if (session is null || !IsRunning)
        {
            return;
        }

        _stopRequested = true;
        // Drop out of Running immediately. Shutdown is asynchronous and takes up to the core's grace
        // period, and until now IsRunning stayed true throughout - so a click on Jump or a speed button
        // in that window reached a stream that was already closing and threw where nothing caught it.
        SetStatus("status.stopping", SessionStatusKind.Stopping);
        _ = session.DisposeAsync().AsTask();
    }

    public async ValueTask DisposeAsync()
    {
        var session = _session;
        _session = null;
        if (session is not null)
        {
            await session.DisposeAsync();
        }
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
        _parentPid = null;
        IsCdp = false;
        Covered = [];
        Observed = [];
        Uncovered = [];
        // 🔴 Unobserved was missing from this list, so a second session in the same window kept listing the
        // first session's channels under "Could not be watched" - both lists are unions, and a union with the
        // previous session is the audit describing a session that is not this one (untouchable rule 4).
        Unobserved = [];
        InstalledLate = [];
        Warnings = [];

        _hasTiming = false;
        _elapsedRealMs = 0;
        _elapsedFakeMs = 0;
        // 🔴 Cleared, not zeroed on screen: "0:00:00" beside a clock is a measurement, and a session that
        // has not reported yet has not measured anything. The tile drops the line instead.
        Fake.Elapsed = string.Empty;
        Real.Elapsed = string.Empty;
        _fakeEndWall = string.Empty;
        _vanishReasonKey = string.Empty;
        _livedMs = 0;
        TargetExitCode = null;
        ResidueKeys = [];
        DiagnosticsText = string.Empty;
        DiagnosticsSavedPath = string.Empty;
        CopyFeedbackKey = string.Empty;
        InFlightErrorKey = string.Empty;
        // A note about what the last LOAD could not fill belongs to the form the user is about to run, not
        // to the run itself - once they start, they have accepted the form as it stands.
        HistoryNoteKey = string.Empty;
    }

    /// <summary>
    /// A covered or observed channel as one line for the copy-summary, its read count folded in as prose so
    /// it does not read as a speed. The view renders the same row through CoverageRowConverter - two
    /// surfaces, one wording (coverage.reads). InvariantCulture keeps the count stable across machines, and
    /// CDP no longer prefixes a raw context id (P6): two contexts reading one channel differ by their count.
    /// </summary>
    private static string FormatReadRow(CoveredChannel channel, Func<string, string> translate)
    {
        var format = translate(channel.Calls == 1 ? "coverage.reads_one" : "coverage.reads");
        return string.Format(CultureInfo.InvariantCulture, format, channel.Channel, channel.Calls);
    }

    /// <summary>Build the wire time from the inputs. The moment is the local time in the session zone
    /// (rule 2, chrono-mock 9.5) - the core turns it into UTC and validates it (docs/08 section 5).</summary>
    internal TimeSpec BuildTime()
    {
        var canonical = Moment.Canonical;
        return new TimeSpec
        {
            Moment = new MomentSpec { Kind = "absolute", Local = canonical, TzBiasMin = SelectedZone.BiasMinutes },
            Mode = SelectedMode.Mode,
            Multiplier = SelectedMode.Multiplier,
            ScaleDuration = _scaleDuration,
            ScaleQpc = _scaleQpc,
        };
    }

    /// <summary>The target the session STARTED on - the snapshot, falling back to the live path when no
    /// session has started (a fresh window, a unit test). Every report reads this rather than
    /// <see cref="TargetPath"/>, so none of them can name a target chosen after the run.</summary>
    private string RequestedTargetPath => _startTargetPath ?? _targetPath ?? string.Empty;

    /// <summary>The zone the session STARTED in, with the same fallback as
    /// <see cref="RequestedTargetPath"/>.</summary>
    private ZoneOption RequestedZone => _startZone ?? SelectedZone;

    /// <summary>The moment the session STARTED at, with the same fallback. Canonical either way - the live
    /// value comes from <see cref="MomentField"/>, which only ever holds a canonical string or nothing.</summary>
    private string RequestedMoment => _startMomentText.Length > 0 ? _startMomentText : Moment.Canonical;

    /// <summary>The mode the session STARTED in, with the same fallback.</summary>
    private ModeOption RequestedMode => _startMode ?? SelectedMode;

    /// <summary>
    /// Compose the paste-into-ticket session summary (chrono-mock 7.2, 8.8) in the interface language. It
    /// mirrors the CLI evidence export (crates/cli render_evidence): a session that is anything other than a
    /// clean "works" ALWAYS leads with an unreliable-evidence banner - evidence that hides doubt is worse
    /// than none. Pure over the view state, so it is unit tested with a fake translator - the caller supplies
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
        // The target the session RAN ON, not the one now in the form - see the snapshot fields.
        sb.Append("  ").Append(translate("report.target")).Append(": ")
          .Append(Path.GetFileName(RequestedTargetPath)).Append('\n');

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
            // Prefer the authoritative end wall from `ended` - fall back to the last heartbeat's fake clock.
            var fakeWall = _fakeEndWall.Length > 0 ? _fakeEndWall : Fake.Wall;
            sb.Append("  ").Append(translate("report.session")).Append(": ")
              .Append(Fmt(translate("report.session_reached"), fakeWall)).Append('\n');
            sb.Append("    ")
              .Append(Fmt(translate("report.elapsed"), Seconds(_elapsedRealMs), Seconds(_elapsedFakeMs)))
              .Append('\n');
        }

        // The target's own exit code, if the app exited on its own (informational, not a verdict).
        if (_targetExitCode is int code)
        {
            sb.Append("  ").Append(translate("report.target_exit")).Append(' ')
              .Append(code.ToString(CultureInfo.InvariantCulture)).Append('\n');
        }

        // Channel names are raw API identifiers (not translated) - warnings are keys the core raised.
        AppendList(sb, translate, "coverage.covered", _covered.Select(ch => FormatReadRow(ch, translate)).ToList(), translateItems: false);
        AppendList(sb, translate, "coverage.observed", _observed.Select(ch => FormatReadRow(ch, translate)).ToList(), translateItems: false);
        AppendList(sb, translate, "coverage.uncovered", _uncovered, translateItems: false);
        AppendList(sb, translate, "coverage.warnings", _warnings, translateItems: true);
        // Cleanup residue the core could not remove (ended.residue_keys) - reported, never hidden (rule 6).
        AppendList(sb, translate, "report.cleanup", _residueKeys, translateItems: true);

        // The "requested" line is the START request (snapshot) throughout - moment, zone and mode alike.
        sb.Append("  ")
          .Append(Fmt(
              translate("report.requested"),
              RequestedMoment,
              RequestedZone.Label,
              translate(RequestedMode.LabelKey)))
          .Append('\n');

        return sb.ToString();
    }

    /// <summary>
    /// Capture the core's diagnostics for support when a session ended in anything but a clean success
    /// (RELEASE-012): compose the block, expose it for the Copy-diagnostics button, and write it to a log
    /// file beside the exe. A clean works session leaves nothing, so the happy path stays quiet. Called from
    /// the StartAsync finally AFTER the client is disposed, so the core's stderr has been fully drained.
    /// Internal so a unit test can drive it with a fake line list and a fake log (no core process).
    /// </summary>
    internal void CaptureDiagnostics(IEnumerable<string> lines, int dropped = 0)
    {
        if (IsReliable)
        {
            return; // a clean works session needs no diagnostics - keep the button and the log out of it
        }

        var block = BuildDiagnosticsBlock(lines, dropped);
        DiagnosticsText = block;
        // Best-effort file: a read-only medium returns null, and the in-memory copy behind the button stands.
        var path = _diagnosticsLog.Save(block);
        if (path is not null)
        {
            DiagnosticsSavedPath = path;
        }
    }

    /// <summary>Compose the diagnostics block: a header naming the status, target, and requested moment, then
    /// the core's stderr and parse-error lines verbatim. English and stable, like the core's own stderr and
    /// the CLI report - it is a technical artifact for a bug report, not interface text (rule 15 governs the
    /// UI - this is data). Pure over the view state, so it is unit tested with a fake line list.</summary>
    internal string BuildDiagnosticsBlock(IEnumerable<string> lines, int dropped = 0)
    {
        var mode = RequestedMode;
        var modeToken = mode.Mode switch { "frozen" => "frozen", "flow" => "flow", _ => $"x{mode.Multiplier ?? 1}" };

        var sb = new StringBuilder();
        sb.Append("Chrono Mock diagnostics\n");
        sb.Append("  when:      ")
          .Append(DateTime.UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture)).Append('\n');
        sb.Append("  status:    ").Append(_statusKind).Append(" (").Append(_statusKey).Append(")\n");
        var target = RequestedTargetPath;
        sb.Append("  target:    ").Append(target.Length > 0 ? target : "(none)").Append('\n');
        sb.Append("  requested: ").Append(RequestedMoment)
          .Append(" (zone ").Append(RequestedZone.Label).Append(", mode ").Append(modeToken).Append(")\n");

        sb.Append("  core output:\n");
        var any = false;
        foreach (var line in lines)
        {
            sb.Append("    ").Append(line).Append('\n');
            any = true;
        }

        if (!any)
        {
            sb.Append("    (no diagnostic output)\n");
        }

        // A block that is a TAIL has to say so, and say how much is missing. The line count is capped
        // so a chatty target cannot grow it without bound, and a reader who takes the tail for the
        // whole of it draws conclusions from an absence that was never there (rule 6).
        if (dropped > 0)
        {
            sb.Append("    [")
              .Append(dropped.ToString(CultureInfo.InvariantCulture))
              .Append(" earlier line(s) dropped - this is the tail of the core's output]\n");
        }

        return sb.ToString();
    }

    /// <summary>A clean "works" session is the only reliable one - anything else must carry the unreliable
    /// banner in an export (chrono-mock 8.8), mirroring the CLI's session_is_reliable.</summary>
    private bool IsReliable => _verdictKind == VerdictKind.Works
                              && _statusKind != SessionStatusKind.DidNotTakeEffect;

    private static string Seconds(long ms) => (ms / 1000.0).ToString("0.0", CultureInfo.InvariantCulture);

    // Format a possibly-missing template safely: a resolver that returns the raw key (no placeholders)
    // leaves it unchanged, because string.Format ignores extra arguments when there are no holes to fill.
    // It does NOT ignore a hole with no argument ({5} of three) or an unbalanced brace, and translation
    // files are loose files anyone may edit - so a bad one degrades to the raw template instead of taking
    // down the summary that was being built.
    private static string Fmt(string template, params object[] args)
    {
        try
        {
            return string.Format(CultureInfo.InvariantCulture, template, args);
        }
        catch (FormatException)
        {
            return template;
        }
    }

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
    /// section 6). Pure over the view state, so it is unit tested - the GUI's own clock is real (only the
    /// target is faked), so DateTime.UtcNow is the true end time.</summary>
    internal SessionRecord BuildRecord()
    {
        // Record the START setup (snapshot), never an in-flight change and never a later edit (rule 4).
        // This one is written from the Start finally, BEFORE the form unlocks, so reading live would have
        // been correct here today - it reads the snapshot anyway, so the record does not depend on WHEN it
        // happens to be built. The fallback (no session started, e.g. a unit test) is the live value.
        var mode = RequestedMode;

        return new SessionRecord
        {
            TargetPath = RequestedTargetPath,
            MomentLocal = RequestedMoment,
            TzBiasMin = RequestedZone.BiasMinutes,
            Mode = mode.Mode,
            Multiplier = mode.Multiplier,
            // Everything else that decides what the session DID, so repeating it repeats the session
            // rather than a partial copy of it. Same snapshot-with-live-fallback rule as the four above.
            TargetArgs = _startCaptured ? _startTargetArgs : _targetArgs,
            WorkingFolder = _startCaptured ? _startWorkingFolder : _workingFolder,
            ScaleDuration = _startCaptured ? _startScaleDuration : _scaleDuration,
            ScaleQpc = _startCaptured ? _startScaleQpc : _scaleQpc,
            Force = _startCaptured ? _startForce : _forceStart,
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
    /// surfaced, never swallowed (rule 6, docs/04 section 7).
    ///
    /// The split is deliberate. <see cref="History"/> is bound to the window, so it is mutated HERE, on the
    /// caller's thread - the session loop deliberately does not use ConfigureAwait(false), which is what
    /// keeps this on the UI thread. Only the WRITE goes to the pool, because
    /// <c>FileSessionHistoryStore.Append</c> ends in a move-retry loop that sleeps 20, 40, 60, 80 and 100 ms
    /// while the destination is locked - two portable instances finishing at once park the dispatcher for
    /// up to ~300 ms exactly as the panel says "session ended". Measured without contention the whole
    /// append cycle is about 5 ms at the 50-record cap, so this is about the contended case, not the
    /// ordinary one.
    ///
    /// Moving the WHOLE method off the UI thread, which is the obvious-looking fix, is wrong: the panel's
    /// collection would then be changed from a pool thread and WPF refuses that outright.</summary>
    internal async Task RecordSessionAsync()
    {
        var record = BuildRecord();
        History.Insert(0, record);
        while (History.Count > SessionHistoryLimits.Max)
        {
            History.RemoveAt(History.Count - 1); // keep the panel in step with the store's cap
        }

        // The exception is carried back as a value rather than rethrown: the message belongs on the panel,
        // and awaiting a faulted Task.Run would wrap it in an AggregateException-shaped rethrow whose type
        // filter is easy to get subtly wrong.
        var error = await Task.Run(() =>
        {
            try
            {
                _store.Append(record);
                return null;
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                return ex.Message;
            }
        }).ConfigureAwait(true);

        HistoryError = error ?? string.Empty;
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
        Moment.LoadCanonical(record.MomentLocal);
        TargetArgs = record.TargetArgs;
        WorkingFolder = record.WorkingFolder;
        ScaleDuration = record.ScaleDuration;
        ScaleQpc = record.ScaleQpc;
        ForceStart = record.Force;

        // A zone or a mode the catalogues no longer offer cannot be filled in, and the old code left the
        // CURRENT one standing without a word - so the form claimed to be the recorded session while one of
        // its two decisive fields belonged to whatever was there before. Reachable when a record predates a
        // change to either closed list. Say it instead (rule 6): the fields that DID load stay loaded, and
        // the note names the one that did not, so the reader knows which one to set by hand.
        var zone = TimeInputs.Zones.FirstOrDefault(z => z.BiasMinutes == record.TzBiasMin);
        var mode = TimeInputs.Modes.FirstOrDefault(
            m => m.Mode == record.Mode && m.Multiplier == record.Multiplier);
        SelectedZone = zone ?? SelectedZone;
        SelectedMode = mode ?? SelectedMode;
        // Written as nested ifs rather than a switch on `(zone, mode)`, and that is not style: a tuple
        // pattern introduces ValueTuple as a coupled type, and this class sits exactly on its CA1506
        // ceiling of 82 (gui/CodeMetricsConfig.txt), so the tidier form reddens the metrics gate.
        if (zone is null)
        {
            HistoryNoteKey = mode is null ? "history.load_zone_and_mode_missing" : "history.load_zone_missing";
        }
        else
        {
            HistoryNoteKey = mode is null ? "history.load_mode_missing" : string.Empty;
        }
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
        RaiseResultChanged();
    }

    private static bool IsTerminal(SessionStatusKind kind) => kind
        is SessionStatusKind.Ended
        or SessionStatusKind.Refused
        or SessionStatusKind.DidNotTakeEffect
        or SessionStatusKind.CoreStopped
        or SessionStatusKind.Stopped
        or SessionStatusKind.CoreUnresponsive
        or SessionStatusKind.Error;

    /// <summary>
    /// True when a heartbeat may still put the session back into "running". Terminal statuses may not,
    /// and neither may <see cref="SessionStatusKind.Stopping"/>: the user has pressed Stop, shutdown is
    /// under way, and the in-flight controls are deliberately gone - "running" with dead buttons reads
    /// as a hang. Heartbeats keep arriving for up to the core's grace period, so without this every one
    /// of them undid the Stop (R2-W4).
    /// <para>
    /// Deliberately NOT folded into <see cref="IsTerminal"/>. That predicate also gates the
    /// <c>ended</c> event, which carries the core's authoritative end timing, the target's exit code
    /// and any cleanup residue - data we still want after a Stop.
    /// </para>
    /// </summary>
    internal static bool CanReturnToRunning(SessionStatusKind kind) =>
        !IsTerminal(kind) && kind != SessionStatusKind.Stopping;

    /// <summary>True when an error event answers one of OUR in-flight commands (jump/set_multiplier),
    /// whose ids run from <see cref="FirstInFlightCommandId"/> up. A start/fatal error instead carries the
    /// start command's id (1) or none, so it is never treated as an in-flight rejection (RELEASE-001).</summary>
    private static bool IsInFlightError(ErrorEvent err) => err.Id is >= FirstInFlightCommandId;

    /// <summary>
    /// Put the two durations on their own clocks.
    /// </summary>
    /// <remarks>
    /// One place rather than three: the timing arrives from a heartbeat and again from the core's
    /// authoritative end report, and a third site resets it. Formatting it at each of them would be three
    /// chances for the fake clock's duration and the real one's to end up written differently.
    /// </remarks>
    private void PublishElapsed()
    {
        Fake.Elapsed = ClockView.FormatDuration(_elapsedFakeMs);
        Real.Elapsed = ClockView.FormatDuration(_elapsedRealMs);
        // The timing facts of the result phase move with the elapsed values, and the headline with the first
        // heartbeat: an error before it is "did not start", after it the verdict stands.
        RaisePropertyChanged(nameof(HasTiming));
        RaisePropertyChanged(nameof(FakeEndPreview));
        RaisePropertyChanged(nameof(AuditNeverArrived));
        RaisePropertyChanged(nameof(AuditNeverStarted));
        RaiseResultChanged();
    }

    /// <summary>
    /// The reason a rejected IN-FLIGHT command gets, where the core's key has a start-time tail on it.
    /// </summary>
    /// <remarks>
    /// 🔴 THE SCREEN CONTRADICTED ITSELF. The core sends one key for a bad moment whether it arrives with
    /// the start command or with a jump, and the same for a rate out of range - and both of its texts end
    /// "the session did not start". In flight that is false twice over: the core's own comment on the rate
    /// path says "the session keeps running at the rate it had", and the panel printed the denial directly
    /// under a status line reading "Running". Measured on the session-error render.
    ///
    /// So the key is mapped, not the text rewritten: at START those two sentences are correct and they are
    /// the whole headline there. The CLI never had this problem - its own wording for the same keys names
    /// the reason and stops ("the requested speed is outside the range this core accepts"), which is the
    /// shape these two now follow in flight.
    ///
    /// Two entries rather than a table over every key: these are the only two of the seven texts that
    /// mention starting AND can answer an in-flight command. The guard in SessionViewModelTests holds that
    /// pairing from both ends.
    /// </remarks>
    private static string InFlightKey(string coreKey) => coreKey switch
    {
        "moment.invalid" => "moment.invalid_in_flight",
        "time.bad_multiplier" => "time.bad_multiplier_in_flight",
        _ => coreKey,
    };
}

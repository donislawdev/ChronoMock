using System.Collections.ObjectModel;
using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
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
public sealed class SessionViewModel : ObservableObject
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
    // What the session has said about itself - status, verdict, audit, processes, timing (R5-D62). The
    // properties below project it onto the panel, SessionReport composes the reports from it.
    private readonly SessionResult _result = new();
    private string _multiplierText = string.Empty;
    private string _lastError = string.Empty;
    private bool _idle = true;
    private string? _targetPath;
    private RecentTarget? _selectedTarget;
    private readonly MomentRequests _moments;
    private bool _momentPending; // Start gave up waiting for the newest date - refused until it lands
    private readonly ScenarioPicker _scenarios;
    private ScenarioItem? _selectedScenario;
    private string _scenarioExplains = string.Empty;
    private string _scenarioErrorKey = string.Empty;
    private bool _momentIsDefault = true;
    /// <summary>The editable moment (a date and optional time in the session zone, rule 2). The shared
    /// MomentInput control binds to it, and MomentParse composes it culture-invariantly (locale-safe).</summary>
    public MomentField Moment { get; } = new();
    /// <summary>
    /// The moment to jump the running clock to - a SEPARATE field from the start <see cref="Moment"/>.
    /// </summary>
    /// <remarks>
    /// 🔴 SEPARATE ON PURPOSE, and the re-analysis that split it named three faults in sharing the start
    /// field. An in-flight jump would rewrite where the session started, and <c>ResetSession</c> keeps the
    /// form filled, so "New session" would carry the jump target into the next run's Starts-at. The start
    /// field pre-fills with the start moment while the clock has moved on, so a jump without an edit would
    /// send the clock backwards to the start. And the start field's setter drives the setup form
    /// (CanStart, the default-moment note, the scenario selection), none of which a jump has any business
    /// touching. This field starts empty and drives nothing but its own Jump button.
    /// </remarks>
    public MomentField JumpMoment { get; } = new();
    private ZoneOption _selectedZone;
    private ModeOption _selectedMode;
    private bool _scaleDuration;
    private bool _scaleQpc;
    private bool _forceStart;
    private bool _reachEmbedded = true;
    // Plain values, for the reason the start snapshot below gives: this class is on its coupling ceiling.
    // Whether the WINDOW runs as administrator (told once, by whoever builds the view model) and whether
    // the tester ticked the option that writes the machine registry for an application that does too.
    private readonly bool _canReachElevated;
    private bool _reachElevatedEmbedded;
    private bool _keepBackgroundTimers;
    // The key of the one warning that must not sit below the fold: a registry value the session could not
    // remove (docs/09 section 12.19). A literal, because the wire carries it as a string.
    private const string LeftBehindKey = "embedded.policy_value_left";
    private string _targetArgs = string.Empty;
    private string _workingFolder = string.Empty;
    private readonly ISessionHistoryStore _store;
    private readonly IDiagnosticsLog _diagnosticsLog;
    // How a session reaches its core: null is the core process every build ships (CoreClient), a test passes
    // a connection that answers in the order it chooses, so the whole start-to-record path runs without a
    // core process (R5-D63).
    private readonly Func<string, ICoreConnection>? _connect;
    private bool _launched;
    private bool _stopRequested;
    private string _historyError = string.Empty;
    private string _historySetAside = string.Empty;
    private string _historyNoteKey = string.Empty;
    private string _targetNoteKey = string.Empty;
    private SessionRecord? _selectedRecord;
    // The setup the session was started with, taken at Start, so history and the reports record what was
    // REQUESTED even after the form is changed (rule 4 - the record is the start). See SessionSetup for why
    // the target and the zone are in it. Null means "no session has started yet", and every reader falls
    // back to the live form for it (Requested), so a view model that never ran (a unit test, a fresh window)
    // reads exactly as it did before. Never cleared: a new session replaces it at its own Start.
    private SessionSetup? _started;
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
        ICalcEngine? calcClient = null,
        ScenarioPicker? scenarios = null,
        bool canReachElevated = false,
        TimeSpan? momentWait = null,
        Func<string, ICoreConnection>? connect = null)
    {
        _store = history;
        _connect = connect;
        _diagnosticsLog = diagnosticsLog ?? new NoOpDiagnosticsLog();
        _scenarios = scenarios ?? new ScenarioPicker();
        _canReachElevated = canReachElevated;

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

        // The scenario list and the relative line fill the same field the At row edits, through ONE set of
        // requests, so a late answer from either can never overwrite whichever was asked later or a date
        // typed since (R4-S25). Both read the zone at the moment of use, so changing the zone changes what
        // "now plus one day" means without any wiring between the two.
        _moments = new MomentRequests(Moment, calcClient, () => SelectedZone.BiasMinutes, momentWait);
        Relative = new RelativeMomentViewModel(_moments);

        // A Start refused for a date still being computed comes back the moment that date lands, or the
        // moment the field is typed in - both make the field current again.
        _moments.CurrentChanged += (_, _) =>
        {
            if (_momentPending && _moments.IsCurrent)
            {
                _momentPending = false;
                RaisePropertyChanged(nameof(CanStart));
                RaisePropertyChanged(nameof(HasContract));
                RaiseStartRefusalChanged();
            }
        };

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
            if (!_moments.FillingFromScenario)
            {
                ClearScenarioSelection();
            }
        };

        History.CollectionChanged += (_, _) =>
        {
            RaisePropertyChanged(nameof(HasHistory));
            RaisePropertyChanged(nameof(ShowsHistorySection));
        };
        RecentTargets.CollectionChanged += (_, _) => RaisePropertyChanged(nameof(HasRecentTargets));
        foreach (var record in _store.Load())
        {
            History.Insert(0, record); // newest first for display
        }

        SeedRecentTargets();

        // The panel's actions, lifted off this view model so it holds state and they hold behaviour (GUI
        // rule 15). Built last, once Relative and the scenario picker exist for the commands to reach. The
        // window that hosts the phases attaches a shell to these later (SessionCommands.AttachShell) - this
        // view model never sees it (rule 16).
        Commands = new SessionCommands(this);
    }
    // Diagnostics captured when a session ends in anything but a clean success (RELEASE-012): the core's
    // stderr and parse errors, composed into a block the user can copy and that is also written to a log
    // file beside the exe. Empty on a clean works session, so the button and log stay out of the happy path.
    private string _diagnosticsText = string.Empty;
    private string _diagnosticsSavedPath = string.Empty;

    /// <summary>The panel's actions as bindable commands, so the phase views bind <c>Command</c> and carry
    /// no Click handler of their own (GUI rules 11 and 15). The window-dependent actions (pickers, clipboard,
    /// confirm, the support link) join them in the next slice through a shell service.</summary>
    public SessionCommands Commands { get; }

    public ClockView Fake { get; } = new("clock.fake");

    public ClockView Real { get; } = new("clock.real");

    /// <summary>Translation key for the current status label (the view renders it in the user's language).</summary>
    public string StatusKey { get => _result.StatusKey; private set => Set(ref _result.StatusKey, value); }

    public SessionStatusKind StatusKind
    {
        get => _result.StatusKind;
        private set
        {
            if (Set(ref _result.StatusKind, value))
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
        RaiseEvidenceCaveatsChanged();
        RaisePropertyChanged(nameof(ResultHasEnding));
        RaisePropertyChanged(nameof(AuditExplainsVerdict));
        RaisePropertyChanged(nameof(AuditExplainsMeaning));
        RaisePropertyChanged(nameof(AuditStartsOpen));
        RaisePropertyChanged(nameof(IsCutShort));
    }

    /// <summary>The caveats under a works headline follow the verdict and the warnings alike.</summary>
    private void RaiseEvidenceCaveatsChanged()
    {
        RaisePropertyChanged(nameof(ResultEvidenceCaveats));
        RaisePropertyChanged(nameof(ResultHasEvidenceCaveats));
    }

    /// <summary>
    /// The core stopped before it closed the session (R4-W3): no <c>ended</c> reached the panel. A core that
    /// died or went quiet ends on <see cref="SessionStatusKind.CoreStopped"/> or
    /// <see cref="SessionStatusKind.CoreUnresponsive"/>, which are only ever set when no <c>ended</c> came -
    /// one that did would have left the session on <see cref="SessionStatusKind.Ended"/>. A Stop is the one
    /// ending that can go either way, because the core is killed when it does not end in time.
    /// </summary>
    public bool IsCutShort => _result.IsCutShort;

    /// <summary>True while the session is live - the in-flight controls bind their visibility to this.</summary>
    public bool IsRunning => _result.StatusKind == SessionStatusKind.Running;

    /// <summary>
    /// The session phase's controls are still worth showing: the session is live, or on its way in or out.
    /// </summary>
    /// <remarks>
    /// 🔴 NOT <see cref="IsRunning"/>. An ended session kept the whole control card on screen, disabled, with a
    /// Stop that could stop nothing - nine buttons offered to a reader who could press none of them. They go
    /// once the session is over. Stopping is not over: the reader has just pressed Stop, and the card stays,
    /// disabled, until the core confirms, rather than vanishing under the pointer.
    /// </remarks>
    public bool ShowsSessionControls => !SessionResult.IsTerminal(_result.StatusKind);

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
    public bool ShowsSetupPhase => _result.StatusKind == SessionStatusKind.Idle;

    /// <summary>See <see cref="ShowsSetupPhase"/> - the result phase owns every terminal status.</summary>
    public bool ShowsResultPhase => SessionResult.IsTerminal(_result.StatusKind);

    /// <summary>See <see cref="ShowsSetupPhase"/> - a session on its way in, live, or shutting down.</summary>
    public bool ShowsSessionPhase => !ShowsSetupPhase && !ShowsResultPhase;

    /// <summary>True once a session has started (running or finished) - there is then something to copy.
    /// The Copy summary button binds its visibility to this (chrono-mock 7.2, 8.8).</summary>
    public bool CanCopySummary => _result.StatusKind is not (SessionStatusKind.Idle or SessionStatusKind.Connecting);

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
    public bool ShowsInFlightError => HasInFlightError && !SessionResult.IsTerminal(_result.StatusKind);

    /// <summary>Past sessions, newest first, for the History panel (docs/04 section 6). Loaded from the
    /// injected store on construction and prepended as each session ends.</summary>
    public ObservableCollection<SessionRecord> History { get; } = [];

    /// <summary>True when there is at least one recorded session - the History panel binds its visibility here.</summary>
    public bool HasHistory => History.Count > 0;

    /// <summary>Whether the history section is on the screen: while there is a recorded session, while it
    /// has to say where an unreadable history went - a clear that moved one leaves no rows, and the section
    /// that would say so used to go with them (R5-D73) - and while a write failed, for the same reason: a
    /// clear or the removal of the last row that the store refused took the section and its reason away.</summary>
    public bool ShowsHistorySection => HasHistory || HasHistorySetAside || HasHistoryError;

    /// <summary>Raw detail when a session could not be written to history (e.g. a read-only drive), empty
    /// otherwise. Surfaced, never swallowed (rule 6).</summary>
    public string HistoryError
    {
        get => _historyError;
        private set
        {
            if (Set(ref _historyError, value))
            {
                RaisePropertyChanged(nameof(HasHistoryError));
                RaisePropertyChanged(nameof(ShowsHistorySection));
            }
        }
    }

    /// <summary>True when a history write failed - the panel shows the reason (rule 6).</summary>
    public bool HasHistoryError => _historyError.Length > 0;

    /// <summary>Where recording this session moved a history this version cannot read, empty when it moved
    /// none (R5-D73). The move kept the old history (rule 7) and said nothing about it, so the panel opened on
    /// one row and the reader had no way to know their earlier sessions were kept, or where.</summary>
    public string HistorySetAside
    {
        get => _historySetAside;
        private set
        {
            if (Set(ref _historySetAside, value))
            {
                RaisePropertyChanged(nameof(HasHistorySetAside));
                RaisePropertyChanged(nameof(ShowsHistorySection));
            }
        }
    }

    /// <summary>True when recording this session set an unreadable history aside - the panel says where.</summary>
    public bool HasHistorySetAside => _historySetAside.Length > 0;

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

    /// <summary>Translation key for how the target was filled in, when that filled more than the target -
    /// a shortcut brings its arguments and working folder, which stand in a folded section (R5-D80). Empty
    /// otherwise, and emptied when the target changes or a session starts.</summary>
    public string TargetNoteKey
    {
        get => _targetNoteKey;
        private set { if (Set(ref _targetNoteKey, value)) { RaisePropertyChanged(nameof(HasTargetNote)); } }
    }

    /// <summary>True while <see cref="TargetNoteKey"/> has something to say.</summary>
    public bool HasTargetNote => _targetNoteKey.Length > 0;

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
                TargetNoteKey = string.Empty; // a note on how the target was filled is about the one before
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

    /// <summary>
    /// Whether the footer promises the application this moment: the field holds one, and it is not standing
    /// in for a chosen scenario that gave no date (R4-Z2).
    /// </summary>
    /// <remarks>
    /// The promise and the field are two things here. The moment section's own summary goes on describing
    /// what the field holds, but the footer is the contract above Start, and beside a Start that refuses
    /// it promised the date from before the choice - one the tester did not choose and Start would not use.
    /// A bool rather than a type, at this class's coupling ceiling (gui/CodeMetricsConfig.txt).
    /// </remarks>
    public bool HasContract => HasMomentPreview && !HasScenarioError && !_momentPending;

    private void RaiseMomentPreviewChanged()
    {
        RaisePropertyChanged(nameof(MomentPreview));
        RaisePropertyChanged(nameof(HasMomentPreview));
        RaisePropertyChanged(nameof(HasContract));
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
        set
        {
            if (Set(ref _selectedZone, value))
            {
                RaiseMomentPreviewChanged();
                AskAgainInZone();
            }
        }
    }

    /// <summary>
    /// 🔴 The zone is part of every question that fills the moment, so a new zone asks again (R4-Z1). A
    /// chosen scenario is a wall clock computed IN the zone, and kept as it was it named another instant in
    /// the new one: the 2038 boundary chosen in UTC and then read at UTC+01:00 is an hour short of it, under
    /// a selection that still claims the boundary. A "now, shifted" still being computed is asked again for
    /// the same reason. A date the user typed is theirs and stays as typed.
    /// </summary>
    private void AskAgainInZone()
    {
        if (_selectedScenario is { } scenario)
        {
            _ = ApplyScenarioAsync(scenario);
        }
        else if (_moments.IsRelativePending)
        {
            _ = Relative.ApplyAsync();
        }
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

    /// <summary>Start the web engine with its slowdown of timers in hidden windows switched off (R4-N28): a
    /// minimized window then keeps the session speed, and the application works at full pace while hidden.
    /// For a Chromium or Electron application and the web pages inside an application. Off by default.
    /// Maps to the wire <c>target.keep_background_timers</c>. Start-only.</summary>
    public bool KeepBackgroundTimers { get => _keepBackgroundTimers; set => Set(ref _keepBackgroundTimers, value); }

    /// <summary>Run even when the opening verdict says the substitution did not take effect. Off by
    /// default: the core stops the target in that case, because an application that looks time-shifted
    /// but is not produces evidence about a session that never happened. Start-only.</summary>
    public bool ForceStart { get => _forceStart; set => Set(ref _forceStart, value); }

    /// <summary>Reach the web pages inside the application through its embedded web engine's debugging
    /// port (docs/09). ON by default - covering those pages is the product's promise, and what the switch
    /// buys the tester who turns it off is that no debugging port opens in their application for the
    /// session. Maps to the wire <c>target.embedded</c>. Start-only.</summary>
    public bool ReachEmbedded
    {
        get => _reachEmbedded;
        set
        {
            if (Set(ref _reachEmbedded, value))
            {
                RaisePropertyChanged(nameof(LeavesEmbeddedPages));
                RaisePropertyChanged(nameof(ElevatedEmbeddedEnabled));
                // The option below rides on this one, so turning the channel off turns it off too: a box
                // left ticked and greyed would claim a session that Start then does not run.
                if (!value)
                {
                    ReachElevatedEmbedded = false;
                }
            }
        }
    }

    /// <summary>The opt-out, for the folded header's chip: reaching the pages is the default, so the chip
    /// shows only when the tester turned it off - a chip for every session would say nothing.</summary>
    public bool LeavesEmbeddedPages => !_reachEmbedded;

    /// <summary>Whether this window runs as administrator, which is what makes the option below usable:
    /// an application started from an elevated window is elevated too, and WebView2 then ignores the
    /// setting the session reaches its pages through (docs/09 section 12.19). Told once at construction -
    /// a process cannot change its token - and false for a view model built bare.</summary>
    public bool CanReachElevatedEmbedded => _canReachElevated;

    /// <summary>The other side of <see cref="CanReachElevatedEmbedded"/>, for the line that says what the
    /// option needs when it cannot be used - a view binds a visibility to a flag, not to its negation.</summary>
    public bool ElevatedEmbeddedUnavailable => !_canReachElevated;

    /// <summary>Whether the option can be changed now: idle, an elevated window, and the channel it rides
    /// on still on.</summary>
    public bool ElevatedEmbeddedEnabled => _idle && _canReachElevated && _reachEmbedded;

    /// <summary>Reach the web pages of an application that runs as administrator: the session writes one
    /// WebView2 value to the machine registry for its duration and removes it at the end (docs/09 section
    /// 12.19). OFF by default, because it changes the machine and opens a debugging port in an application
    /// with administrator rights. Maps to the wire <c>target.elevated_embedded</c>. Start-only.</summary>
    public bool ReachElevatedEmbedded
    {
        get => _reachElevatedEmbedded;
        set => Set(ref _reachElevatedEmbedded, value);
    }

    /// <summary>What Start asks of the core: the tick, held to what can be done. A window that is not
    /// elevated cannot ask (the core would refuse it, and the box is greyed), and the option has nothing to
    /// ride on without the channel.</summary>
    private bool WantsElevatedEmbedded => _reachElevatedEmbedded && _canReachElevated && _reachEmbedded;

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

    /// <summary>True when a session may be started: nothing is running, a target is chosen, the moment is
    /// valid, and it is not standing in for a chosen scenario that gave no date (R4-Z2) - the field then
    /// still holds the date from before the choice, and starting with it would start a session the tester
    /// did not choose. Nor while a Start already gave up waiting for the newest date to be computed - it
    /// comes back the moment that date lands or the field is typed in.</summary>
    public bool CanStart => _idle && HasTarget && Moment.IsValid && !HasScenarioError && !_momentPending;

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
        : HasScenarioError ? "setup.scenario_failed"
        : _momentPending ? "setup.moment_pending"
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

    /// <summary>Choose the target executable to run (from the picker, the recent list, or the dev default).
    /// A file that has to be checked or read first - a drop, a shortcut - comes through
    /// <see cref="TargetIntake"/>.</summary>
    public void SetTarget(string path) => TargetPath = path;

    /// <summary>Take the program a shortcut starts, with the arguments and working folder it starts it with
    /// (R5-D80), and say so - the two fields stand in a folded section, and filling them where nobody can see
    /// would hand a session arguments the tester never read. Nothing starts (rule 7).</summary>
    public void SetTargetFromShortcut(string program, string arguments, string workingFolder)
    {
        SetTarget(program);
        TargetArgs = arguments;
        WorkingFolder = workingFolder;
        TargetNoteKey = "setup.from_shortcut";
    }

    /// <summary>
    /// Take a moment built elsewhere (the calculator's "Use this date"), with its zone (rule 2 - a moment
    /// never travels without one). On the setup form it becomes the start moment and the zone, and a zone
    /// the closed catalogue does not offer leaves the current one standing, which the calculator already
    /// refuses to send. While a session runs it becomes the Jump to target instead, in the session's zone.
    /// Nothing starts and nothing jumps (rule 7).
    /// </summary>
    /// <remarks>
    /// 🔴 It leaves a finished result first, the way <see cref="LoadFromHistory"/> does. Idle is true once a
    /// session has ended, so the moment used to land on the setup form while the result screen stayed on
    /// top of it: the press switched to this module and showed the same verdict as before, with the new
    /// date invisible behind it.
    /// </remarks>
    /// <returns>True when a field took the moment, false when nothing did - a session on its way in or
    /// out, or a running one handed a text the jump field could not re-express. The caller switches to
    /// this module only on true, so a press that adopted nothing does not land on a form that shows
    /// nothing of it.</returns>
    public bool AdoptMoment(string momentLocal, int zoneBias)
    {
        if (IsRunning)
        {
            // A live run's START is not up for editing, but its clock is: the date goes to the Jump to
            // field, re-expressed in the session's zone (which cannot change in flight), and waits for
            // the Jump button - the press fills a field, it never jumps by itself (rule 7). It used to
            // switch to this module and leave the field empty, which read as the press doing nothing.
            return JumpMoment.LoadInZone(momentLocal, zoneBias, SelectedZone.BiasMinutes);
        }

        if (!_idle)
        {
            return false;
        }

        BeginNewSession();
        Moment.LoadCanonical(momentLocal);
        SelectedZone = Zones.FirstOrDefault(z => z.BiasMinutes == zoneBias) ?? SelectedZone;
        return true;
    }

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

    /// <summary>
    /// Read the scenario list, once the window is shown. Never from the constructor: reading the catalogue
    /// runs the engine now (R4/18), and a window built in a test must start nothing. Without a catalogue (a
    /// test that gives none) the list is read as empty.
    /// </summary>
    public Task EnsureScenariosAsync()
        => _scenarios.LoadAsync();

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
                _scenarios.SetChosen(value is not null);
                ScenarioExplains = value?.DisplayExplains ?? string.Empty;
                if (value is not null)
                {
                    _ = ApplyScenarioAsync(value);
                }
            }
        }
    }

    /// <summary>Whether a scenario is chosen. The other half - the number on offer, shown only while none is
    /// and the list is read - is <see cref="ScenarioPicker.ShowsCount"/>, told by
    /// <see cref="ScenarioPicker.SetChosen"/>, so this class carries no type for the list's state (its
    /// coupling ceiling, gui/CodeMetricsConfig.txt).</summary>
    public bool HasSelectedScenario => _selectedScenario is not null;

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
        private set
        {
            if (Set(ref _scenarioErrorKey, value))
            {
                RaisePropertyChanged(nameof(HasScenarioError));
                RaisePropertyChanged(nameof(CanStart));
                RaisePropertyChanged(nameof(HasContract));
                RaiseStartRefusalChanged();
            }
        }
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
    /// <para>
    /// The answer fills the field only while this is still the newest request and nothing was typed since
    /// (R4-S25) - see <see cref="MomentRequests"/>.
    /// </para>
    /// </summary>
    internal async Task ApplyScenarioAsync(ScenarioItem scenario)
    {
        ArgumentNullException.ThrowIfNull(scenario);
        ScenarioErrorKey = string.Empty;

        if (await _moments.ChooseScenarioAsync(scenario) is { } errorKey)
        {
            ScenarioErrorKey = errorKey;
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
        _scenarios.SetChosen(false);
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

    /// <summary>Jump the wall to the moment typed in the session's jump field, in the session zone (rule 2).
    /// No-op if that moment is malformed (the Jump button is disabled then) or no session is running.</summary>
    public void JumpToEnteredMoment()
    {
        if (JumpMoment.IsValid)
        {
            SendJumpAbsolute(JumpMoment.Canonical, SelectedZone.BiasMinutes);
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

    /// <summary>True once the session has ended AND been closed down, so a finished result can be returned to
    /// a fresh setup form (New session). Only the terminal states qualify - a live or a still-closing session
    /// is not a result to leave yet. Tracks the same set as <see cref="ShowsSessionControls"/>, from the other
    /// side.
    /// <para>
    /// The status turns terminal as the event stream ends, and the start task then still records the session,
    /// disposes the core and captures its diagnostics before it goes idle - up to about two seconds when the
    /// core has to be stopped. A new form begun in between was then written over by that tail (R4-S27), so
    /// the button waits for idle as well.
    /// </para></summary>
    public bool CanBeginNewSession => SessionResult.IsTerminal(_result.StatusKind) && _idle;

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
        // The mode the session STARTED with, not the speed it was left at (R5-D51). The live speed buttons
        // move the Mode list while the session runs (SyncModeToMultiplier), so the new form came up at x60
        // after a session started in flow. The list is on the setup screen only, so nobody changed it by
        // hand in between, and the callers that fill the form from history or the calculator do it after
        // this. Not in ResetSession, which also runs at Start, where the list is the user's choice.
        if (_started is { } started)
        {
            SelectedMode = started.Mode;
        }

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
                RaisePropertyChanged(nameof(ElevatedEmbeddedEnabled));
            }
        }
    }

    /// <summary>True once a verdict has arrived - the indicator stays hidden until then.</summary>
    public bool VerdictKnown { get => _result.VerdictKnown; private set => Set(ref _result.VerdictKnown, value); }

    /// <summary>The verdict kind, driving the indicator's glyph and colour (chrono-mock 7.1).</summary>
    public VerdictKind VerdictKind { get => _result.VerdictKind; private set => Set(ref _result.VerdictKind, value); }

    /// <summary>Translation key for the verdict label ("verdict.works" etc.).</summary>
    public string VerdictLabelKey { get => _result.VerdictLabelKey; private set => Set(ref _result.VerdictLabelKey, value); }

    /// <summary>The core's specific reason key (rendered raw if untranslated), shown for a non-works verdict.</summary>
    public string VerdictReasonKey { get => _result.VerdictReasonKey; private set => Set(ref _result.VerdictReasonKey, value); }

    /// <summary>The plain-language "what this means for the test" key, shown for a non-works verdict.</summary>
    public string VerdictMeaningKey { get => _result.VerdictMeaningKey; private set => Set(ref _result.VerdictMeaningKey, value); }

    public bool VerdictHasReason { get => _result.VerdictHasReason; private set => Set(ref _result.VerdictHasReason, value); }

    public bool VerdictHasMeaning { get => _result.VerdictHasMeaning; private set => Set(ref _result.VerdictHasMeaning, value); }

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
    public string ResultHeadlineKey => _result.StatusKind switch
    {
        SessionStatusKind.DidNotTakeEffect => "result.headline_did_not_take_effect",
        SessionStatusKind.Error when !_result.HasTiming => "result.headline_did_not_start",
        _ when IsCutShort => "result.headline_cut_short",
        _ => _result.VerdictKnown ? _result.VerdictLabelKey : "result.headline_no_verdict",
    };

    /// <summary>The kind behind <see cref="ResultHeadlineKey"/>, driving its glyph and colour: an outcome
    /// without a verdict word is drawn as a failure when the substitution never took effect and as
    /// undetermined when nothing was judged.</summary>
    public VerdictKind ResultKind => _result.StatusKind switch
    {
        SessionStatusKind.DidNotTakeEffect => VerdictKind.Fails,
        SessionStatusKind.Error when !_result.HasTiming => VerdictKind.Fails,
        _ when IsCutShort => VerdictKind.Undetermined,
        _ => _result.VerdictKnown ? _result.VerdictKind : VerdictKind.Undetermined,
    };

    /// <summary>The headline is the verdict word itself, so the verdict's own reason and meaning belong under it.
    /// Under any other headline they would explain a word that is not on the screen.</summary>
    private bool ResultIsVerdict
        => _result.VerdictKnown && ResultHeadlineKey == _result.VerdictLabelKey;

    /// <summary>
    /// The core's reason for the verdict, for EVERY verdict the result phase leads with.
    /// </summary>
    /// <remarks>
    /// 🔴 UNLIKE <see cref="VerdictHasReason"/>, which hides the reason under a clean "works" because a badge
    /// in a footer needs no caveat. The result phase exists to explain, and to somebody on their first run
    /// "Works" alone says nothing about what worked - the core's sentence ("every process that read the time
    /// saw the fake clock") is the explanation, and it arrives for works as for the rest.
    /// </remarks>
    public bool ResultHasReason => ResultIsVerdict && _result.VerdictReasonKey.Length > 0;

    /// <summary>The plain-language meaning, under the headline it is about (see <see cref="ResultHasReason"/>).</summary>
    public bool ResultHasMeaning => ResultIsVerdict && _result.VerdictHasMeaning;

    /// <summary>
    /// The warnings that undo a works headline as evidence (<see cref="WarningsThatUndoEvidence"/>), each as
    /// its own sentence under the reason. Empty under any other headline.
    /// </summary>
    /// <remarks>
    /// 🔴 THE SCREEN SAYS WHAT THE COPY SAYS. A works session of a Go program, or of an application that read
    /// none of the clocks the session replaces, gets the unreliable banner in the copied summary (R5-D3), and
    /// the screen showed a green "Works" with both sentences folded inside the audit, behind a warning count.
    /// The CLI prints them in its report, so the window was the one place a tester could miss them. The
    /// verdict stays, as it does in the CLI. A partial or failing verdict has its meaning line and an open
    /// audit already, so the sentences are not repeated there.
    /// </remarks>
    public IReadOnlyList<string> ResultEvidenceCaveats
        => ResultIsVerdict && _result.VerdictKind == VerdictKind.Works ? [.. _result.EvidenceUndoingWarnings] : [];

    public bool ResultHasEvidenceCaveats => ResultEvidenceCaveats.Count > 0;

    /// <summary>The status line tells how the session ended, unless the headline already did: a target that
    /// vanished has "did not take effect" as its headline and how long it lived under it, and the status
    /// sentence would say the same thing a third time.</summary>
    public bool ResultHasEnding => _result.StatusKind != SessionStatusKind.DidNotTakeEffect;

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
    public bool AuditExplainsVerdict => _result.VerdictHasReason && !SessionResult.IsTerminal(_result.StatusKind);

    /// <summary>The meaning line of the audit block, on the same terms as <see cref="AuditExplainsVerdict"/>.</summary>
    public bool AuditExplainsMeaning => _result.VerdictHasMeaning && !SessionResult.IsTerminal(_result.StatusKind);

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
    public bool AuditStartsOpen => _result.CoverageKnown && _result.VerdictKnown && _result.VerdictKind != VerdictKind.Works;

    /// <summary>The core's reason key for a target that vanished, shown with <see cref="LivedMs"/>. On screen only
    /// for a session that did not take effect - the summary composes the same two into one line.</summary>
    public string VanishReasonKey => _result.VanishReasonKey;

    public bool HasVanishReason
        => _result.StatusKind == SessionStatusKind.DidNotTakeEffect && _result.VanishReasonKey.Length > 0;

    /// <summary>How long the vanished target lived, in milliseconds - what tells a single-instance hand-off
    /// (gone within a blink) from an application that ran and then quit.</summary>
    public long LivedMs => _result.LivedMs;

    /// <summary>True once a heartbeat or the end timing arrived: the facts block of the result phase has
    /// something to say. A start that failed or was refused has no timing and shows no facts.</summary>
    public bool HasTiming => _result.HasTiming;

    /// <summary>The moment the session STARTED at, formatted like <see cref="MomentPreview"/>, from the start
    /// snapshot rather than the form - the form is unlocked once the session is over.</summary>
    public string StartedAtPreview => ClockView.FormatMoment(Requested.Moment, Requested.Zone.Label);

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
        => ClockView.FormatMoment(_result.FakeEndWall.Length > 0 ? _result.FakeEndWall : Fake.Wall, Fake.Zone);

    /// <summary>True when a raw failure detail is being shown (see <see cref="LastError"/>).</summary>
    public bool HasLastError => _lastError.Length > 0;

    /// <summary>Size of the process family the session verdict covers (parent plus children).</summary>
    public int ProcessCount
    {
        get => _result.ProcessCount;
        private set
        {
            if (Set(ref _result.ProcessCount, value))
            {
                RaisePropertyChanged(nameof(IsFamily));
                RaisePropertyChanged(nameof(HasCoverageNote));
            }
        }
    }

    /// <summary>True when the session spanned more than one process (the verdict is the family aggregate).</summary>
    public bool IsFamily => _result.IsFamily;

    /// <summary>True once a coverage report has arrived - the audit block stays hidden until then.</summary>
    public bool CoverageKnown
    {
        get => _result.CoverageKnown;
        private set
        {
            if (Set(ref _result.CoverageKnown, value))
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
    public bool IsAuditPending => !_result.CoverageKnown && !SessionResult.IsTerminal(_result.StatusKind);

    /// <summary>
    /// The session is over and no audit ever arrived, so the place for it has to say that it will not.
    /// </summary>
    /// <remarks>
    /// 🔴 A PROMISE THE TOOL CANNOT KEEP. The session phase told an ended session with no report that the list
    /// would appear once the tool had checked - after the application had vanished, with nothing left to
    /// check.
    /// </remarks>
    public bool AuditNeverArrived => !_result.CoverageKnown && SessionResult.IsTerminal(_result.StatusKind) && !AuditNeverStarted;

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
        => !_result.CoverageKnown && _result.StatusKind == SessionStatusKind.Error && !_result.HasTiming;

    /// <summary>True when this session is driven over CDP (a Chromium/Electron target, ADR-9). The coverage
    /// unit is then a JS context, not an OS process, so the audit accumulates every context and the note
    /// reflects that. Set once at start from the plan.</summary>
    /// <remarks>The setter is internal rather than private so a test can drive the CDP coverage branch
    /// without a Chromium target and a live debug port. Production still sets it in exactly one place
    /// (from the session plan) - the widening buys a test for how contexts are told apart, which is a
    /// thing the panel gets wrong silently when it is wrong.</remarks>
    public bool IsCdp
    {
        get => _result.IsCdp;
        internal set
        {
            if (Set(ref _result.IsCdp, value))
            {
                RaisePropertyChanged(nameof(CoverageNoteKey));
                RaisePropertyChanged(nameof(HasCoverageNote));
            }
        }
    }

    /// <summary>Translation key for the coverage note: a CDP session shows every JS context, a native
    /// family session shows the parent's call counts with the whole family's warnings and uncovered
    /// channels (rule 4 - counts are never summed across processes).</summary>
    public string CoverageNoteKey => _result.IsCdp ? "coverage.contexts_note" : "coverage.family_note";

    /// <summary>Whether to show the coverage note: for CDP once coverage arrived (it spans contexts), for
    /// native only when the session spanned a process family.</summary>
    public bool HasCoverageNote => _result.IsCdp ? _result.CoverageKnown : IsFamily;

    /// <summary>Covered channels, formatted "channel  xN", from the parent process (never summed, rule 4).</summary>
    public IReadOnlyList<CoveredChannel> Covered
    {
        get => _result.Covered;
        private set { if (SetItems(ref _result.Covered, value)) { RaisePropertyChanged(nameof(HasCovered)); RaisePropertyChanged(nameof(HasAuditRows)); } }
    }

    /// <summary>Channels hooked but deliberately left real (e.g. QPC-based waits, ADR-2), formatted "channel  xN".</summary>
    public IReadOnlyList<CoveredChannel> Observed
    {
        get => _result.Observed;
        private set { if (SetItems(ref _result.Observed, value)) { RaisePropertyChanged(nameof(HasAuditRows)); } }
    }

    /// <summary>Uncovered channel identifiers (raw API names, not translation keys) - the partial verdict's
    /// evidence, unioned over every process of the family (R2-W5).</summary>
    public IReadOnlyList<string> Uncovered
    {
        get => _result.Uncovered;
        private set { if (SetItems(ref _result.Uncovered, value)) { RaisePropertyChanged(nameof(HasUncovered)); RaisePropertyChanged(nameof(HasAuditRows)); } }
    }

    /// <summary>Channels the session meant to watch and could not hook, unioned over the family. Its own
    /// list rather than part of <see cref="Uncovered"/>, because it is not the partial verdict's evidence
    /// - it is the panel admitting that a watch it promised is not running.</summary>
    public IReadOnlyList<string> Unobserved
    {
        get => _result.Unobserved;
        private set { if (SetItems(ref _result.Unobserved, value)) { RaisePropertyChanged(nameof(HasUnobserved)); RaisePropertyChanged(nameof(HasAuditRows)); } }
    }

    /// <summary>Channels that came under the fake clock only once their module loaded, unioned over the
    /// family. Every one is also in <see cref="Covered"/> or <see cref="Observed"/> with its count - this
    /// names which of those counts are floors, which the late-hook warning needs and cannot say itself.</summary>
    public IReadOnlyList<string> InstalledLate
    {
        get => _result.InstalledLate;
        private set { if (SetItems(ref _result.InstalledLate, value)) { RaisePropertyChanged(nameof(HasAuditRows)); } }
    }

    /// <summary>Warning translation keys the core raised (rendered in the current language), unioned over
    /// every process of the family and the session aggregate (R2-W5, R2-S9).</summary>
    public IReadOnlyList<string> Warnings
    {
        get => _result.Warnings;
        private set
        {
            if (SetItems(ref _result.Warnings, value))
            {
                RaisePropertyChanged(nameof(HasWarnings));
                RaiseEvidenceCaveatsChanged();
            }
        }
    }

    /// <summary>The processes this session's family spawned without the hook inside them, as the session
    /// verdict names them - each ran on the real clock the whole time. Raw wire records: the screen folds
    /// them into a table by executable and role (UncoveredProcessRowsConverter), the copied summary prints
    /// them one per pid the way the CLI report does. The wire names at most a fixed number of them
    /// (UNCOVERED_CHILDREN_WIRE_MAX) - <see cref="UncoveredChildrenTotal"/> is the true count regardless.</summary>
    public IReadOnlyList<UncoveredChild> UncoveredChildren
    {
        get => _result.UncoveredChildren;
        private set
        {
            if (Set(ref _result.UncoveredChildren, value))
            {
                RaisePropertyChanged(nameof(UncoveredChildrenUnnamed));
                RaisePropertyChanged(nameof(HasUnnamedUncoveredChildren));
            }
        }
    }

    /// <summary>How many processes ran without the hook, named or not - the number the heading and the
    /// chip print. Never below the length of the named list: a core that counted fewer than it named would
    /// be a core contradicting itself, and the screen prints the larger of the two rather than a count
    /// smaller than the rows under it.</summary>
    public int UncoveredChildrenTotal
    {
        get => _result.UncoveredChildrenTotal;
        private set
        {
            if (Set(ref _result.UncoveredChildrenTotal, value))
            {
                RaisePropertyChanged(nameof(HasUncoveredChildren));
                RaisePropertyChanged(nameof(UncoveredChildrenUnnamed));
                RaisePropertyChanged(nameof(HasUnnamedUncoveredChildren));
            }
        }
    }

    /// <summary>How many of the counted processes the report could not name - the ones past the wire's cap.
    /// Said in words under the table, so a total above the rows is never a mismatch left for the reader to
    /// notice (untouchable rule 4). Distinct from a named row that has no image: that process was seen and
    /// gone before it could be asked, this one was never listed at all.</summary>
    public int UncoveredChildrenUnnamed => _result.UncoveredChildrenUnnamed;

    public bool HasUncoveredChildren => _result.UncoveredChildrenTotal > 0;

    /// <summary>The web engines inside the application the session reached through their debugging ports,
    /// as the verdict names them - whether any of their pages was put on the session clock is
    /// <see cref="EnginePagesReached"/>, not this list. Raw wire records: the
    /// screen folds them into a table of name and port (EngineRowsConverter), the copied summary prints
    /// the pid beside them the way the CLI report does. Empty for a session that found none, and for a
    /// Chromium session, which opens its own port rather than finding one.</summary>
    public IReadOnlyList<ReachedEngine> Engines
    {
        get => _result.Engines;
        private set { if (Set(ref _result.Engines, value)) { RaisePropertyChanged(nameof(HasEngines)); } }
    }

    public bool HasEngines => _result.Engines.Count > 0;

    /// <summary>
    /// Whether the session put any page on its clock through an engine's debugging port - the session
    /// verdict's context count, read as the CLI report reads it (R5, P15). The engines note and the copied
    /// summary say only what this says: they used to say the engines' pages ran on the session clock
    /// whenever an engine was reached, also when no page was put on it.
    /// </summary>
    public bool EnginePagesReached
    {
        get => _result.EnginePagesReached;
        private set { if (Set(ref _result.EnginePagesReached, value)) { RaisePropertyChanged(nameof(EnginesNoteKey)); } }
    }

    /// <summary>The note under the engines table, one of two keys by <see cref="EnginePagesReached"/>.</summary>
    public string EnginesNoteKey => _result.EnginePagesReached ? "audit.engines_note" : "audit.engines_note_none";

    public bool HasUnnamedUncoveredChildren => UncoveredChildrenUnnamed > 0;

    /// <summary>The target's own exit code, from <c>ended.target_exit_code</c> - present only for a native
    /// session whose app exited on its own (null for a Stop/end or a CDP session). Informational, never a
    /// verdict - shown so the reader can tell "the app closed itself (code N)" from "the session was stopped".</summary>
    public int? TargetExitCode
    {
        get => _result.TargetExitCode;
        private set
        {
            if (Set(ref _result.TargetExitCode, value))
            {
                RaisePropertyChanged(nameof(HasTargetExit));
                RaisePropertyChanged(nameof(TargetExitText));
            }
        }
    }

    public bool HasTargetExit => _result.TargetExitCode.HasValue;

    /// <summary>The exit code as the panel shows it, with the hexadecimal form of a crash (see
    /// <see cref="SessionResult.ExitCodeLabel"/>), or empty when there is none.</summary>
    public string TargetExitText => _result.TargetExitCode is int code ? SessionResult.ExitCodeLabel(code) : string.Empty;

    /// <summary>The processes the session went on for after the application closed (ADR-16), one line each
    /// in the CLI report's own spelling: "app.exe (pid 5150)", or "pid 5151" for one the process list could
    /// not name. Lines rather than wire records because the screen and the copied summary print the same
    /// text, and one formatter cannot disagree with itself. Under the exit code, because "exited on its own"
    /// alone reads as the end of the session, and for a launcher it was not.</summary>
    public IReadOnlyList<string> FollowedRows
    {
        get => _result.FollowedRows;
        private set { if (Set(ref _result.FollowedRows, value)) { RaisePropertyChanged(nameof(HasFollowed)); } }
    }

    public bool HasFollowed => _result.FollowedRows.Count > 0;

    /// <summary>The copies of the program running outside the session that the application most likely
    /// passed its work to (ADR-4, 2026-11-11), in the spelling of <see cref="FollowedRows"/>. They run on the
    /// real clock and are why the verdict is not Works, so they stand on the result itself, under the exit
    /// code, where the tester looks for what happened to the application. Empty for every other session.</summary>
    public IReadOnlyList<string> CopiesOutsideRows
    {
        get => _result.CopiesOutsideRows;
        private set { if (Set(ref _result.CopiesOutsideRows, value)) { RaisePropertyChanged(nameof(HasCopiesOutside)); } }
    }

    public bool HasCopiesOutside => _result.CopiesOutsideRows.Count > 0;

    /// <summary>The processes of the application a refusal could not end, from <c>verdict.left_running</c>
    /// (R4-S5), in the spelling of <see cref="FollowedRows"/>. They run on the real clock, which the status
    /// line alone would hide - it says the application was ended. Empty for every session not refused.</summary>
    public IReadOnlyList<string> LeftRunningRows
    {
        get => _result.LeftRunningRows;
        private set { if (Set(ref _result.LeftRunningRows, value)) { RaisePropertyChanged(nameof(HasLeftRunning)); } }
    }

    public bool HasLeftRunning => _result.LeftRunningRows.Count > 0;

    /// <summary>Whether a refusal could not look for all of the application's processes, from
    /// <c>verdict.family_search_incomplete</c> - some may run on the real clock unnamed, so the status that
    /// says the application was ended must not stand alone. False for every session not refused.</summary>
    public bool RefusalIncomplete
    {
        get => _result.RefusalIncomplete;
        private set => Set(ref _result.RefusalIncomplete, value);
    }

    /// <summary>Cleanup residue translation keys from <c>ended.residue_keys</c> - what a teardown could not
    /// remove (today only a CDP temp profile that stayed locked). Empty on a clean end - rendered as
    /// warnings so a session reports the mess it left rather than hiding it (rule 6).</summary>
    public IReadOnlyList<string> ResidueKeys
    {
        get => _result.ResidueKeys;
        private set { if (Set(ref _result.ResidueKeys, value)) { RaisePropertyChanged(nameof(HasResidue)); } }
    }

    public bool HasResidue => _result.ResidueKeys.Count > 0;

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

    public bool HasCovered => _result.Covered.Count > 0;

    public bool HasUncovered => _result.Uncovered.Count > 0;

    public bool HasUnobserved => _result.Unobserved.Count > 0;

    /// <summary>Whether the audit table has a row to show: any function the session saw, wherever it read
    /// from. The table folds the four channel lists into one (AuditRowsConverter), so its visibility is
    /// the union of theirs. HasObserved and HasInstalledLate went with the separate lists they gated.</summary>
    public bool HasAuditRows => _result.Covered.Count > 0 || _result.Observed.Count > 0 || _result.Uncovered.Count > 0
        || _result.Unobserved.Count > 0 || _result.InstalledLate.Count > 0;

    public bool HasWarnings => _result.Warnings.Count > 0;

    /// <summary>How long the machine slept during the session, written like the elapsed lines beside it, or
    /// empty when it did not sleep (R5-D71). The session clock stood still for that long.</summary>
    public string HostSlept => HasHostSlept ? ClockView.FormatDuration(_result.HostSleptMs) : string.Empty;

    /// <summary>True when the machine slept during the session - the result's fact row shows only then.</summary>
    public bool HasHostSlept => _result.HostSleptMs > 0;

    /// <summary>
    /// Fold one event into the view state. Pure and synchronous (no I/O, no threading) so the mapping is
    /// unit-testable - the live loop marshals each call onto the UI thread. A late <c>state</c> after a
    /// terminal outcome is ignored, so a finished session is never resurrected as "running".
    /// </summary>
    public void Apply(ChronoEvent evt)
    {
        NoteForReport(evt);
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
                _result.ElapsedRealMs = s.ElapsedRealMs;
                _result.ElapsedFakeMs = s.ElapsedFakeMs;
                PublishElapsed();
                SetStatus("status.running", SessionStatusKind.Running);
                break;
            case VanishedEvent vd:
                _result.VanishReasonKey = vd.ReasonKey;
                _result.LivedMs = vd.LivedMs;
                RaisePropertyChanged(nameof(VanishReasonKey));
                RaisePropertyChanged(nameof(LivedMs));
                SetStatus("status.did_not_take_effect", SessionStatusKind.DidNotTakeEffect);
                break;
            // Guarded on the terminal state like `state` above (M-9): a late `ended`/`error` after a
            // terminal outcome (e.g. `ended` arriving right after `vanished` -> DidNotTakeEffect) must not
            // overwrite the honest "did not take effect" verdict in the summary and history.
            case EndedEvent e when !SessionResult.IsTerminal(StatusKind):
                _result.EndedSeen = true;
                if (e.FakeEndWall is not null)
                {
                    // The core's authoritative end timing (docs/08 section 6) - prefer it over the last heartbeat.
                    _result.FakeEndWall = e.FakeEndWall;
                    _result.ElapsedRealMs = e.ElapsedRealMs;
                    _result.ElapsedFakeMs = e.ElapsedFakeMs;
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
            case ErrorEvent err when !SessionResult.IsTerminal(StatusKind):
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
                // Empty and false unless the core refused and could not end, or look for, all of the
                // application (R4-S5).
                LeftRunningRows = SessionReport.ProcessLines(v.LeftRunning);
                RefusalIncomplete = v.FamilySearchIncomplete;
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
                // The processes the hook never got into, named on the one event that speaks for the whole
                // family - none of them has a coverage event of its own, that is what uncovered means. The
                // total is the true count and the list is the wire's capped slice of it, so the screen
                // prints the total and says how many it could not name.
                UncoveredChildren = sv.UncoveredChildren;
                // Plain comparisons rather than Math.Max: System.Math would be one more type on a class that
                // stands on its coupling ceiling, and the ceiling is for the one type this slice needs.
                UncoveredChildrenTotal = sv.UncoveredChildrenTotal > sv.UncoveredChildren.Count
                    ? sv.UncoveredChildrenTotal
                    : sv.UncoveredChildren.Count;
                // Taken as it comes, without a test on it: this method sits ON the cyclomatic ceiling
                // (CA1502, measured with tools/margin.ps1), so one more branch here would be a red build.
                // Everything the rows need deciding - a nameless engine, an endpoint seen twice - is
                // decided in the converter, which is where presentation belongs anyway.
                Engines = sv.Engines;
                EnginePagesReached = sv.ContextCount > 0;
                FollowedRows = SessionReport.ProcessLines(sv.Followed);
                CopiesOutsideRows = SessionReport.ProcessLines(sv.CopiesOutside);
                // Session-level warnings join the per-process ones (R2-S9). They are about the family,
                // not about any one process, so the panel shows them in the same list - a warning the
                // reader has to attribute to an event type is a warning they will not read.
                // The one that says something is STILL SET on this machine leads, ahead of what the
                // processes said: the list is shown through a window shorter than itself, and this line
                // may not sit below the fold. Written with comparisons and no new branch (this method
                // stands on the CA1502 ceiling).
                Warnings =
                [
                    .. sv.WarningKeys.Where(w => w == LeftBehindKey),
                    .. _result.Warnings,
                    .. sv.WarningKeys.Where(w => w != LeftBehindKey).Except(_result.Warnings),
                ];
                SetVerdict(VerdictKinds.Parse(sv.Verdict), sv.ReasonKey);
                break;
            case CoverageEvent c:
                ApplyCoverage(c);
                break;
            default:
                // ready and ack do not change the panel.
                break;
        }
    }

    /// <summary>
    /// What an event adds to the report beyond the panel's projection (<see cref="SessionResult.Absorb"/>,
    /// R5-D71), and the two things of it the panel shows: the sleep warning, on the list the moment the core
    /// notices rather than with the verdict at the end, and the slept time on the result.
    /// </summary>
    private void NoteForReport(ChronoEvent evt)
    {
        var sleptBefore = _result.HostSleptMs;
        if (_result.Absorb(evt))
        {
            Warnings = [.. _result.Warnings, .. SessionResult.HostSleptWarning.Except(_result.Warnings)];
        }

        if (_result.HostSleptMs != sleptBefore)
        {
            RaisePropertyChanged(nameof(HostSlept));
            RaisePropertyChanged(nameof(HasHostSlept));
        }
    }

    /// <summary>
    /// Fold one <c>coverage</c> event into the audit. Its own method so <see cref="Apply"/> holds one branch
    /// for it rather than four: Apply stood on the cyclomatic ceiling (CA1502 at 18), and every event the
    /// panel learned after that had to be squeezed past the limit instead of written plainly (R5/6b).
    /// </summary>
    private void ApplyCoverage(CoverageEvent c)
    {
        if (_result.IsCdp)
        {
            // CDP emits one coverage per JS context - accumulate them all (each context's counts stay
            // its own, never summed across contexts, rule 4) - union the warnings and uncovered lists.
            // Two pages both produce "page Date.now", so two contexts reading one channel become two
            // rows that tell apart only by their read count (rule 4 keeps each context's count its own,
            // never summed). The raw context id used to prefix each row - dropped in P6 as a number that
            // named nothing to the reader.
            Covered = [.. _result.Covered, .. c.Covered];
            Observed = [.. _result.Observed, .. c.Observed];
            Uncovered = [.. _result.Uncovered, .. c.Uncovered.Where(u => !_result.Uncovered.Contains(u))];
            Unobserved = [.. _result.Unobserved, .. c.Unobserved.Where(u => !_result.Unobserved.Contains(u))];
            InstalledLate = [.. _result.InstalledLate, .. c.InstalledLate.Where(u => !_result.InstalledLate.Contains(u))];
        }
        else if (c.Kind == CoverageEvent.UnitContext)
        {
            // A page or worker inside a natively hooked application, reached through its web
            // engine's debugging port (docs/09 section 12). Its number is a context index, never a
            // pid, so it must not be mistaken for the parent. Rows accumulate per context like the
            // CDP branch above and stand after the parent's, whichever event arrives last.
            _result.PageCovered = [.. _result.PageCovered, .. c.Covered];
            Covered = [.. _result.ParentCovered, .. _result.PageCovered];
        }
        else if (c.Kind == CoverageEvent.UnitProcess)
        {
            // Native. Call counts stay the PARENT's - the process the first event names - because
            // summing them across processes would fabricate a per-process picture (untouchable rule
            // 4), and a per-process family breakdown is a later slice. But a LATER event for that
            // same pid replaces the earlier one: the core reports a process when it is discovered
            // and again when the session ends, and the first snapshot is taken inside the guard
            // window, so its counts are the session's first blink (R2-X8) - measured, a probe that
            // read the clock 25 times showed "×2" in this panel.
            _result.ParentPid ??= c.Pid;
            if (c.Pid == _result.ParentPid)
            {
                _result.ParentCovered = c.Covered.ToList();
                Covered = [.. _result.ParentCovered, .. _result.PageCovered];
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
            Uncovered = [.. _result.Uncovered, .. c.Uncovered.Where(u => !_result.Uncovered.Contains(u))];
            Unobserved = [.. _result.Unobserved, .. c.Unobserved.Where(u => !_result.Unobserved.Contains(u))];
            InstalledLate = [.. _result.InstalledLate, .. c.InstalledLate.Where(u => !_result.InstalledLate.Contains(u))];
        }
        else
        {
            // A kind this build does not know - a newer core naming a unit this panel has never
            // heard of. Its number is neither a pid nor a context index, so it must not become the
            // parent and its counts have no row to go in. What it says about the session - the
            // warnings and the gaps - is evidence all the same, and stays (rule 4, rule 6).
            Uncovered = [.. _result.Uncovered, .. c.Uncovered.Where(u => !_result.Uncovered.Contains(u))];
            Unobserved = [.. _result.Unobserved, .. c.Unobserved.Where(u => !_result.Unobserved.Contains(u))];
        }

        // Every kind says what it warned about, and every kind is coverage the panel now knows.
        Warnings = [.. _result.Warnings, .. c.WarningKeys.Where(w => !_result.Warnings.Contains(w))];
        CoverageKnown = true;
    }

    /// <summary>
    /// Spawn the core, gate on <c>ready</c>, launch the demo target, and relay its state until the core
    /// closes the stream. Safe to call from a UI click handler - it never throws for the expected failures,
    /// it turns them into an honest status instead.
    /// </summary>
    public async Task StartAsync()
    {
        // 🔴 A press straight after choosing a scenario waits for its date, rather than start with the one
        // the field held before the choice (R4-S25). It completes at once when nothing is being computed,
        // and a date typed while it waits is an answer of its own, so Start goes with what the field shows
        // once nothing is coming for it. CanStart is read AFTER the wait: a scenario that gave no date
        // refuses the start, it does not start the session with the old date (R4-Z2).
        //
        // A wait that runs out refuses too, and says so: going ahead would start with the date from before
        // the request - the very fault the wait is for - and silently. The answer re-enables Start when it
        // lands (the handler beside _moments in the constructor).
        if (!await _moments.WhenCurrentAsync() && !_moments.IsCurrent)
        {
            _momentPending = true;
            RaisePropertyChanged(nameof(CanStart));
            RaisePropertyChanged(nameof(HasContract));
            RaiseStartRefusalChanged();
            return;
        }

        // A window closing while this waited starts nothing (R4/13): it would launch the application after
        // the window that was to show it had gone.
        if (!CanStart || _closing)
        {
            return;
        }

        _run = RunSessionAsync();
        await _run;
    }

    /// <summary>The session itself, from the moment Start is committed until the window is usable again -
    /// its task is what a closing window waits for (<see cref="FinishForCloseAsync"/>).</summary>
    private Task? _run;

    /// <summary>Set once the window has begun to close. Checked before each step that would take the session
    /// further, because a launch after that point would be an application nobody asked for any more.</summary>
    private bool _closing;

    private async Task RunSessionAsync()
    {
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
            var setup = LiveSetup;
            _started = setup;
            RaisePropertyChanged(nameof(StartedAtPreview));

            // Build the plan by reading the target's PE header. Classify a TARGET problem here (RELEASE-007)
            // so it is not reported as a broken core install: a non-PE file yields InvalidOperationException
            // ("cannot determine the bitness"), while a missing or unreadable file propagates from PeReader
            // as an IO/access error (it does not swallow those - only a malformed PE becomes Unknown).
            //
            // Built OFF the UI thread (R4-N48): it reads the target's header twice and asks three times whether
            // a file beside it exists, and on a share that has gone each of those blocks for as long as the
            // share takes to fail - measured 21 s for one. Its inputs are the setup taken above, on this thread,
            // and a record nothing can change under the pool thread that reads it.
            SessionPlan plan;
            try
            {
                plan = await Task.Run(setup.Plan).ConfigureAwait(true);
            }
            catch (InvalidOperationException ex)
            {
                NoteFault(ex);
                SetStatus("status.target_unsupported", SessionStatusKind.Error);
                return;
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                NoteFault(ex);
                SetStatus("status.target_unreadable", SessionStatusKind.Error);
                return;
            }

            IsCdp = plan.IsCdp;
            _result.CorePath = plan.CorePath;

            // The handshake and its two refusals (no ready, or a gate that says no) come back as ONE
            // translation key, because that is all the difference amounts to here - both refuse before the
            // target is ever launched (docs/08 section 3, zasady/13 section 11). The session comes back
            // either way so the finally below still disposes the core and still captures its stderr.
            var open = await CoreSession.OpenAsync(plan, ReadyTimeout, _connect);
            session = open.Session;
            _session = session;
            _result.HeardFrom(session);
            if (open.RefusalKey is not null)
            {
                SetStatus(open.RefusalKey, SessionStatusKind.Error);
                return;
            }

            // 🔴 The last point before the application starts, and the one a window closed while the plan was
            // built or the core was shaking hands reaches (R4/13): without this the core would launch the
            // target after the window was gone. One check here rather than one per await, because this is
            // the step that matters - the finally below lets the core go, and nothing ran, so nothing is
            // recorded.
            if (_closing)
            {
                return;
            }

            session.Send(plan.Start);
            _launched = true; // the target is now running - this session will be recorded in history on exit
            SetStatus("status.running", SessionStatusKind.Running);

            OnStreamEnded(await CoreSession.PumpAsync(session.Events, Apply, IdleTimeout), _stopRequested);
        }
        catch (Exception ex) when (ex is FileNotFoundException or InvalidOperationException
                                       or UnauthorizedAccessException
                                       or System.ComponentModel.Win32Exception)
        {
            // The core itself could not be started: its executable is missing or would not spawn (a broken
            // or incomplete install). Target-file problems were already classified above and returned, so
            // reaching here means the core, not the target (RELEASE-007).
            NoteFault(ex);
            SetStatus("status.core_missing", SessionStatusKind.Error);
        }
        catch (IOException ex)
        {
            // The protocol pipe broke mid-session.
            NoteFault(ex);
            SetStatus("status.error", SessionStatusKind.Error);
        }
        catch (Exception ex)
        {
            // Anything else - most likely a malformed core event dereferenced in Apply (M-11). Surface it
            // as an honest error rather than let it escape `async void OnStartClick` and crash the UI thread.
            NoteFault(ex);
            SetStatus("status.error", SessionStatusKind.Error);
        }
        finally
        {
            // Nested, so nothing that fails on the way out can skip what makes the window usable again: an
            // exception out of recording the session used to leave the core undisposed and the window never
            // idle, with Start disabled for good and a hung core still running (R4-S27).
            try
            {
                if (_launched)
                {
                    // A session actually ran (the target launched) - record it with its final verdict.
                    await RecordSessionAsync();
                }
            }
            finally
            {
                try
                {
                    await CloseSessionAsync(session);
                }
                finally
                {
                    Idle = true;
                }
            }
        }
    }

    /// <summary>Dispose the core and capture its diagnostics, the part of the way out of a session that has to
    /// happen whatever else failed.</summary>
    private async Task CloseSessionAsync(CoreSession? session)
    {
        if (session is null)
        {
            // No core ran - it would not start, or the application could not be read - so there is nothing of
            // its own to drain. That is precisely the session someone asks for help with, and it used to leave
            // no block at all (R5/6b, D2): the block still says which core was asked for and what failed.
            CaptureDiagnostics([]);
            return;
        }

        // Dispose blocks briefly (it waits for the core to exit) and keeps that wait off the UI
        // thread itself - see CoreSession.DisposeAsync.
        await session.DisposeAsync();

        // Now that dispose has drained the core's stderr, capture diagnostics for support if the
        // session was anything but a clean success (RELEASE-012). A clean works session captures
        // nothing. (On a Stop the core was already disposed off-thread, so this is best-effort -
        // but a stopped healthy session has no error stderr to lose.)
        CaptureDiagnostics(session.Diagnostics, session.DiagnosticsDropped, session.CoreExitCode);

        if (ReferenceEquals(_session, session))
        {
            _session = null;
        }
    }

    /// <summary>
    /// Decide the final status once the event stream has ended, by WHY it ended (M-10). Its own method, with
    /// the Stop passed in rather than read, so the endings a live core produces can be reached without one -
    /// the result phase is drawn from them, and a Stop the core did or did not close is tested (R4-W3).
    /// </summary>
    internal void OnStreamEnded(bool watchdogFired, bool stopRequested)
    {
        if (stopRequested)
        {
            // The user pressed Stop. Show that plainly even if the core managed a clean `ended` first -
            // the verdict and coverage already captured still stand and are shown separately.
            SetStatus("status.stopped", SessionStatusKind.Stopped);
        }
        else if (!SessionResult.IsTerminal(StatusKind))
        {
            // No terminal event arrived. Either the idle watchdog fired (the core stopped heartbeating)
            // or the core just closed its stdout on its own (docs/08 section 7). Either way the finally
            // stops the core, the hook self-detaches, and the target returns to real time - do not hang.
            SetStatus(
                watchdogFired ? "status.core_unresponsive" : "status.core_stopped",
                watchdogFired ? SessionStatusKind.CoreUnresponsive : SessionStatusKind.CoreStopped);
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

    /// <summary>
    /// The window is closing: end the session the way Stop does and wait for it to be over - recorded in the
    /// history, its diagnostics captured, the core let go - but never longer than <paramref name="limit"/>,
    /// because a window must always close. True when there was nothing to wait for or it ended in time.
    /// <para>
    /// 🔴 This replaced a dispose the window BLOCKED on, three seconds on the UI thread (R4-S24). The end of a
    /// session runs on that very thread, so the wait always ran out, and the application then exited before
    /// the session was recorded: measured, every close during a session took 3.05 s and recorded nothing.
    /// Nothing blocks here, so the end runs while the window waits - showing "Stopping" as a Stop does.
    /// </para>
    /// <para>
    /// A session still connecting is not stopped, it is not started: <see cref="RunSessionAsync"/> checks
    /// <see cref="_closing"/> before it launches anything. A Start still waiting for its date never commits
    /// at all, so it has no run to wait for.
    /// </para>
    /// </summary>
    public async Task<bool> FinishForCloseAsync(TimeSpan limit)
    {
        _closing = true;
        _moments.Abandon();
        var run = _run;
        if (run is null || run.IsCompleted)
        {
            return true;
        }

        RequestStop(); // a no-op unless the session is running - one still connecting stops itself above
        // A race with Task.Delay rather than WaitAsync: its TimeoutException would be one type too many for
        // this class's coupling ceiling, and the delay outliving a run that won lives only until the process
        // exits, which is what a closing window does next.
        return await Task.WhenAny(run, Task.Delay(limit)).ConfigureAwait(true) == run;
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
        _result.ParentPid = null;
        IsCdp = false;
        Covered = [];
        _result.ParentCovered = [];
        _result.PageCovered = [];
        Observed = [];
        Uncovered = [];
        // 🔴 Unobserved was missing from this list, so a second session in the same window kept listing the
        // first session's channels under "Could not be watched" - both lists are unions, and a union with the
        // previous session is the audit describing a session that is not this one (untouchable rule 4).
        Unobserved = [];
        InstalledLate = [];
        Warnings = [];
        UncoveredChildren = [];
        UncoveredChildrenTotal = 0;
        Engines = [];
        EnginePagesReached = false;

        _result.HasTiming = false;
        _result.EndedSeen = false;
        _result.ElapsedRealMs = 0;
        _result.ElapsedFakeMs = 0;
        // 🔴 Cleared, not zeroed on screen: "0:00:00" beside a clock is a measurement, and a session that
        // has not reported yet has not measured anything. The tile drops the line instead.
        Fake.Elapsed = string.Empty;
        Real.Elapsed = string.Empty;
        _result.FakeEndWall = string.Empty;
        _result.VanishReasonKey = string.Empty;
        _result.LivedMs = 0;
        TargetExitCode = null;
        FollowedRows = [];
        CopiesOutsideRows = [];
        LeftRunningRows = [];
        RefusalIncomplete = false;
        ResidueKeys = [];
        _result.ForgetReportFacts();
        RaisePropertyChanged(nameof(HostSlept));
        RaisePropertyChanged(nameof(HasHostSlept));
        DiagnosticsText = string.Empty;
        DiagnosticsSavedPath = string.Empty;
        InFlightErrorKey = string.Empty;
        // A note about what the last LOAD could not fill belongs to the form the user is about to run, not
        // to the run itself - once they start, they have accepted the form as it stands.
        HistoryNoteKey = string.Empty;
        TargetNoteKey = string.Empty;
        // Said about the session that moved the old history, not about the next one.
        HistorySetAside = string.Empty;
        // The jump target belongs to the session it was typed in (R5-P11). It is read in the zone of the
        // session it is sent to, so a moment left from the last run would jump the next one somewhere nobody
        // asked for. Cleared to the untouched state - an empty field typed out says "the date is empty", and a
        // new session's field must not open on an error. The field is on the session screen only, so at Start
        // nothing typed for this session is lost.
        JumpMoment.Clear();
    }

    /// <summary>The setup the form holds right now - what Start takes as the session's setup.</summary>
    internal SessionSetup LiveSetup => new(
        _targetPath ?? string.Empty,
        Moment.Canonical,
        SelectedZone,
        SelectedMode,
        _targetArgs,
        _workingFolder,
        _scaleDuration,
        _scaleQpc,
        _keepBackgroundTimers,
        _forceStart,
        _reachEmbedded,
        WantsElevatedEmbedded);

    /// <summary>The setup the session STARTED with, falling back to the live form when no session has
    /// started (a fresh window, a unit test). Every report reads this rather than the form, so none of them
    /// can name a target, a zone or a moment chosen after the run.</summary>
    private SessionSetup Requested => _started ?? LiveSetup;

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
        return SessionReport.Summary(_result, Requested, Fake.Wall, translate);
    }

    /// <summary>
    /// Capture the core's diagnostics for support when a session ended in anything but a clean success
    /// (RELEASE-012): compose the block, expose it for the Copy-diagnostics button, and write it to a log
    /// file beside the exe. A clean works session leaves nothing, so the happy path stays quiet. Called from
    /// the StartAsync finally AFTER the client is disposed, so the core's stderr has been fully drained.
    /// Internal so a unit test can drive it with a fake line list and a fake log (no core process).
    /// </summary>
    internal void CaptureDiagnostics(IEnumerable<string> lines, int dropped = 0, int? coreExit = null)
    {
        if (_result.IsReliable)
        {
            return; // a clean works session needs no diagnostics - keep the button and the log out of it
        }

        var block = BuildDiagnosticsBlock(lines, dropped, coreExit);
        DiagnosticsText = block;
        // Best-effort file: a read-only medium returns null, and the in-memory copy behind the button stands.
        var path = _diagnosticsLog.Save(block);
        if (path is not null)
        {
            DiagnosticsSavedPath = path;
        }
    }

    /// <summary>A failure the session loop caught: its message for the screen, and the whole exception for the
    /// diagnostics block (R5/6b, D2) - the message alone reached support with no type and no stack.</summary>
    private void NoteFault(Exception fault)
    {
        LastError = fault.Message;
        _result.Fault = fault;
    }

    /// <summary>Compose the diagnostics block: a header naming the status, target, and requested moment, then
    /// the core's stderr and parse-error lines verbatim. English and stable, like the core's own stderr and
    /// the CLI report - it is a technical artifact for a bug report, not interface text (rule 15 governs the
    /// UI - this is data). Pure over the view state, so it is unit tested with a fake line list.</summary>
    internal string BuildDiagnosticsBlock(IEnumerable<string> lines, int dropped = 0, int? coreExit = null)
        => SessionReport.Diagnostics(_result, Requested, lines, dropped, coreExit, DateTime.UtcNow);

    /// <summary>Build a history record from the START setup and the session's final verdict (docs/04
    /// section 6). Written from the Start finally, BEFORE the form unlocks, so reading live would be correct
    /// here today - it reads the start setup anyway, so the record does not depend on WHEN it happens to be
    /// built. The GUI's own clock is real (only the target is faked), so DateTime.UtcNow is the true end
    /// time.</summary>
    internal SessionRecord BuildRecord() => SessionReport.Record(_result, Requested, DateTime.UtcNow);

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

        HistoryError = await _store.AppendAsync(record).ConfigureAwait(true);
        HistorySetAside = _store.TakeSetAside();
    }

    /// <summary>Remove one past session from the panel and the store. Mild and left un-confirmed (zasady/13
    /// section 11) - it is a log entry, and a re-run re-creates one. The row leaves the panel at once, and
    /// the store follows in the background (R4/13).</summary>
    public async Task RemoveFromHistoryAsync(SessionRecord record)
    {
        ArgumentNullException.ThrowIfNull(record);
        History.Remove(record);
        HistoryError = await _store.RemoveAsync(record).ConfigureAwait(true);
    }

    /// <summary>Remove every past session from the panel and the store. The view confirms first (zasady/13
    /// section 11) - this method just performs it.</summary>
    public async Task ClearHistoryAsync()
    {
        History.Clear();
        HistoryError = await _store.ClearAsync().ConfigureAwait(true);
        // A clear that found the file unreadable moves it aside as an append does, and says so here - read
        // only by the next record, the move was silent now and claimed by a record that moved nothing.
        HistorySetAside = _store.TakeSetAside();
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

        // 🔴 Leave the result FIRST. The history well lives on the result screen, so this used to fill the
        // setup form behind that screen: every field changed and nothing on the display did, which read as
        // a button that does nothing. A no-op on a fresh form, and it keeps the history (rule 7 - nothing
        // starts, the filled form waits for Start).
        BeginNewSession();

        SetTarget(record.TargetPath);
        Moment.LoadCanonical(record.MomentLocal);
        TargetArgs = record.TargetArgs;
        WorkingFolder = record.WorkingFolder;
        ScaleDuration = record.ScaleDuration;
        ScaleQpc = record.ScaleQpc;
        KeepBackgroundTimers = record.KeepBackgroundTimers;
        ForceStart = record.Force;
        ReachEmbedded = record.Embedded;
        // What a window that is not elevated cannot do is not loaded as ticked: the box would claim it.
        ReachElevatedEmbedded = record.ElevatedEmbedded && _canReachElevated;

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
        !SessionResult.IsTerminal(kind) && kind != SessionStatusKind.Stopping;

    /// <summary>True when an error event answers one of OUR in-flight commands (jump/set_multiplier),
    /// whose ids run from <see cref="FirstInFlightCommandId"/> up. A start/fatal error instead carries the
    /// start command's id (1) or none, so it is never treated as an in-flight rejection (RELEASE-001).</summary>
    private static bool IsInFlightError(ErrorEvent err) => err.Id is >= FirstInFlightCommandId;

    /// <summary>
    /// Put the two durations on their own clocks, and say the session has timing the first time it does.
    /// </summary>
    /// <remarks>
    /// One place rather than three: the timing arrives from a heartbeat and again from the core's
    /// authoritative end report, and a third site resets it. Formatting it at each of them would be three
    /// chances for the fake clock's duration and the real one's to end up written differently.
    /// </remarks>
    private void PublishElapsed()
    {
        Fake.Elapsed = ClockView.FormatDuration(_result.ElapsedFakeMs);
        Real.Elapsed = ClockView.FormatDuration(_result.ElapsedRealMs);
        // Where the fake clock got to moves with every heartbeat.
        RaisePropertyChanged(nameof(FakeEndPreview));
        if (_result.HasTiming)
        {
            return;
        }

        // The rest follows the first timing only, so it is said once rather than on every heartbeat (R5-WY4):
        // the facts block of the result appears with it, and an error before it is "did not start" while
        // after it the verdict stands.
        _result.HasTiming = true;
        RaisePropertyChanged(nameof(HasTiming));
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

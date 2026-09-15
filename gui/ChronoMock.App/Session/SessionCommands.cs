using System.ComponentModel;
using System.Windows.Input;

namespace ChronoMock.App;

/// <summary>
/// The substitution panel's actions, lifted off <see cref="SessionViewModel"/> so the view model stays a
/// holder of state and these stay a holder of behaviour (GUI rule 15), and so the rebuilt phase views can
/// bind <c>Command</c> instead of carrying a Click handler each - the released panel wrote 29 handlers into
/// one screen, which is what made it impossible to rearrange.
/// <para>
/// Two kinds live here. The view-model-only actions need nothing but the view model. The window-dependent
/// ones (the pickers, the clipboard, the confirm dialog) reach the window through an
/// <see cref="IShellInteraction"/> attached by the window with <see cref="AttachShell"/> - deliberately NOT
/// passed through the view model's constructor, which would couple every caller of that constructor (the
/// tests among them, one of which sits on the class-coupling ceiling) to a UI type the view model must not
/// know (rule 16). When no shell is attached - a phase rendered for the state sheet, or a view model built in
/// a test - the window commands' CanExecute still follows the data gate so the drawing matches the shipped
/// panel, and their work is a quiet no-op. Support and About are NOT here: they live on the window's title
/// bar, which is shell chrome the phases never show.
/// </para>
/// <para>
/// Every command re-queries its CanExecute whenever the view model raises PropertyChanged. That is cheap and
/// rare here: the two clocks are separate ClockView objects, so their ~1 s ticks do not travel through the
/// view model's PropertyChanged - only genuine state changes (status, target, the moment-driven CanStart,
/// the chosen history row, the history count) do. A ClockView tick never refreshes a command.
/// </para>
/// </summary>
public sealed class SessionCommands
{
    private readonly RelayCommand _stop;
    private readonly RelayCommand<long> _speed;
    private readonly RelayCommand<string> _jump;
    private readonly RelayCommand<string> _setCustomSpeed;
    private readonly RelayCommand _today;
    private readonly RelayCommand _now;
    private readonly RelayCommand _repeat;
    private readonly RelayCommand _forget;
    private readonly RelayCommand _newSession;
    private readonly RelayCommand _chooseTarget;
    private readonly RelayCommand _browseFolder;
    private readonly RelayCommand _copySummary;
    private readonly RelayCommand _copyDiagnostics;
    private readonly RelayCommand _clearHistory;
    private readonly RelayCommand _chooseFirstScenario;
    private readonly RelayCommand _jumpToEntered;
    private readonly AsyncRelayCommand _start;
    private readonly AsyncRelayCommand _relativeApply;
    private readonly AsyncRelayCommand _refreshRecentTargets;
    private IShellInteraction? _shell;

    internal SessionCommands(SessionViewModel session)
    {
        _start = new AsyncRelayCommand(session.StartAsync, () => session.CanStart);
        _stop = new RelayCommand(session.RequestStop, () => session.IsRunning);
        _speed = new RelayCommand<long>(session.SendMultiplier, _ => session.IsRunning);
        _jump = new RelayCommand<string>(session.SendJump, _ => session.IsRunning);
        _setCustomSpeed = new RelayCommand<string>(session.SetCustomSpeed, _ => session.IsRunning);
        _relativeApply = new AsyncRelayCommand(session.Relative.ApplyAsync, () => session.CanEditTime);

        // Re-check which recent targets still exist on disk as the list opens, so a wiped build output is
        // marked rather than silently offered (rule 6). Runs off the UI thread inside the view model - a dead
        // network path would otherwise freeze the window for as long as the share takes to fail. Idle only:
        // the list is start-only, and the box is disabled once a session is under way.
        _refreshRecentTargets = new AsyncRelayCommand(session.RefreshRecentTargetsAsync, () => session.IsIdle);

        // Today and Now fill the At field in the SESSION zone (rule 2), never the OS clock - the selected
        // zone's bias is passed so the moment control stays zone-agnostic. Editable idle or while running.
        _today = new RelayCommand(
            () => session.Moment.SetToday(session.SelectedZone.BiasMinutes), () => session.CanEditTime);
        _now = new RelayCommand(
            () => session.Moment.SetNow(session.SelectedZone.BiasMinutes), () => session.CanEditTime);

        // Enter in the scenario filter picks the first hit - the keyboard's version of clicking the top row.
        // Editable-only like the moment fillers above, since choosing a scenario is what fills the At field.
        _chooseFirstScenario = new RelayCommand(session.ChooseFirstScenario, () => session.CanEditTime);

        // Jump the running clock to the moment typed in the session's own field. Live only, and only once
        // that field holds a valid moment - a jump to a malformed date is a no-op the button refuses rather
        // than a silent failure. JumpMoment raises its OWN PropertyChanged as it is typed, which does not
        // travel through the view model's, so the button's CanExecute is refreshed from the field here.
        _jumpToEntered = new RelayCommand(
            session.JumpToEnteredMoment, () => session.IsRunning && session.JumpMoment.IsValid);
        session.JumpMoment.PropertyChanged += (_, _) => _jumpToEntered.RaiseCanExecuteChanged();

        // Repeat and Forget act on the row chosen in the history well (the list sets SelectedRecord), so both
        // are enabled only with a chosen row. Repeat fills the setup form and starts nothing (rule 7).
        _repeat = new RelayCommand(
            () =>
            {
                if (session.SelectedRecord is { } record)
                {
                    session.LoadFromHistory(record);
                }
            },
            () => session.HasSelectedRecord);
        _forget = new RelayCommand(
            () =>
            {
                if (session.SelectedRecord is { } record)
                {
                    session.RemoveFromHistory(record);
                }
            },
            () => session.HasSelectedRecord);

        // New session returns a finished result to a fresh setup form (the result phase's one step forward),
        // so it only makes sense once the session has ended.
        _newSession = new RelayCommand(session.BeginNewSession, () => session.CanBeginNewSession);

        // Window-dependent actions. CanExecute follows the data gate alone - the shell is checked in Execute,
        // so a shell-less render draws them exactly as the shipped panel would and a shell-less Execute is a
        // no-op rather than a crash. They read _shell late, so AttachShell before the first click is enough.
        _chooseTarget = new RelayCommand(
            () =>
            {
                if (_shell?.PickExecutable() is { } path)
                {
                    session.SetTarget(path);
                }
            },
            () => session.IsIdle);
        _browseFolder = new RelayCommand(
            () =>
            {
                if (_shell?.PickFolder(session.WorkingFolder) is { } folder)
                {
                    session.WorkingFolder = folder;
                }
            },
            () => session.IsIdle);
        _copySummary = new RelayCommand(
            () =>
            {
                if (_shell is not null)
                {
                    session.NoteCopy(_shell.CopyToClipboard(session.BuildSummary(_shell.Text)));
                }
            },
            () => session.CanCopySummary);
        _copyDiagnostics = new RelayCommand(
            () =>
            {
                if (_shell is not null)
                {
                    session.NoteCopy(_shell.CopyToClipboard(session.DiagnosticsText));
                }
            },
            () => session.HasDiagnostics);
        _clearHistory = new RelayCommand(
            () =>
            {
                // Destructive, so it confirms first with the effect spelled out and the affirmative named
                // after what it does (zasady/13 section 11).
                if (_shell is not null
                    && _shell.Confirm("history.clear_confirm", "history.clear_undone", "history.clear_title"))
                {
                    session.ClearHistory();
                }
            },
            () => session.HasHistory);

        session.PropertyChanged += OnSessionChanged;
    }

    /// <summary>Give the commands a window to open pickers and dialogs against. Called once by the window that
    /// hosts the phases, before it is shown - the view model never sees this (rule 16).</summary>
    public void AttachShell(IShellInteraction shell) => _shell = shell;

    /// <summary>Start the session (Start), or do nothing while one is already starting (async, no double run).</summary>
    public ICommand Start => _start;

    /// <summary>End the running session cleanly - the target reverts to real time and is never killed (M-10).</summary>
    public ICommand Stop => _stop;

    /// <summary>Set the in-flight multiplier - 0 freezes. Parameter is the multiplier.</summary>
    public ICommand Speed => _speed;

    /// <summary>Jump the fake wall by a relative delta in flight (e.g. "+1d", "-1h"). Parameter is the delta.</summary>
    public ICommand Jump => _jump;

    /// <summary>Apply an arbitrary in-flight speed typed as "N" or "xN". Parameter is the raw text.</summary>
    public ICommand SetCustomSpeed => _setCustomSpeed;

    /// <summary>"Now, shifted" - fill the At field with a moment relative to now (the calculator's shift step).</summary>
    public ICommand RelativeApply => _relativeApply;

    /// <summary>Re-check which recent targets still exist, bound to the recent list opening (setup only).</summary>
    public ICommand RefreshRecentTargets => _refreshRecentTargets;

    /// <summary>Fill the At field with today at midnight, in the session zone.</summary>
    public ICommand Today => _today;

    /// <summary>Fill the At field with the current instant, in the session zone.</summary>
    public ICommand Now => _now;

    /// <summary>Pick the first scenario the filter left standing - the SearchBox binds this to Enter.</summary>
    public ICommand ChooseFirstScenario => _chooseFirstScenario;

    /// <summary>Jump the running clock to the moment typed in the session's jump field. Live only.</summary>
    public ICommand JumpToEntered => _jumpToEntered;

    /// <summary>Fill the setup form from the chosen history row. Never starts a session (rule 7).</summary>
    public ICommand Repeat => _repeat;

    /// <summary>Delete the chosen history row.</summary>
    public ICommand Forget => _forget;

    /// <summary>Return a finished result to a fresh setup form. Enabled only once the session has ended.</summary>
    public ICommand NewSession => _newSession;

    /// <summary>Pick an executable to run (a file picker), then fill the target. Never starts a session (rule 7).</summary>
    public ICommand ChooseTarget => _chooseTarget;

    /// <summary>Pick the working folder the target starts in (a folder picker).</summary>
    public ICommand BrowseFolder => _browseFolder;

    /// <summary>Copy the paste-into-ticket session summary to the clipboard.</summary>
    public ICommand CopySummary => _copySummary;

    /// <summary>Copy the diagnostics block (the core's stderr and parse errors) to the clipboard.</summary>
    public ICommand CopyDiagnostics => _copyDiagnostics;

    /// <summary>Clear the whole history, after confirming - a re-run re-creates one.</summary>
    public ICommand ClearHistory => _clearHistory;

    private void OnSessionChanged(object? sender, PropertyChangedEventArgs e)
    {
        // A genuine state change may flip any of these gates, and telling exactly which from the property
        // name would be a second copy of each CanExecute's dependency list - one that goes stale silently.
        // Re-querying all of them is correct and, because ClockView ticks do not reach here, cheap.
        _start.RaiseCanExecuteChanged();
        _stop.RaiseCanExecuteChanged();
        _speed.RaiseCanExecuteChanged();
        _jump.RaiseCanExecuteChanged();
        _setCustomSpeed.RaiseCanExecuteChanged();
        _relativeApply.RaiseCanExecuteChanged();
        _refreshRecentTargets.RaiseCanExecuteChanged();
        _today.RaiseCanExecuteChanged();
        _now.RaiseCanExecuteChanged();
        _chooseFirstScenario.RaiseCanExecuteChanged();
        _jumpToEntered.RaiseCanExecuteChanged();
        _repeat.RaiseCanExecuteChanged();
        _forget.RaiseCanExecuteChanged();
        _newSession.RaiseCanExecuteChanged();
        _chooseTarget.RaiseCanExecuteChanged();
        _browseFolder.RaiseCanExecuteChanged();
        _copySummary.RaiseCanExecuteChanged();
        _copyDiagnostics.RaiseCanExecuteChanged();
        _clearHistory.RaiseCanExecuteChanged();
    }
}

using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using Wpf.Ui.Controls;
using ChronoMock.App.Calc;

namespace ChronoMock.App;

public partial class MainWindow : FluentWindow
{
    // The panel is a second consumer of the calculator engine and of the shared preset catalogue: the
    // scenario list turns a named preset into a date (7.1 pt 2). Same layout seam as everything else,
    // resolved in AppPaths.
    private readonly SessionViewModel _session = new(
        FileSessionHistoryStore.ForApp(), FileDiagnosticsLog.ForApp(), AppPaths.CalcClient, AppPaths.PresetsDir);
    private readonly CalculatorViewModel _calculator = CreateCalculator();


    // The calculator is a client of the same engine (ADR-6) - it reads the shared preset catalogue and the
    // calendars from the portable install beside the exe, or from the cargo outputs in a dev checkout - the
    // layout seam lives in AppPaths, not here.
    private static CalculatorViewModel CreateCalculator()
        => new(AppPaths.CalcClient, AppPaths.PresetsDir);

    public MainWindow()
    {
        InitializeComponent();
        DataContext = _session;
        CalculatorContainer.DataContext = _calculator;

        // Hand the session's window-dependent commands (the pickers, the clipboard, the confirm dialog) a
        // window to act against. This is the ONE place a UI type meets those commands: the view model never
        // holds it (GUI rules 15 and 16), and a command whose shell is not attached is a quiet no-op rather
        // than a crash, which is how a phase drawn for the state sheet stays harmless.
        _session.Commands.AttachShell(new WindowShellInteraction(this));

        // Bridge: the calculator asks to send its result to substitution - this window fills the panel.
        _calculator.UseInSubstitutionRequested += OnUseInSubstitution;

        // Dev convenience: pre-select the bundled sample target so the panel is usable at once. The user
        // can pick any executable instead - this default is dev scaffolding (DemoSession.DefaultTargetPath).
        var sample = SessionPlan.DefaultTargetPath();
        if (sample is not null)
        {
            _session.SetTarget(sample);
        }

        // Closing the window ends the session: disposing the client stops the core, and the hook
        // self-detaches so the target reverts to real time on its own (slice 10) - we never kill it.
        // Done in Closing, with a bounded wait, rather than as an async-void handler on Closed: WPF does
        // not await such a handler, so the process could exit mid-shutdown. It survived on the core seeing
        // EOF on stdin and cleaning up by itself, which is luck, not a design. The wait is bounded because
        // a window must always close - a core that will not end is killed by the dispose path anyway.
        Closing += OnWindowClosing;
    }

    private void OnWindowClosing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        Closing -= OnWindowClosing;
        try
        {
            _session.DisposeAsync().AsTask().Wait(TimeSpan.FromSeconds(3));
        }
        catch (AggregateException)
        {
            // The core was already gone, or refused to end - either way the window still closes and the
            // hook self-detaches, so the target is not left on a fake clock.
        }
    }

    // Swap the visible module: substitution (the three phases) vs calculator view. The default radio's
    // Checked fires during InitializeComponent, before the content elements exist, so both are null-checked
    // here. The substitution container holds the phases - which phase shows inside it is the view model's
    // ShowsXPhase decision, not this switch's.
    private void OnModeChanged(object sender, RoutedEventArgs e)
    {
        if (SubstitutionContainer is null || CalculatorContainer is null)
        {
            return;
        }

        bool calculator = ModeCalculator.IsChecked == true;
        SubstitutionContainer.Visibility = calculator ? Visibility.Collapsed : Visibility.Visible;
        CalculatorContainer.Visibility = calculator ? Visibility.Visible : Visibility.Collapsed;
        if (calculator)
        {
            // Compute on first reveal (not at construction, so building the window in a test spawns nothing).
            _ = _calculator.EnsureComputedAsync();
        }
    }

    // Bridge from the calculator (chrono-mock 6.3): fill the substitution setup with the moment and its
    // zone (rule 2 - the moment travels with its zone, never a bare date), then show the substitution
    // module. Only when idle - a running session's moment is left alone.
    private void OnUseInSubstitution(string momentLocal, int zoneBias)
    {
        if (_session.IsIdle)
        {
            _session.Moment.LoadCanonical(momentLocal);
            _session.SelectedZone =
                _session.Zones.FirstOrDefault(z => z.BiasMinutes == zoneBias) ?? _session.SelectedZone;
        }

        ModeSubstitution.IsChecked = true; // OnModeChanged swaps the visible module
    }

    // Dropping an application on the window (chrono-mock 7.1 pt 1). Only fills the target - it never
    // starts a session (rule 7), exactly like picking one from the dialog or the recent list.
    //
    // The window accepts the drop, not one small panel: aiming at a strip is the part people miss, and
    // the whole window is the obvious target for "run this app". Refused while a session runs, because
    // the target is start-only and a silently ignored drop would look like the drop failed.
    private void OnWindowDragOver(object sender, DragEventArgs e)
    {
        e.Effects = DroppedExecutable(e) is null ? DragDropEffects.None : DragDropEffects.Copy;
        e.Handled = true;
    }

    private void OnWindowDrop(object sender, DragEventArgs e)
    {
        e.Handled = true;
        if (!_session.IsIdle)
        {
            return;
        }

        if (DroppedExecutable(e) is { } path)
        {
            _session.SetTarget(path);
            return;
        }

        // Say why nothing happened rather than swallowing the drop (rule 6). A dropped .lnk or .bat could
        // only fail later, and failing at the drop is the honest place for it.
        if (e.Data.GetDataPresent(DataFormats.FileDrop))
        {
            Views.MessageDialog.Tell(this, Text("target.drop_rejected_title"), Text("target.drop_rejected"));
        }
    }

    /// <summary>The single .exe in a drop, or null - several files, a folder or another format is not a target.</summary>
    private string? DroppedExecutable(DragEventArgs e)
    {
        if (!_session.IsIdle || !e.Data.GetDataPresent(DataFormats.FileDrop))
        {
            return null;
        }

        return e.Data.GetData(DataFormats.FileDrop) is string[] { Length: 1 } paths
            && paths[0].EndsWith(".exe", StringComparison.OrdinalIgnoreCase)
            && File.Exists(paths[0])
                ? paths[0]
                : null;
    }

    // The support link. The destination is chosen inside ExternalLinks and never here, so this method is
    // not a place where a string could turn into something the shell runs.
    //
    // A machine with nothing associated with https is rare and real, and on one the shell simply refuses.
    // Refusing quietly would leave a button that does nothing at all, so the failure is said out loud and
    // the address goes into the message beneath it - a notice with no way out is half an answer, and the
    // way out here is being able to type the address in by hand.
    private void OnSupportClick(object sender, RoutedEventArgs e)
    {
        if (!ExternalLinks.TryOpenSupport())
        {
            Views.MessageDialog.Tell(
                this,
                Text("support.failed_title"),
                Text("support.failed") + Environment.NewLine + Environment.NewLine + ExternalLinks.Support);
        }
    }

    // The About window, which is where this application states its licence, its version and what somebody
    // else wrote inside it. The command line has answered that since it existed (`chrono license`), and
    // until now the window had no answer at all - the asymmetry, not a licence requirement, is the reason
    // it is here.
    private void OnAboutClick(object sender, RoutedEventArgs e)
        => Views.AboutDialog.Show(this, AppPaths.LicenceClient);

    // Resolve a translation key to text for a native dialog (rule 15) - falls back to the raw key if missing.
    private static string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;
}

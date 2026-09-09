using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Runtime.InteropServices;
using System.Windows;
using Microsoft.Win32;
using Wpf.Ui.Controls;
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

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
        // The relative line's own view model, which lives on the panel (SessionViewModel.Relative) - this
        // only points the row at it, the same shape as the line above.
        RelativeMomentRow.DataContext = _session.Relative;

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
        // self-detaches so the target reverts to real time on its own (plasterek 10) - we never kill it.
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

    // Swap the visible module: substitution panel vs calculator view. The default radio's Checked fires
    // during InitializeComponent, before the content elements exist, so both are null-checked here.
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

    private void OnChooseTargetClick(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFileDialog
        {
            Title = Text("target.dialog_title"),
            Filter = Text("target.dialog_filter"),
            CheckFileExists = true,
        };

        if (dialog.ShowDialog(this) == true)
        {
            _session.SetTarget(dialog.FileName);
        }
    }

    // Opening the recent list re-checks which of its targets still exist, so a wiped build output is
    // marked instead of silently offered (rule 6). The check itself runs off the UI thread - a dead
    // network path would otherwise freeze the window for as long as the share takes to fail.
    private async void OnRecentTargetsOpened(object sender, EventArgs e)
        => await _session.RefreshRecentTargetsAsync();

    // The folder the target starts in (chrono-mock 7.1 pt 1). Seeded with whatever is already typed so
    // browsing from a filled field starts where the tester was, not at the shell root.
    private void OnBrowseFolderClick(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFolderDialog { Title = Text("launch.cwd_label") };
        if (!string.IsNullOrWhiteSpace(_session.WorkingFolder) && Directory.Exists(_session.WorkingFolder))
        {
            dialog.InitialDirectory = _session.WorkingFolder;
        }

        if (dialog.ShowDialog(this) == true)
        {
            _session.WorkingFolder = dialog.FolderName;
        }
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

    // The panel's one action, which is Start or Stop depending on the session (M-10). It reads the state
    // rather than trusting the label, so a click that arrives just as the session changed does the thing
    // the session is actually in - stopping is what a RUNNING session does, starting is what any other
    // does, and neither is decided by what the button happened to say when it was drawn.
    //
    // Stopping is what frees the tester from closing the whole window when they are done, or when the core
    // stops responding. The target reverts to real time, and the app under test is never killed.
    private async void OnPrimaryActionClick(object sender, RoutedEventArgs e)
    {
        if (_session.IsRunning)
        {
            _session.RequestStop();
            return;
        }

        await _session.StartAsync();
    }

    // In-flight speed control: each button carries its multiplier in Tag ("0" = freeze).
    private void OnSpeedClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string tag }
            && long.TryParse(tag, NumberStyles.Integer, CultureInfo.InvariantCulture, out var multiplier))
        {
            _session.SendMultiplier(multiplier);
        }
    }

    // In-flight jump: each button carries its relative delta in Tag (e.g. "+1d", "-1h").
    private void OnJumpClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string delta } && delta.Length > 0)
        {
            _session.SendJump(delta);
        }
    }

    // In-flight jump to the ABSOLUTE moment currently in the At field (in the session zone).
    private void OnJumpToClick(object sender, RoutedEventArgs e) => _session.JumpToEnteredMoment();

    // Quick-fill the At field with today (midnight) or now, in the SESSION zone (rule 2) - the selected
    // zone's bias, not the OS local time. Works idle (sets the start moment) and while running (the user
    // then presses Jump to).
    private void OnTodayClick(object sender, RoutedEventArgs e)
        => _session.Moment.SetToday(_session.SelectedZone.BiasMinutes);

    private void OnNowClick(object sender, RoutedEventArgs e)
        => _session.Moment.SetNow(_session.SelectedZone.BiasMinutes);

    // "Now, shifted" - the panel's equivalent of `--at +30d`. Awaited rather than fire-and-forget: the
    // engine runs out of process, and a discarded task would carry any failure away with it (the view model
    // turns every reachable one into an error key on screen, and this keeps the unreachable ones loud).
    private async void OnRelativeSetClick(object sender, RoutedEventArgs e)
        => await _session.Relative.ApplyAsync();

    // In-flight arbitrary speed: the view model parses the custom-speed box ("500" or "x500") and either
    // applies it or surfaces an in-flight error, so a bad value is not a silent no-op (rule 6, RELEASE P3).
    private void OnSetSpeedClick(object sender, RoutedEventArgs e)
        => _session.SetCustomSpeed(CustomSpeedBox.Text);

    // Copy the session summary to the clipboard (chrono-mock 7.2, 8.8). The summary is built in the UI
    // language - a clipboard held by another process is reported honestly, never swallowed (rule 6).
    private void OnCopySummaryClick(object sender, RoutedEventArgs e)
        => _session.NoteCopy(TrySetClipboard(_session.BuildSummary(Text)));

    // Copy the diagnostics block to the clipboard (RELEASE-012): the core's stderr and parse errors, so a QA
    // report has something to attach when an injection is blocked. Shown only for a non-clean session - same
    // clipboard-failure honesty as Copy summary (rule 6).
    private void OnCopyDiagnosticsClick(object sender, RoutedEventArgs e)
        => _session.NoteCopy(TrySetClipboard(_session.DiagnosticsText));

    private static bool TrySetClipboard(string text)
    {
        for (int attempt = 0; attempt < 2; attempt++)
        {
            try
            {
                Clipboard.SetText(text);
                return true;
            }
            catch (ExternalException)
            {
                // Another process holds the clipboard lock - retry once, then report the failure.
            }
        }

        return false;
    }

    // Repeat a past session: fill the setup form from the clicked record. It never starts a session
    // (untouchable rule 7, docs/04 section 6) - the user reviews the filled form and clicks Start.
    private void OnHistoryClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: SessionRecord record })
        {
            _session.LoadFromHistory(record);
        }
    }

    // Delete one past session. Mild and un-confirmed (zasady/13 section 11) - a re-run re-creates one.
    private void OnHistoryDeleteClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: SessionRecord record })
        {
            _session.RemoveFromHistory(record);
        }
    }

    // Clear all history. Destructive, so it confirms first with the effect spelled out (zasady/13 section 11)
    // and the affirmative NAMED after what it does, rather than left as a Yes that says nothing about which
    // question it is answering.
    private void OnHistoryClearClick(object sender, RoutedEventArgs e)
    {
        if (Views.MessageDialog.Ask(
                this, Text("history.clear_confirm"), Text("history.clear_undone"), Text("history.clear_title")))
        {
            _session.ClearHistory();
        }
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

using System.Globalization;
using System.Runtime.InteropServices;
using System.Windows;
using Microsoft.Win32;
using Wpf.Ui.Controls;
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App;

public partial class MainWindow : FluentWindow
{
    private readonly SessionViewModel _session = new(FileSessionHistoryStore.ForApp());
    private readonly CalculatorViewModel _calculator = CreateCalculator();

    // The calculator is a client of the same engine (ADR-6); it reads the shared preset catalogue and the
    // calendars from the portable install beside the exe, or from the cargo outputs in a dev checkout - the
    // layout seam lives in AppPaths, not here.
    private static CalculatorViewModel CreateCalculator()
        => new(AppPaths.CalcClient, AppPaths.PresetsDir);

    public MainWindow()
    {
        InitializeComponent();
        DataContext = _session;
        CalculatorContainer.DataContext = _calculator;

        // Bridge: the calculator asks to send its result to substitution; this window fills the panel.
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
        Closed += async (_, _) => await _session.DisposeAsync();
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

    private async void OnStartClick(object sender, RoutedEventArgs e) => await _session.StartAsync();

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

    // In-flight arbitrary speed: parse the custom-speed box (accepts "500" or "x500") and set it.
    private void OnSetSpeedClick(object sender, RoutedEventArgs e)
    {
        var raw = CustomSpeedBox.Text?.Trim().TrimStart('x', 'X', '×');
        if (long.TryParse(raw, NumberStyles.Integer, CultureInfo.InvariantCulture, out var multiplier)
            && multiplier >= 0)
        {
            _session.SendMultiplier(multiplier);
        }
    }

    // Stop the running session (M-10) - so the user is not forced to close the whole window when they are
    // done (or when the core stops responding). The target reverts to real time, the app is never killed.
    private void OnStopClick(object sender, RoutedEventArgs e) => _session.RequestStop();

    // Copy the session summary to the clipboard (chrono-mock 7.2, 8.8). The summary is built in the UI
    // language; a clipboard held by another process is reported honestly, never swallowed (rule 6).
    private void OnCopySummaryClick(object sender, RoutedEventArgs e)
        => _session.NoteCopy(TrySetClipboard(_session.BuildSummary(Text)));

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

    // Clear all history. Destructive, so it confirms first with the effect spelled out (zasady/13 section 11).
    private void OnHistoryClearClick(object sender, RoutedEventArgs e)
    {
        // Fully qualified: wpfui also defines a MessageBox type, so the bare names are ambiguous here.
        var confirmed = System.Windows.MessageBox.Show(
            Text("history.clear_confirm"), Text("history.clear_title"),
            System.Windows.MessageBoxButton.YesNo, System.Windows.MessageBoxImage.Warning)
            == System.Windows.MessageBoxResult.Yes;
        if (confirmed)
        {
            _session.ClearHistory();
        }
    }

    // Resolve a translation key to text for a native dialog (rule 15); falls back to the raw key if missing.
    private static string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;
}

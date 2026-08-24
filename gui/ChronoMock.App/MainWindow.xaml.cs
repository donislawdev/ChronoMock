using System.Globalization;
using System.Windows;
using Microsoft.Win32;
using Wpf.Ui.Controls;

namespace ChronoMock.App;

public partial class MainWindow : FluentWindow
{
    private readonly SessionViewModel _session = new();

    public MainWindow()
    {
        InitializeComponent();
        DataContext = _session;

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

    // Resolve a translation key to text for a native dialog (rule 15); falls back to the raw key if missing.
    private static string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;
}

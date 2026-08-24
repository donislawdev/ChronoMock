using System.Windows;
using Wpf.Ui.Controls;

namespace ChronoMock.App;

public partial class MainWindow : FluentWindow
{
    private readonly SessionViewModel _session = new();

    public MainWindow()
    {
        InitializeComponent();
        DataContext = _session;

        // Closing the window ends the session: disposing the client stops the core, and the hook
        // self-detaches so the target reverts to real time on its own (plasterek 10) - we never kill it.
        Closed += async (_, _) => await _session.DisposeAsync();
    }

    private async void OnStartClick(object sender, RoutedEventArgs e) => await _session.StartAsync();
}

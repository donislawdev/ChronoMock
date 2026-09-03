using System.Windows;
using System.Windows.Threading;
using ChronoMock.App.Localization;

namespace ChronoMock.App;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        // Last-resort net for a UI-thread exception that escaped a view model (a malformed core event,
        // an unreadable file) - shown to the user, never left to terminate the app silently (M-6, rule 6).
        DispatcherUnhandledException += OnDispatcherUnhandledException;

        try
        {
            // Merge the interface strings before the first window loads, so translation keys resolve.
            // Default culture is English; a language swap is a later slice.
            LocalizationService.Apply(this, LocalizationService.DefaultCulture);

            new MainWindow().Show();
        }
        catch (Exception ex)
        {
            // Startup failed before the window is up (a missing strings file, a missing dev artifact, an
            // unreadable history file). DispatcherUnhandledException does not cover OnStartup itself, so
            // show the reason and shut down cleanly rather than crash with a raw stack (M-6).
            System.Windows.MessageBox.Show(
                ex.Message, "Chrono Mock - startup failed",
                System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Error);
            Shutdown(1);
        }
    }

    private void OnDispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        // Surface the failure and keep the app alive - a recoverable slip (one bad event, one bad preset)
        // should not take down the whole session. Marked handled so the dispatcher does not tear down.
        //
        // Shown ONCE per run (R2-N12): a repeating exception - a binding that throws on every heartbeat -
        // used to open a modal box per occurrence, and a window the user cannot out-click is worse than the
        // fault it reports. The later ones are still handled, so the app stays up, and the first box has
        // already named the failure.
        if (!_reportedUnhandled)
        {
            _reportedUnhandled = true;
            System.Windows.MessageBox.Show(
                e.Exception.Message, "Chrono Mock",
                System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Error);
        }

        e.Handled = true;
    }

    private bool _reportedUnhandled;
}

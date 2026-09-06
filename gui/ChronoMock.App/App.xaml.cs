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
            //
            // A damaged strings file degrades the interface instead of stopping the app: the window
            // then shows raw keys, which is ugly and honest, rather than "cannot be opened", which
            // is total (R3-11). Everything that matters - the target, the verdict, the coverage - is
            // data, not translation.
            var stringsProblem = LocalizationService.ApplyOrDegrade(this, LocalizationService.DefaultCulture);

            new MainWindow().Show();

            if (stringsProblem is not null)
            {
                // Deliberately a literal, and the one place in the app where that is right: the
                // dictionary this text would be looked up in is precisely what failed to load.
                System.Windows.MessageBox.Show(
                    $"The interface strings could not be loaded, so labels show their internal keys.\n\n{stringsProblem}",
                    "Chrono Mock - interface strings",
                    System.Windows.MessageBoxButton.OK,
                    System.Windows.MessageBoxImage.Warning);
            }
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

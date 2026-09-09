using System.Windows;
using System.Windows.Threading;
using ChronoMock.App.Localization;

namespace ChronoMock.App;

public partial class App : Application
{
    // 🔴 THE THREE DIALOGS BELOW STAY NATIVE, and that is a decision rather than an oversight. Everything
    // this app asks or announces in normal operation goes through Views.MessageDialog, which is themed and
    // speaks the interface language. These three do not, because each of them reports that something the
    // themed window DEPENDS ON has failed: the strings dictionary, the startup path, or the dispatcher. A
    // dialog that cannot draw itself is a worse way to report a failure than a plain grey box that always
    // can, and this is the one place where the ugly thing is the reliable one.
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        // Last-resort net for a UI-thread exception that escaped a view model (a malformed core event,
        // an unreadable file) - shown to the user, never left to terminate the app silently (M-6, rule 6).
        DispatcherUnhandledException += OnDispatcherUnhandledException;

        try
        {
            // Merge the interface strings before the first window loads, so translation keys resolve.
            // Default culture is English - a language swap is a later slice.
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
        // A modal box per occurrence is not an option (R2-N12): a repeating exception - a binding that
        // throws on every heartbeat - opened one box per beat, and a window the user cannot out-click is
        // worse than the fault it reports. But the fix for that was a flag set once per RUN, which
        // silenced every later exception including UNRELATED ones. An early stumble in the preset list
        // then muted a coverage failure an hour later, and the app went on looking healthy while every
        // operation threw. That is rule 6 spread over a session.
        //
        // So the box is per SIGNATURE, not per run. A repeating fault still shows once - a genuinely new
        // one is still reported, because it is new information and the reader has not seen it.
        var signature = $"{e.Exception.GetType().FullName}|{e.Exception.StackTrace?.Split('\n').FirstOrDefault()?.Trim()}";
        if (_reportedSignatures.Count < MaxReportedSignatures && _reportedSignatures.Add(signature))
        {
            System.Windows.MessageBox.Show(
                e.Exception.Message, "Chrono Mock",
                System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Error);
        }

        e.Handled = true;
    }

    /// <summary>How many DISTINCT faults are reported before the app stops opening boxes. Past this many
    /// different failures the interface is not recoverable in any useful sense, and the boxes have stopped
    /// being information - one more would be noise on top of an app that is already broken.</summary>
    private const int MaxReportedSignatures = 8;

    /// <summary>Fault signatures already reported this run - type plus innermost frame, so a repeating
    /// fault is one box and a genuinely different one is still heard.</summary>
    private readonly HashSet<string> _reportedSignatures = new(StringComparer.Ordinal);
}

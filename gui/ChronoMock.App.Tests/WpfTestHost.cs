using System.Windows;
using System.Windows.Threading;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Tests;

/// <summary>
/// One shared WPF host for the whole test assembly (method from the betterwindowsservices project). WPF
/// needs three things at once, none of them the default in a test host: an STA thread, a live message
/// pump (Dispatcher.Run), and a single Application (Application.Current is static - a second `new` throws)
/// with ShutdownMode = OnExplicitShutdown so the first closed window does not end the app. The application
/// loads its real resources (wpfui dark theme + our dictionaries) and the default-culture strings, so a
/// constructed window resolves its StaticResource/DynamicResource references exactly as at runtime.
/// </summary>
internal static class WpfTestHost
{
    private static readonly Lazy<Dispatcher> Dispatcher = new(StartUiThread);

    private static Dispatcher StartUiThread()
    {
        var ready = new ManualResetEventSlim();
        Dispatcher? dispatcher = null;

        var thread = new Thread(() =>
        {
            if (Application.Current is null)
            {
                var app = new global::ChronoMock.App.App();
                app.InitializeComponent(); // load App.xaml resources (wpfui dark + Values + Colours)
                app.ShutdownMode = ShutdownMode.OnExplicitShutdown;
                LocalizationService.Apply(app, LocalizationService.DefaultCulture);
            }

            dispatcher = System.Windows.Threading.Dispatcher.CurrentDispatcher;
            ready.Set();
            System.Windows.Threading.Dispatcher.Run();
        })
        {
            IsBackground = true,
        };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();

        ready.Wait();
        return dispatcher!;
    }

    /// <summary>Run work on the UI thread and return its result.</summary>
    public static T Invoke<T>(Func<T> func) => Dispatcher.Value.Invoke(func);

    /// <summary>
    /// Run work on the UI thread, then drain the queue to ContextIdle so bindings are evaluated before the
    /// caller asserts. Without this last drain an assertion can read state from before the change,
    /// non-deterministically.
    /// </summary>
    public static T InvokeSettled<T>(Func<T> func)
    {
        return Dispatcher.Value.Invoke(() =>
        {
            var result = func();
            Dispatcher.Value.Invoke(() => { }, DispatcherPriority.ContextIdle);
            return result;
        });
    }
}

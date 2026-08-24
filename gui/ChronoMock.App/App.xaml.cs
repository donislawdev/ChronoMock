using System.Windows;
using ChronoMock.App.Localization;

namespace ChronoMock.App;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        // Merge the interface strings before the first window loads, so translation keys resolve.
        // Default culture is English; a language swap is a later slice.
        LocalizationService.Apply(this, LocalizationService.DefaultCulture);

        new MainWindow().Show();
    }
}

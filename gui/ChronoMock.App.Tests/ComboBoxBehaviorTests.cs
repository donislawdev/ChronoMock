using System.Reflection;
using System.Windows.Controls;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The attached behaviour that turns a ComboBox opening its drop-down into a command, so the recent-targets
/// list can re-check itself as it opens without a handler in a phase view (GUI rule 11). A ComboBox has no
/// Command of its own and DropDownOpened does not bubble, so this is the only rule-compliant bridge.
/// </summary>
/// <remarks>
/// The drop-down is raised through the protected OnDropDownOpened rather than by setting IsDropDownOpen: a
/// ComboBox coerces IsDropDownOpen back to false while it is not loaded into a shown window, so a headless
/// test cannot open it that way. Invoking the method the framework itself calls exercises the same event the
/// behaviour listens to, without standing up a window.
/// </remarks>
public class ComboBoxBehaviorTests
{
    [Fact]
    public void Opening_the_drop_down_runs_the_bound_command()
    {
        // Reversal probe: drop the DropDownOpened subscription in OnOpenedCommandChanged and this stays 0.
        var runs = WpfTestHost.InvokeSettled(() =>
        {
            var count = 0;
            var box = new ComboBox();
            ComboBoxBehavior.SetOpenedCommand(box, new RelayCommand(() => count++));

            RaiseDropDownOpened(box);

            return count;
        });

        Assert.Equal(1, runs);
    }

    [Fact]
    public void A_command_that_cannot_execute_is_not_run()
    {
        // Reversal probe: drop the CanExecute check in OnDropDownOpened and this runs anyway.
        var runs = WpfTestHost.InvokeSettled(() =>
        {
            var count = 0;
            var box = new ComboBox();
            ComboBoxBehavior.SetOpenedCommand(box, new RelayCommand(() => count++, () => false));

            RaiseDropDownOpened(box);

            return count;
        });

        Assert.Equal(0, runs);
    }

    private static void RaiseDropDownOpened(ComboBox box)
        => typeof(ComboBox)
            .GetMethod("OnDropDownOpened", BindingFlags.Instance | BindingFlags.NonPublic)!
            .Invoke(box, [EventArgs.Empty]);
}

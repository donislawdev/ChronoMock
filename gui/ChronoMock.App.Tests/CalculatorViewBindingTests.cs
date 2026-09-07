using System.IO;
using System.Windows;
using System.Windows.Controls;
using ChronoMock.App.Calc;
using ChronoMock.App.Views;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The reported failure: the custom-format field did nothing. It was bound with
/// <c>UpdateSourceTrigger=LostFocus</c>, and it is the last control in the result column, so a tester who
/// typed a mask and then looked at the result never moved focus and never saw one. Measured on the running
/// window before the fix: the box held "yyyy-MM-dd HH:mm:ss" and the result row was absent, then appeared
/// the moment focus went elsewhere.
/// <para>
/// This asserts the half a view-model test cannot reach. <see cref="CalculatorViewModel.CustomFormatMask"/>
/// already had its own tests and all of them passed over the dead field, because they set the property
/// directly - which is exactly what the view was failing to do.
/// </para>
/// <para>
/// <b>What this does not prove.</b> Nothing here computes a result. The view model is built on a client
/// that would launch a binary that does not exist, and the first-reveal gate is never opened, so the mask
/// setter schedules nothing. The question is only whether typing reaches the view model at all.
/// </para>
/// </summary>
public class CalculatorViewBindingTests
{
    [Fact]
    public void Typing_a_custom_format_mask_reaches_the_view_model_without_leaving_the_field()
    {
        var mask = WpfTestHost.InvokeSettled(() =>
        {
            var (view, vm) = NewCalculatorView();
            var box = (TextBox)view.FindName("CustomFormatBox");

            // Not vacuous: a box that never found its data context would take the text and report an
            // empty mask below for a reason that has nothing to do with the trigger.
            Assert.Same(vm, box.DataContext);

            // What typing does. Focus is never moved, which is the whole point.
            box.Text = "yyyy-MM-dd HH:mm:ss";
            return vm.CustomFormatMask;
        });

        Assert.Equal("yyyy-MM-dd HH:mm:ss", mask);
    }

    [Fact]
    public void Clearing_the_custom_format_mask_reaches_the_view_model_too()
    {
        // The other direction of the same wire, and the one the clear button on the wpfui box uses. A
        // mask that can be typed but not withdrawn would leave the result row stuck on screen.
        //
        // The first draft of this test asserted only the empty end state and was VACUOUS: put the old
        // LostFocus trigger back and it stayed green, because a mask that never arrives is also a mask
        // that never has to be withdrawn. It therefore reads the box twice, and the first read is what
        // makes the second one mean anything.
        var (typed, cleared) = WpfTestHost.InvokeSettled(() =>
        {
            var (view, vm) = NewCalculatorView();
            var box = (TextBox)view.FindName("CustomFormatBox");
            box.Text = "yyyy";
            var afterTyping = vm.CustomFormatMask;
            box.Text = string.Empty;
            return (afterTyping, vm.CustomFormatMask);
        });

        Assert.Equal("yyyy", typed);
        Assert.Equal(string.Empty, cleared);
    }

    /// <summary>A calculator screen wired to a view model that can never start a process: the path callback
    /// hands out a name that does not exist, and nothing here opens the first-reveal gate that would make
    /// the mask setter schedule a recompute.</summary>
    private static (CalculatorView View, CalculatorViewModel ViewModel) NewCalculatorView()
    {
        var client = new CalcClient(() => Path.Combine(Path.GetTempPath(), "chrono-does-not-exist-here.exe"));
        var vm = new CalculatorViewModel(client);
        var view = new CalculatorView { DataContext = vm };

        // Lay the control out so its template is applied and every binding is attached. An unmeasured
        // control can leave bindings unattached, which would make the assertion above pass or fail for
        // the wrong reason (the same trap TargetBoxTests documents for an unshown Window).
        view.Measure(new Size(1600, 1400));
        view.Arrange(new Rect(0, 0, 1600, 1400));
        view.UpdateLayout();
        return (view, vm);
    }
}

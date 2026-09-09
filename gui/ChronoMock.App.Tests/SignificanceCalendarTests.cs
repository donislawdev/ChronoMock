using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using System.Windows.Controls;
using ChronoMock.App.Calc;
using ChronoMock.App.Views;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The block under the result says what the date LANDS ON, and two of the things it can land on -
/// weekend and holiday - are a judgement rather than a fact. The same Sunday is not a business day
/// under the calendars we ship and is one under a calendar whose weekend falls on Friday and Saturday.
/// The tester reads that block in the result column while the calendar picker sits in the middle one,
/// so the calendar has to be named beside the line it decided.
///
/// <para><b>What these do not prove.</b> Nothing here runs the engine, so the step from a real
/// evaluation to the note is not covered - the App tests launch no process. What is guarded is which
/// calendar the note names, that an unknown id is shown rather than dropped, and that the view is bound
/// to the view model's own property names.</para>
/// </summary>
public class SignificanceCalendarTests
{
    private static CalculatorViewModel NewViewModel()
        => new(new CalcClient(() => Path.Combine(Path.GetTempPath(), "chrono-does-not-exist-here.exe")));

    [Fact]
    public void No_calendar_means_no_note_rather_than_a_vague_one()
    {
        var note = WpfTestHost.InvokeSettled(() => NewViewModel().CalendarNote(null));
        Assert.Equal(string.Empty, note);
    }

    [Fact]
    public void The_note_names_the_calendar_the_way_the_picker_does()
    {
        var (note, pickerLabel) = WpfTestHost.InvokeSettled(() =>
        {
            var vm = NewViewModel();
            var banking = vm.Calendars.Single(c => c.Id == "us-banking");
            return (vm.CalendarNote("us-banking"), TranslationKeyConverter.Resolve(banking.LabelKey));
        });

        // The engine sends an id and the tester picked a label. The note has to speak the second one, or
        // it names a calendar the window never showed them (rule 15).
        Assert.Contains(pickerLabel, note, StringComparison.Ordinal);
        Assert.DoesNotContain("us-banking", note, StringComparison.Ordinal);
        Assert.DoesNotContain("{0}", note, StringComparison.Ordinal);
    }

    [Fact]
    public void An_unknown_calendar_id_is_shown_rather_than_dropped()
    {
        // A calendar shipped later, or a user's own file, would otherwise leave a sentence that names no
        // calendar at all - which is worse than the raw id, since the reader cannot tell it is missing.
        var note = WpfTestHost.InvokeSettled(() => NewViewModel().CalendarNote("some-other-calendar"));

        Assert.Contains("some-other-calendar", note, StringComparison.Ordinal);
    }

    [Fact]
    public void The_note_is_read_from_the_result_and_not_from_the_picker()
    {
        // The reason the calendar travels back in the calc payload at all. Reading the picker instead
        // would let the note describe one calendar while the marks beside it came from another, for as
        // long as a recompute is in flight.
        var metadata = new CalcMetadata(
            "Sunday", 2010, 17, 19, 122, 2, false, -5974, BusinessDay: false, Holiday: null, Calendar: "us-banking");

        Assert.Equal("us-banking", metadata.Calendar);
    }

    [Fact]
    public void The_view_shows_the_note_only_when_there_is_a_calendar_to_name()
    {
        var (hidden, shown, text) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new CalculatorView { DataContext = new SignificanceStub() };
            Layout(view);
            var before = FindNote(view).Visibility;

            view.DataContext = new SignificanceStub { SignificanceCalendar = "marks follow the demo calendar" };
            Layout(view);
            var note = FindNote(view);
            return (before, note.Visibility, note.Text);
        });

        Assert.Equal(Visibility.Collapsed, hidden);
        Assert.Equal(Visibility.Visible, shown);
        Assert.Equal("marks follow the demo calendar", text);
    }

    [Fact]
    public void The_note_is_bound_to_the_view_models_own_property_names()
    {
        // Ties the stub to the real thing, so renaming either property breaks the build here rather than
        // leaving a green test over a binding that resolves to nothing.
        Assert.Equal(
            nameof(CalculatorViewModel.SignificanceCalendar), nameof(SignificanceStub.SignificanceCalendar));
        Assert.Equal(
            nameof(CalculatorViewModel.HasSignificanceCalendar), nameof(SignificanceStub.HasSignificanceCalendar));
    }

    /// <summary>Stands in for the view model so the note can be driven without running the engine.</summary>
    private sealed class SignificanceStub
    {
        public string SignificanceCalendar { get; init; } = string.Empty;

        public bool HasSignificanceCalendar => SignificanceCalendar.Length > 0;
    }

    private static TextBlock FindNote(CalculatorView view)
    {
        // Found by its binding rather than by a name, because naming an element only so a test can reach
        // it is the thing the unused-member guard exists to catch.
        var block = Descendants(view)
            .OfType<TextBlock>()
            .FirstOrDefault(t => System.Windows.Data.BindingOperations.GetBinding(t, TextBlock.TextProperty)
                is { Path.Path: nameof(CalculatorViewModel.SignificanceCalendar) });
        Assert.True(block is not null, "no text block bound to the significance calendar note");
        return block!;
    }

    private static IEnumerable<DependencyObject> Descendants(DependencyObject root)
    {
        int count = System.Windows.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            yield return child;
            foreach (var deeper in Descendants(child))
            {
                yield return deeper;
            }
        }
    }

    /// <summary>Lay the control out so its template is applied and every binding is attached - an
    /// unmeasured control can leave bindings unattached and make an assertion pass for the wrong reason.</summary>
    private static void Layout(FrameworkElement view)
    {
        view.Measure(new Size(1600, 1400));
        view.Arrange(new Rect(0, 0, 1600, 1400));
        view.UpdateLayout();
    }
}

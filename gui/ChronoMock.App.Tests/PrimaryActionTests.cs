using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The panel's one action is Start or Stop depending on the session, and the whole switch rests on a
/// style trigger. That makes it silently breakable in a way a screenshot of an idle window would never
/// show: in WPF a LOCAL value outranks a trigger, so writing Content, Background or IsEnabled onto the
/// element leaves a button that says Start for the entire life of a running session and cannot stop it.
/// The panel would look right and do the wrong thing.
///
/// So the guard is on the trap rather than on the appearance. It asserts that none of the three
/// properties the running state replaces is set locally, and that the trigger which replaces them is
/// still there and still watches the running flag. Colour is checked as WELL as the label, never
/// instead of it (zasady/13 9). What this does NOT prove is how any of it looks - that was measured on a
/// live window, by pixel, and belongs in the changelog rather than here.
/// </summary>
public class PrimaryActionTests
{
    private static Button PrimaryAction()
        => WpfTestHost.InvokeSettled(() =>
        {
            var window = new MainWindow();
            return (Button)window.FindName("PrimaryActionButton")!;
        });

    [Theory]
    [InlineData("Content")]
    [InlineData("Background")]
    [InlineData("IsEnabled")]
    public void The_running_state_is_not_outranked_by_a_local_value(string property)
    {
        var dependencyProperty = property switch
        {
            "Content" => ContentControl.ContentProperty,
            "Background" => Control.BackgroundProperty,
            "IsEnabled" => UIElement.IsEnabledProperty,
            _ => throw new ArgumentOutOfRangeException(nameof(property), property, "unknown property"),
        };

        var local = WpfTestHost.InvokeSettled(() => PrimaryAction().ReadLocalValue(dependencyProperty));

        Assert.Same(DependencyProperty.UnsetValue, local);
    }

    [Fact]
    public void The_button_becomes_the_stop_control_while_a_session_runs()
    {
        var button = PrimaryAction();
        var style = WpfTestHost.InvokeSettled(() => button.Style);
        Assert.NotNull(style);

        // The value a DataTrigger carries from XAML stays the TEXT "True" rather than the boolean, since
        // the parser has no type for a binding path, so comparing against true never matches.
        var running = style!.Triggers.OfType<DataTrigger>().SingleOrDefault(
            trigger => trigger.Binding is Binding { Path.Path: nameof(SessionViewModel.IsRunning) }
                       && string.Equals(trigger.Value?.ToString(), "True", StringComparison.OrdinalIgnoreCase));
        Assert.True(running is not null, "no trigger on the running flag - the button can no longer stop a session");

        var replaced = running!.Setters.OfType<Setter>().Select(setter => setter.Property).ToList();
        Assert.Contains(ContentControl.ContentProperty, replaced);
        Assert.Contains(Control.BackgroundProperty, replaced);
        Assert.Contains(UIElement.IsEnabledProperty, replaced);

        // The label carries the meaning and the fill only reinforces it, so the label has to be the
        // session-stopping one rather than any replacement text.
        var content = running.Setters.OfType<Setter>().Single(setter => setter.Property == ContentControl.ContentProperty);
        Assert.Equal("control.stop", (content.Value as DynamicResourceExtension)?.ResourceKey);
    }

    [Fact]
    public void The_idle_label_and_the_running_label_are_different_strings()
    {
        // The canary for the test above: it would pass just as happily if both states resolved to the
        // same words, which is the one outcome that makes the whole switch pointless.
        var idle = WpfTestHost.InvokeSettled(() => Application.Current.TryFindResource("action.start") as string);
        var running = WpfTestHost.InvokeSettled(() => Application.Current.TryFindResource("control.stop") as string);

        Assert.False(string.IsNullOrWhiteSpace(idle));
        Assert.False(string.IsNullOrWhiteSpace(running));
        Assert.NotEqual(idle, running);
    }
}

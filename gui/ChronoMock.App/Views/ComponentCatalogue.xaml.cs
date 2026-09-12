using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The component catalogue: every part of the interface, in every state that can be shown without input.
/// </summary>
/// <remarks>
/// It carries no behaviour on purpose. A catalogue that computed anything would be a second screen to
/// keep working rather than a picture of the parts library, and the parts it shows are drawings, not
/// controls with logic behind them.
/// </remarks>
public partial class ComponentCatalogue : UserControl
{
    public ComponentCatalogue() => InitializeComponent();

    /// <summary>
    /// Sample clocks for the clock part, which cannot be written in the markup.
    /// </summary>
    /// <remarks>
    /// 🔴 DATA, NOT BEHAVIOUR - the distinction this class's remarks draw. ClockView takes its role in the
    /// constructor and exposes the time through one settable member, so a compiled XAML file cannot build
    /// one: there is no parameterless constructor for it to call. Leaving the clock out of the catalogue
    /// was the alternative, and a catalogue missing a part it has is the fault the catalogue exists to
    /// prevent.
    ///
    /// The pair is deliberate. Both clocks are ONE drawing with different data, and the only way to see
    /// that they have not drifted apart is to put them next to each other, which is also how the session
    /// phase shows them.
    /// </remarks>
    public ClockView SampleFakeClock { get; } = new("clock.fake")
    {
        Wall = "2038-01-19T03:14:07",
        Zone = "UTC+00:00",
    };

    /// <summary>See <see cref="SampleFakeClock"/>.</summary>
    public ClockView SampleRealClock { get; } = new("clock.real")
    {
        Wall = "2026-09-12T20:30:00",
        Zone = "UTC+02:00",
    };

    /// <summary>The extreme the catalogue is for: a zone label far longer than any real one, and a time
    /// that has run past four digits of hours. If either overflows the card, it does it here rather than
    /// on somebody's screen.</summary>
    public ClockView SampleExtremeClock { get; } = new("clock.fake")
    {
        Wall = "2038-01-19T03:14:07",
        Zone = "UTC+14:00 - Line Islands, Kiritimati, the furthest offset there is",
    };
}

using System.Windows;
using ChronoMock.Protocol;

namespace ChronoMock.App.Views;

/// <summary>
/// The About window: which build this is, whose it is, what licence it is under, and what somebody else
/// wrote inside it.
/// <para>
/// Opened through the static helper rather than by constructing this type, for the reason
/// <see cref="MessageDialog"/> is: a dialog with no owner opens wherever Windows likes and does not stay
/// above the application.
/// </para>
/// </summary>
public partial class AboutDialog
{
    private readonly LicenseClient _licence;
    private readonly CancellationTokenSource _closing = new();
    private bool _componentsAsked;

    /// <summary>
    /// Internal, and it does NOT take the owner.
    /// </summary>
    /// <remarks>
    /// Two reasons, and the second was found by a red test rather than reasoned out. WPF refuses to set
    /// <c>Owner</c> to a window that has never been shown, so a constructor that took one could only ever
    /// run against a live window - which is exactly what a build test does not have. Ownership therefore
    /// belongs to <see cref="Show"/>, where the owner is on screen by definition. That also keeps the
    /// promise the ownership was there for: callers outside this assembly reach this window only through
    /// <see cref="Show"/>, so none of them can forget it.
    /// </remarks>
    internal AboutDialog(LicenseClient licence)
    {
        InitializeComponent();
        _licence = licence;

        // The product line is built from the SAME format the title bar uses, so the window cannot end up
        // naming a different version from the one on the frame behind it.
        ProductLine.Text = AppVersion.FormatTitle(Text("app.title"), AppVersion.Current);
        CopyrightLine.Text = AppLicence.Copyright;
        SpdxLine.Text = AppLicence.Spdx;

        // Named when present, admitted when absent. Both files ship in the package and neither is beside
        // the window in a dev checkout, so a path printed unconditionally would point at nothing.
        LicenceFileLine.Text = AppLicence.LicenceFile ?? Text("about.file_absent");
        NoticesFileLine.Text = AppLicence.NoticesFile ?? Text("about.file_absent");

        // Cancels the component query if the reader closes the window while it is still running, so a
        // dialog that is gone cannot be written to.
        Closed += (_, _) => _closing.Cancel();
    }

    /// <summary>Show the About window over its owner.</summary>
    /// <remarks>
    /// The only way in from outside this assembly, for the reason <see cref="MessageDialog"/> has one: a
    /// dialog with no owner opens wherever Windows likes and does not stay above the application.
    /// </remarks>
    public static void Show(Window owner, LicenseClient licence)
        => new AboutDialog(licence) { Owner = owner }.ShowDialog();

    /// <summary>
    /// Fetch the component register the first time the expander is opened, and never again.
    /// </summary>
    /// <remarks>
    /// Lazy on purpose: reading a copyright line should not cost a process start. Asked once because the
    /// answer is compiled into the core and cannot change while this window is open, so re-asking on every
    /// toggle would only add a way for the list to flicker.
    /// </remarks>
    private async void OnComponentsExpanded(object sender, RoutedEventArgs e)
    {
        if (_componentsAsked)
        {
            return;
        }

        _componentsAsked = true;
        ComponentsText.Text = Text("about.components_asking");

        var register = await _licence.TryReadComponentsAsync(_closing.Token).ConfigureAwait(true);

        // Say that it could not be read rather than showing an empty list. An empty register reads as
        // "there is nothing third-party in here", which is the one answer this must never give by
        // accident (untouchable rules 4 and 6) - and the notices file named above is the way out.
        ComponentsText.Text = register is null
            ? Text("about.components_unavailable")
            : LicenseClient.RegisterOnly(register);
    }

    // The online copy of the licence, for a build with no LICENSE beside it. The address is a constant in
    // ExternalLinks, never assembled here, and the failure is said out loud with the address under it -
    // the same shape as the support button, for the same reason.
    private void OnReadLicenceClick(object sender, RoutedEventArgs e)
    {
        if (!ExternalLinks.TryOpenLicence())
        {
            MessageDialog.Tell(
                this,
                Text("support.failed_title"),
                Text("support.failed") + Environment.NewLine + Environment.NewLine + ExternalLinks.Licence);
        }
    }

    private void OnCloseClick(object sender, RoutedEventArgs e) => Close();

    /// <summary>A translation key resolved to text, falling back to the raw key (rule 15).</summary>
    private static string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;
}

using System.Diagnostics;

namespace ChronoMock.App;

/// <summary>
/// The addresses this application can hand to the user's browser, and the one way it hands them over.
/// <para>
/// Every destination is a constant in this file, and no caller supplies one. That is a safety property
/// rather than tidiness: <c>UseShellExecute</c> asks the Windows shell to open whatever string it is
/// given, so a caller free to pass its own string would turn a support button into a way of starting
/// anything. Callers therefore choose a named destination, never an address.
/// </para>
/// <para>
/// The application still opens no socket of its own, which is the promise the README makes. What happens
/// here is a handover - the address goes to the shell, the shell starts the browser the user already
/// chose, and the browser is what connects. The button says this before it is pressed, because a tool
/// that promises to stay offline cannot afford to surprise anybody about that.
/// </para>
/// </summary>
internal static class ExternalLinks
{
    /// <summary>Where somebody who wants to support the project is sent.</summary>
    internal const string Support = "https://donislawdev.com/support/";

    /// <summary>
    /// The full text of the GNU GPL, for a copy of this application that does not carry its own.
    /// </summary>
    /// <remarks>
    /// The same address the core prints for the same reason, and it is a fallback rather than the plan:
    /// the package ships <c>LICENSE</c> beside the executable, and the About window names that file when
    /// it is there. This is what a reader gets when it is not, which is a real case (a dev checkout, or
    /// an executable copied out of its folder) and the one where an offline tool has to admit it cannot
    /// show the text itself.
    /// </remarks>
    internal const string Licence = "https://www.gnu.org/licenses/gpl-3.0.html";

    /// <summary>Hand the licence text to the user's browser, and report whether the shell took it.</summary>
    internal static bool TryOpenLicence() => TryOpen(Licence);

    /// <summary>Hand the support page to the user's browser, and report whether the shell took it.</summary>
    /// <remarks>
    /// A bool rather than an exception, because there is a real machine where this fails - one with no
    /// browser associated with https at all. The caller has to say so and show the address, since a
    /// button that quietly does nothing is the one outcome this must not have (untouchable rule 6).
    /// </remarks>
    internal static bool TryOpenSupport() => TryOpen(Support);

    /// <summary>
    /// Hand one address to the shell.
    /// </summary>
    /// <remarks>
    /// Internal rather than private only so a test can drive it with an address the shell is certain to
    /// refuse, which is the only honest way to prove the failure path. Everything else in the app goes
    /// through a named destination above, and a guard in the test suite keeps it that way.
    /// </remarks>
    internal static bool TryOpen(string address)
    {
        try
        {
            // UseShellExecute is what makes this open the user's browser instead of trying to execute the
            // address, and it is also the reason the destinations are constants in this file.
            using var started = Process.Start(new ProcessStartInfo(address) { UseShellExecute = true });
            return true;
        }
        catch (Exception failure) when (failure
            is System.ComponentModel.Win32Exception
            or InvalidOperationException
            or System.IO.FileNotFoundException
            or PlatformNotSupportedException)
        {
            // Swallowed here and reported by the caller, which is a different thing from going quiet:
            // the caller owns the window, so it is the one that can show the address the user can type
            // in by hand instead.
            return false;
        }
    }
}

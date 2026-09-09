using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Reflection;

namespace ChronoMock.App;

/// <summary>
/// What the window can state about its own licence without asking anybody, and where the two shipped
/// licence files are when they are on disk.
/// <para>
/// The copyright line is READ FROM THE ASSEMBLY rather than written here, so it comes from
/// <c>gui/Directory.Build.props</c> - the same line a test on the Rust side already compares against the
/// core's own constant, which is what stops the two halves of one product claiming different years. The
/// SPDX identifier below is the one fact with no such home on this side, so it has a mirror test of its
/// own against the workspace manifest.
/// </para>
/// </summary>
internal static class AppLicence
{
    /// <summary>
    /// The licence this product is under, as an SPDX identifier.
    /// </summary>
    /// <remarks>
    /// Written down here and mirrored against <c>Cargo.toml</c> by a test, in the manner of the other
    /// constants that exist twice across the language boundary. An identifier rather than a translation
    /// key on purpose: "GPL-3.0-only" is a name, and a translated licence name would name a licence that
    /// does not exist.
    /// </remarks>
    internal const string Spdx = "GPL-3.0-only";

    private const string LicenceFileName = "LICENSE";
    private const string NoticesFileName = "THIRD-PARTY-NOTICES.md";

    /// <summary>The copyright line this build carries, from the assembly metadata.</summary>
    /// <remarks>
    /// Degrades to an empty string rather than throwing. A window that cannot draw because its own
    /// copyright attribute is missing would be a worse failure than a window that draws without one, and
    /// the licence statement beside it does not depend on this line being present.
    /// </remarks>
    internal static string Copyright { get; } = WithoutLicenceSentence(
        typeof(AppLicence).Assembly.GetCustomAttribute<AssemblyCopyrightAttribute>()?.Copyright ?? string.Empty);

    /// <summary>
    /// The copyright line without the licence sentence the assembly attribute appends to it.
    /// </summary>
    /// <remarks>
    /// The metadata reads "Copyright (C) yyyy holder. Licensed under GPL-3.0-only.", which is right for a
    /// file property - Explorer shows that one field and nothing else - and wrong for a window that states
    /// the licence on a line of its own two rows below. Measured on the first About render: the same
    /// identifier stood in two adjacent lines and read as a mistake rather than as emphasis. Cut here
    /// rather than removed from the metadata, because those are two readers with two different needs.
    /// Anything unrecognised is returned whole, so a reworded attribute loses nothing.
    /// </remarks>
    internal static string WithoutLicenceSentence(string copyright)
    {
        ArgumentNullException.ThrowIfNull(copyright);

        var at = copyright.IndexOf(". Licensed under", StringComparison.Ordinal);
        return at >= 0 ? copyright[..at] : copyright;
    }

    /// <summary>The full licence text beside the running application, or <c>null</c> when it is not there.</summary>
    internal static string? LicenceFile => Beside(LicenceFileName);

    /// <summary>The third-party notices beside the running application, or <c>null</c> when not there.</summary>
    internal static string? NoticesFile => Beside(NoticesFileName);

    /// <summary>
    /// One shipped file beside the executable, or <c>null</c>.
    /// </summary>
    /// <remarks>
    /// Looked up rather than assumed, and reported as absent rather than named optimistically. Both files
    /// ship in the package and neither sits beside the window in a dev checkout, so a window that printed
    /// a path unconditionally would be pointing at nothing on the machine of whoever builds this. That is
    /// the same honesty the core applies to the same question (rule 4).
    /// </remarks>
    private static string? Beside(string fileName)
    {
        try
        {
            var path = Path.Combine(AppContext.BaseDirectory, fileName);
            return File.Exists(path) ? path : null;
        }
        catch (Exception failure) when (failure is IOException or UnauthorizedAccessException
            or ArgumentException)
        {
            // An unreadable directory is not worth taking the window down for - the address of the online
            // copy is shown either way, so the reader still has a way to the licence.
            return null;
        }
    }
}

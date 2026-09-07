using System.Reflection;

namespace ChronoMock.App;

/// <summary>
/// The product version this build carries, and the one place that puts it into a title.
/// </summary>
/// <remarks>
/// Read from the assembly rather than written down here. The version already lives in two places
/// that must be bumped together (the Rust workspace manifest for the CLI, gui/Directory.Build.props
/// for this stack), and a third copy in source or in a strings file would be a third thing to
/// forget.
/// </remarks>
public static class AppVersion
{
    /// <summary>The version of the running build, for example <c>0.1.0</c>.</summary>
    public static string Current { get; } = ReadFromAssembly();

    /// <summary>
    /// Fill a window-title format with the version, for example <c>Chrono Mock {0}</c>.
    /// </summary>
    /// <remarks>
    /// A title is translated, so the format arrives from a file a user is invited to edit. Two ways
    /// that goes wrong are handled here rather than at startup: a format with no placeholder is
    /// returned unchanged (a title without a version reads fine), and a format naming an argument
    /// that does not exist would otherwise throw and take the window with it. Both degrade to a
    /// usable title, which is the same choice ApplyOrDegrade makes for the strings as a whole.
    /// </remarks>
    public static string FormatTitle(string format, string version)
    {
        ArgumentNullException.ThrowIfNull(format);

        try
        {
            return string.Format(System.Globalization.CultureInfo.InvariantCulture, format, version);
        }
        catch (FormatException)
        {
            return format;
        }
    }

    private static string ReadFromAssembly()
    {
        var assembly = typeof(AppVersion).Assembly;

        // InformationalVersion is what Directory.Build.props sets, and it is deliberately kept a
        // clean "0.1.0" there rather than carrying a commit hash. AssemblyVersion is the fallback
        // and prints four parts, which is why it is second rather than first.
        var informational = assembly
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?
            .InformationalVersion;

        if (!string.IsNullOrWhiteSpace(informational))
        {
            return informational;
        }

        return assembly.GetName().Version?.ToString() ?? "unknown";
    }
}

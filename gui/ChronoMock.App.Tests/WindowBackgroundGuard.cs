using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Xml.Linq;

namespace ChronoMock.App.Tests;

/// <summary>
/// The dead-attribute guard: a Background written onto a ui:FluentWindow root paints nothing.
///
/// wpfui accepts the attribute, the parser accepts it, the literal guard accepts it (it is a NAMED
/// resource, which is exactly what that guard asks for), and the window renders in the library's default
/// colour regardless. Both windows in this app carried one for months and neither ever painted a pixel.
/// A declaration that looks identical to a working one and does nothing is worse than no declaration,
/// so this guard refuses the shape rather than trusting anyone to remember.
///
/// This is deliberately only HALF the protection, and the weaker half. It proves nobody re-added the
/// dead idiom, not that the app has a background at all - a view could satisfy it by having no colour
/// anywhere. The half that measures the paint is the render assertion in BackgroundGuardTests.
/// </summary>
internal static class WindowBackgroundGuard
{
    /// <summary>A view whose FluentWindow root carries a Background that will never be painted.</summary>
    public sealed record Offender(string File, string Value);

    private static readonly XName FluentWindow =
        XName.Get("FluentWindow", "http://schemas.lepo.co/wpfui/2022/xaml");

    /// <summary>Inspect one XAML text. Parsed as XML rather than pattern-matched, so a reformatted
    /// attribute list, a comment above the root, or a line break mid-element cannot hide the attribute.</summary>
    public static IReadOnlyList<Offender> Inspect(string file, string xaml)
    {
        var root = XDocument.Parse(xaml).Root;
        if (root is null || root.Name != FluentWindow)
        {
            return [];
        }

        var background = root.Attribute("Background");
        return background is null ? [] : [new Offender(file, background.Value)];
    }

    /// <summary>Every XAML under the app directory, walked rather than listed, so moving a view does not
    /// quietly drop it out of the guard's sight.</summary>
    public static IReadOnlyList<Offender> Scan(string appDirectory)
    {
        var offenders = new List<Offender>();
        foreach (var file in EnumerateViews(appDirectory))
        {
            offenders.AddRange(Inspect(Relative(appDirectory, file), File.ReadAllText(file)));
        }

        return offenders;
    }

    /// <summary>How many views this guard actually reads, and how many of them are FluentWindow roots.
    /// The canary for the scan itself: a guard that reads nothing is green and useless.</summary>
    public static (int Views, int Windows) Coverage(string appDirectory)
    {
        int views = 0;
        int windows = 0;
        foreach (var file in EnumerateViews(appDirectory))
        {
            views++;
            if (XDocument.Parse(File.ReadAllText(file)).Root?.Name == FluentWindow)
            {
                windows++;
            }
        }

        return (views, windows);
    }

    private static IEnumerable<string> EnumerateViews(string appDirectory)
        => Directory.EnumerateFiles(appDirectory, "*.xaml", SearchOption.AllDirectories)
            .Where(file => !IsBuildOutput(Relative(appDirectory, file)));

    private static string Relative(string appDirectory, string file)
        => Path.GetRelativePath(appDirectory, file).Replace('\\', '/');

    private static bool IsBuildOutput(string relative)
        => relative.StartsWith("obj/", StringComparison.Ordinal)
            || relative.StartsWith("bin/", StringComparison.Ordinal);
}

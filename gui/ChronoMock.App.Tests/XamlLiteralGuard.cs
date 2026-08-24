using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;

namespace ChronoMock.App.Tests;

/// <summary>
/// The design-literal guard (zasady/13 sections 2.2 and 5): a narrow scan of VIEW XAML for design values
/// written in place where a named reference belongs - a hard-coded colour, font size, or spacing. It is
/// deliberately narrow (a wide "suspicious numbers" scan is the guard that gets switched off in a week).
///
/// What it does NOT catch, on purpose: element/window geometry (Width/Height/MinWidth/MinHeight), a value
/// built from named parts, a value computed in code, and a resource assigned to the wrong-typed property.
/// It scans views only - the Themes dictionaries DEFINE the values and the Localization files hold strings.
/// </summary>
internal static class XamlLiteralGuard
{
    public sealed record Violation(string File, int Line, string Kind, string Snippet);

    private static readonly (string Kind, Regex Pattern)[] Rules =
    [
        // A literal hex colour in a brush/colour attribute.
        ("colour", new Regex("""(?i)\b(Foreground|Background|Fill|Stroke|BorderBrush|Color)\s*=\s*"#""", RegexOptions.Compiled)),
        // A literal numeric font size.
        ("font-size", new Regex("""(?i)\bFontSize\s*=\s*"[0-9]""", RegexOptions.Compiled)),
        // A literal spacing/thickness/radius. "0" is allowed - it is not a design token worth naming.
        ("spacing", new Regex("""(?i)\b(Margin|Padding|BorderThickness|CornerRadius)\s*=\s*"(?!0")[0-9.\-]""", RegexOptions.Compiled)),
    ];

    /// <summary>Find design-literal violations in a single XAML text.</summary>
    public static IReadOnlyList<Violation> FindViolations(string file, string content)
    {
        var violations = new List<Violation>();
        var lines = content.Replace("\r\n", "\n", StringComparison.Ordinal).Split('\n');
        for (int i = 0; i < lines.Length; i++)
        {
            foreach (var (kind, pattern) in Rules)
            {
                if (pattern.IsMatch(lines[i]))
                {
                    violations.Add(new Violation(file, i + 1, kind, lines[i].Trim()));
                }
            }
        }

        return violations;
    }

    /// <summary>Scan every view XAML under the app directory (skipping the value dictionaries, the string
    /// files, and build output).</summary>
    public static IReadOnlyList<Violation> ScanViews(string appDirectory)
    {
        var violations = new List<Violation>();
        foreach (var file in Directory.EnumerateFiles(appDirectory, "*.xaml", SearchOption.AllDirectories))
        {
            var rel = Path.GetRelativePath(appDirectory, file).Replace('\\', '/');
            if (IsExcluded(rel))
            {
                continue;
            }

            violations.AddRange(FindViolations(rel, File.ReadAllText(file)));
        }

        return violations;
    }

    private static bool IsExcluded(string relativePath)
        => relativePath.StartsWith("Themes/", StringComparison.OrdinalIgnoreCase)
           || relativePath.StartsWith("Localization/", StringComparison.OrdinalIgnoreCase)
           || relativePath.StartsWith("bin/", StringComparison.OrdinalIgnoreCase)
           || relativePath.StartsWith("obj/", StringComparison.OrdinalIgnoreCase)
           || relativePath.Contains("/bin/", StringComparison.OrdinalIgnoreCase)
           || relativePath.Contains("/obj/", StringComparison.OrdinalIgnoreCase);
}

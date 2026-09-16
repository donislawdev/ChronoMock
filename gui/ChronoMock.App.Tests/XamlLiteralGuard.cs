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
///
/// 🔴 WHAT IT SCANS, and why that changed. It used to skip all of Themes/, which was right when Themes/
/// held only definitions. It is wrong now: control templates live there too, and a template is a CONSUMER
/// of the scale exactly like a view. Measured when the exclusion was narrowed - the style dictionary was
/// already carrying Padding="7,6" and FontSize="16", neither of which is on any declared scale, and no
/// guard had ever looked at them. The split is now by ROLE: a file that DEFINES values is skipped, a file
/// that USES them is scanned.
///
/// The other half of the same blind spot stays open and is not this guard's job: SpacingReport only judges
/// a gap whose nearest named ancestor is named in our views, so gaps INSIDE a control template are not
/// measured on the render either. That is why this text scan has to hold that ground.
/// </summary>
internal static class XamlLiteralGuard
{
    public sealed record Violation(string File, int Line, string Kind, string Snippet);

    private static readonly (string Kind, Regex Pattern)[] Rules =
    [
        // 🔴 ANY literal colour in a brush attribute, not just a hex one. This used to require a "#", so
        // Background="White" and Fill="Red" walked straight past a guard whose whole job is colour coming
        // from the palette - and WPF has 140-odd named colours to walk past it with. The rule is now
        // inverted: a value that does not START a markup extension is a literal, whatever it spells.
        //
        // Transparent is the one word allowed, and it is not a colour decision - it means "paint nothing
        // here", which is exactly what a template says when the fill belongs to a parent or to a
        // TemplateBinding. Measured when this was tightened: 8 named colours in the whole of the scanned
        // XAML and all 8 of them Transparent, so nothing needed an allowance.
        ("colour", new Regex("""(?i)\b(Foreground|Background|Fill|Stroke|BorderBrush|Color)\s*=\s*"(?!\{|Transparent")""", RegexOptions.Compiled)),
        // A literal numeric font size.
        ("font-size", new Regex("""(?i)\bFontSize\s*=\s*"[0-9]""", RegexOptions.Compiled)),
        // A literal spacing/thickness/radius. "0" is allowed - it is not a design token worth naming.
        ("spacing", new Regex("""(?i)\b(Margin|Padding|BorderThickness|CornerRadius)\s*=\s*"(?!0")[0-9.\-]""", RegexOptions.Compiled)),
        // The same decisions written in a Setter: Property names the target, Value carries the literal. The
        // attribute rules above miss these because the value is in Value=, not in the property attribute. The
        // Value lookaheads keep the same exemptions - a markup extension, Transparent, a bare 0.
        ("setter-colour", new Regex("""(?i)<Setter\b(?=[^>]*\bProperty\s*=\s*"(?:\w+\.)?(Foreground|Background|Fill|Stroke|BorderBrush|Color)")(?=[^>]*\bValue\s*=\s*"(?!\{|Transparent"))""", RegexOptions.Compiled)),
        ("setter-spacing", new Regex("""(?i)<Setter\b(?=[^>]*\bProperty\s*=\s*"(?:\w+\.)?(Margin|Padding|BorderThickness|CornerRadius|FontSize)")(?=[^>]*\bValue\s*=\s*"(?!0")[0-9.\-])""", RegexOptions.Compiled)),
    ];

    /// <summary>
    /// Literals that stay for now, each with the reason and the condition under which it goes.
    /// </summary>
    /// <remarks>
    /// 🔴 These all sit in styles that the parts library REPLACES. Rewriting them to tokens first would
    /// mean editing code that is about to be deleted, and would move the pixels of a shipped build for no
    /// reason. The list only shrinks: <see cref="Allowances"/> is checked from both ends, so an entry that
    /// stops matching anything has to come out, and a new literal cannot hide behind an old excuse.
    ///
    /// The match is on the exact trimmed line, not a line number, so ordinary edits above do not silently
    /// re-arm an allowance somewhere else.
    /// </remarks>
    internal static readonly (string File, string Line, string Reason)[] Allowances =
    [
        ("Themes/Controls.xaml",
         """BorderThickness="1" CornerRadius="4" Padding="7,6" SnapsToDevicePixels="True">""",
         "CalendarToggleStyle face; 7 is on no scale. Goes when the toggle gets its own template (2b)"),
        ("Themes/Controls.xaml",
         """<TextBlock Text="&#xE787;" FontFamily="Segoe MDL2 Assets" FontSize="16" """.TrimEnd(),
         "calendar glyph sized off the type scale; goes with the same template (2b)"),
        ("Themes/Controls.xaml",
         """BorderThickness="1" CornerRadius="4" Padding="10,6" SnapsToDevicePixels="True">""",
         "TitleBarActionButton face; goes when the button gets its own template (2b)"),
    ];

    /// <summary>Find design-literal violations in a single XAML text.</summary>
    public static IReadOnlyList<Violation> FindViolations(string file, string content)
    {
        var violations = new List<Violation>();
        var lines = content.Replace("\r\n", "\n", StringComparison.Ordinal).Split('\n');
        for (int i = 0; i < lines.Length; i++)
        {
            var trimmed = lines[i].Trim();
            if (IsAllowed(file, trimmed))
            {
                continue;
            }

            foreach (var (kind, pattern) in Rules)
            {
                if (pattern.IsMatch(lines[i]))
                {
                    violations.Add(new Violation(file, i + 1, kind, trimmed));
                }
            }
        }

        return violations;
    }

    /// <summary>Whether this exact line in this exact file carries a standing allowance.</summary>
    internal static bool IsAllowed(string file, string trimmedLine)
        => Allowances.Any(a
            => file.EndsWith(a.File, StringComparison.OrdinalIgnoreCase)
               && string.Equals(a.Line, trimmedLine, StringComparison.Ordinal));

    /// <summary>Scan every view XAML under the app directory (skipping the value dictionaries, the string
    /// files, and build output).</summary>
    public static IReadOnlyList<Violation> ScanConsumers(string appDirectory)
    {
        var violations = new List<Violation>();
        foreach (var rel in ConsumerPaths(appDirectory))
        {
            violations.AddRange(FindViolations(rel, File.ReadAllText(Path.Combine(appDirectory, rel))));
        }

        return violations;
    }

    /// <summary>
    /// The files this guard walks, relative to the app directory. Exposed so a test can assert WHAT was
    /// visited rather than only what came back: a guard that silently stops visiting the style dictionary
    /// reports zero violations and looks exactly like a clean one.
    /// </summary>
    public static IReadOnlyList<string> ConsumerPaths(string appDirectory)
    {
        var paths = new List<string>();
        foreach (var file in Directory.EnumerateFiles(appDirectory, "*.xaml", SearchOption.AllDirectories))
        {
            var rel = Path.GetRelativePath(appDirectory, file).Replace('\\', '/');
            if (!IsExcluded(rel))
            {
                paths.Add(rel);
            }
        }

        return paths;
    }

    /// <summary>
    /// The files that DEFINE design values. Everything else under the app - views and the style
    /// dictionaries alike - consumes them and is scanned.
    /// </summary>
    private static readonly string[] ValueDefinitions =
    [
        "Themes/Colours.xaml",
        "Themes/Values.xaml",
    ];

    private static bool IsExcluded(string relativePath)
        => ValueDefinitions.Contains(relativePath, StringComparer.OrdinalIgnoreCase)
           || relativePath.StartsWith("Localization/", StringComparison.OrdinalIgnoreCase)
           || relativePath.StartsWith("bin/", StringComparison.OrdinalIgnoreCase)
           || relativePath.StartsWith("obj/", StringComparison.OrdinalIgnoreCase)
           || relativePath.Contains("/bin/", StringComparison.OrdinalIgnoreCase)
           || relativePath.Contains("/obj/", StringComparison.OrdinalIgnoreCase);
}

using System.Globalization;
using System.Windows.Data;

namespace ChronoMock.App;

/// <summary>
/// The note a renderer row carries when the session reached the pages it hosts: a translated phrase in
/// brackets, or nothing at all.
/// </summary>
/// <remarks>
/// A converter rather than a second <c>Run</c> bound to a resource, because a Run cannot be hidden - the
/// pattern the audit chips already document. A false here has to produce an EMPTY string, not a blank
/// one: a stray space after the role would show as the column being one character wider on some rows
/// than on others.
///
/// It reads the translation through <see cref="TranslationKeyConverter"/> rather than holding English,
/// so the phrase follows the interface language like every other piece of text (untouchable rule 15).
/// </remarks>
public sealed class PagesReachedNoteConverter : IValueConverter
{
    /// <summary>The phrase, resolved from the key defined in both language files. The key sits at its
    /// point of use rather than in a named constant: the hygiene guard counts a definition referenced
    /// only from its own class as unreferenced, and a constant read once two lines below itself is a
    /// name that earns nothing.</summary>
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is true ? TranslationKeyConverter.Resolve("audit.role_pages_reached") : string.Empty;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException("The process table is read-only.");
}

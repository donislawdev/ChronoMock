namespace ChronoMock.App.Calc;

/// <summary>
/// Turns an engine failure into interface text.
///
/// <para>
/// The calculator runs <c>chrono calc</c> as a child process and gets its failures as the stderr sentence,
/// which is English by design - the CLI is English, without exception (rule 15). That sentence used to go
/// straight onto the panel, so a Polish window answered a mistyped date with
/// <c>chrono calc: unrecognised date format '31.02.2026' (calc.analyze_unrecognized)</c>: the wrong
/// language, the name of a process the user is not supposed to know runs, and a raw contract key. Exactly
/// one case out of nine was translated. This is the RELEASE-002 class, which the protocol side already
/// closed and the calculator side had not, because the calculator does not travel over the protocol.
/// </para>
/// <para>
/// The engine names its own failures: every one of them ends in <c>(calc.something)</c>, a stable key that
/// is part of the surface contract (docs/08 section 10) and does not move when the sentence is reworded.
/// So the key is what is read, never the prose around it - the same rule the missing-calendar check has
/// always followed.
/// </para>
/// <para>
/// 🔴 Not every failure the calculator surface can produce carries one. A bad moment, a preset that will
/// not resolve, and a missing calendar FILE come out of shared code that predates these keys, and they
/// arrive as a bare sentence. Those still show in English, and saying so is better than a mapping that
/// pretends otherwise (rule 6). What they no longer show is the <c>chrono calc:</c> prefix, which is a
/// process name leaking into an interface that has no processes in it.
/// </para>
/// </summary>
internal static class CalcErrorText
{
    /// <summary>The prefixes the CLI puts in front of its own diagnostics. They belong to a command line,
    /// not to a window, so they come off either way.</summary>
    private static readonly string[] Prefixes = ["chrono calc: ", "chrono: "];

    /// <summary>
    /// The interface text for an engine failure: the translation of its stable key when it has one, else
    /// the engine's own sentence with the command-line prefix removed.
    /// </summary>
    /// <param name="message">The engine's stderr, as <c>CalcException.Message</c> carries it.</param>
    /// <param name="translate">Key resolver. A resolver that does not know a key returns the key itself
    /// (the production fallback), which is what the "did this translate" test below is.</param>
    public static string Describe(string message, Func<string, string> translate)
    {
        ArgumentNullException.ThrowIfNull(translate);
        var raw = (message ?? string.Empty).Trim();

        if (KeyOf(raw) is { } key)
        {
            var translationKey = $"calc.err.{key["calc.".Length..]}";
            var text = translate(translationKey);
            // A resolver returns the key when it has no string for it, so an unmapped engine key must not
            // put "calc.err.something" on the panel - that is the jargon this exists to remove.
            if (!string.Equals(text, translationKey, StringComparison.Ordinal))
            {
                return text;
            }
        }

        return Detail(raw);
    }

    /// <summary>
    /// The engine's own sentence, minus the command-line prefix and the trailing key - shown BESIDE the
    /// translation, never instead of it.
    ///
    /// <para>
    /// It exists because the translation cannot carry everything. Most engine refusals name a step number
    /// or a year range, and a key alone reproduces neither. The two ways to keep that detail were to parse
    /// it back out of an English sentence - the very thing the key exists to avoid, and something a
    /// rewording would break silently - or to show the sentence as what it is: a technical detail under a
    /// message the reader can act on. This is the second.
    /// </para>
    /// <para>
    /// Empty when the sentence adds nothing the translation does not already say - there is no point
    /// repeating one line in two languages.
    /// </para>
    /// </summary>
    public static string Detail(string message) => StripPrefix(WithoutKeySuffix((message ?? string.Empty).Trim()));

    /// <summary>The stable key an engine sentence ends with (<c>calc.something</c>), or null when it
    /// carries none. Anchored on the trailing parenthesis, so a key mentioned mid-sentence - or a
    /// parenthesis that is part of the prose - is not mistaken for one.</summary>
    internal static string? KeyOf(string message)
    {
        var text = (message ?? string.Empty).TrimEnd();
        if (!text.EndsWith(')'))
        {
            return null;
        }

        var open = text.LastIndexOf('(');
        if (open < 0)
        {
            return null;
        }

        var key = text[(open + 1)..^1];
        return key.StartsWith("calc.", StringComparison.Ordinal) && IsKeyShaped(key) ? key : null;
    }

    /// <summary>A key is lowercase words joined by dots and underscores - the same shape the Rust-side
    /// guard checks. This is what keeps a parenthesised English aside from reading as a key.</summary>
    private static bool IsKeyShaped(string key)
        => key.Length > "calc.".Length
           && key.All(c => c is '.' or '_' or (>= 'a' and <= 'z'));

    private static string WithoutKeySuffix(string message)
    {
        if (KeyOf(message) is null)
        {
            return message;
        }

        var open = message.TrimEnd().LastIndexOf('(');
        return message[..open].TrimEnd();
    }

    private static string StripPrefix(string message)
    {
        foreach (var prefix in Prefixes)
        {
            if (message.StartsWith(prefix, StringComparison.Ordinal))
            {
                return message[prefix.Length..];
            }
        }

        return message;
    }
}

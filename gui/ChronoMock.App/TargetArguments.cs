namespace ChronoMock.App;

/// <summary>
/// Splits the arguments the tester typed for the target into the argument LIST the wire carries
/// (<c>target.args</c>).
/// <para>
/// This mirrors <c>split_args</c> in crates/cli/src/main.rs deliberately and exactly. The CLI takes the
/// same text through <c>--args</c>, and both end up in the same <c>CreateProcessW</c> call, so a rule
/// that differed by one character would mean the same string launching an application two different
/// ways depending on which surface the tester used - the kind of difference nobody reports as a bug
/// because they never run both.
/// </para>
/// <para>
/// The rule: a double quote toggles quoting and starts a token (so <c>""</c> is an empty argument);
/// whitespace outside quotes ends a token; anything else is part of it. Quotes are removed. It is not
/// the full <c>CommandLineToArgvW</c> grammar - backslash escaping is not honoured - and that is the
/// point: it matches what the core does, not what Windows does. The core re-quotes each argument on
/// the way out.
/// </para>
/// </summary>
internal static class TargetArguments
{
    public static IReadOnlyList<string> Split(string? raw)
    {
        var result = new List<string>();
        if (string.IsNullOrEmpty(raw))
        {
            return result;
        }

        var current = new System.Text.StringBuilder();
        var inQuotes = false;
        var hasToken = false;

        foreach (var c in raw)
        {
            if (c == '"')
            {
                inQuotes = !inQuotes;
                hasToken = true;
            }
            else if (char.IsWhiteSpace(c) && !inQuotes)
            {
                if (hasToken)
                {
                    result.Add(current.ToString());
                    current.Clear();
                    hasToken = false;
                }
            }
            else
            {
                current.Append(c);
                hasToken = true;
            }
        }

        if (hasToken)
        {
            result.Add(current.ToString());
        }

        return result;
    }
}

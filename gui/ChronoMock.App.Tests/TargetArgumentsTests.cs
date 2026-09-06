using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;

using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The panel and the CLI take the same text and must turn it into the same argument list. They cannot
/// share code - one is Rust, the other C# - so the cases below are lifted verbatim from the Rust unit
/// test of <c>split_args</c> in crates/cli/src/main.rs, and the last test here notices if that set ever
/// grows without this one following.
/// </summary>
public class TargetArgumentsTests
{
    [Fact]
    public void Plain_words_become_one_argument_each()
    {
        Assert.Equal(new[] { "a", "b", "c" }, TargetArguments.Split("a b c"));
    }

    [Fact]
    public void Runs_of_whitespace_do_not_produce_empty_arguments()
    {
        Assert.Equal(new[] { "x", "y" }, TargetArguments.Split("  x   y "));
    }

    [Fact]
    public void Numbers_are_arguments_like_anything_else()
    {
        Assert.Equal(new[] { "out", "100", "10" }, TargetArguments.Split("out 100 10"));
    }

    /// <summary>A quoted run keeps its spaces and arrives as ONE argument - the whole point of quoting.</summary>
    [Fact]
    public void A_quoted_run_stays_one_argument_and_loses_its_quotes()
    {
        Assert.Equal(new[] { "a b", "c" }, TargetArguments.Split("\"a b\" c"));
    }

    [Fact]
    public void Nothing_typed_means_no_arguments()
    {
        Assert.Empty(TargetArguments.Split(""));
        Assert.Empty(TargetArguments.Split(null));
        Assert.Empty(TargetArguments.Split("   "));
    }

    /// <summary>
    /// An explicitly empty argument. It survives, because a target that distinguishes "no argument" from
    /// "empty argument" would otherwise be unreachable from the panel while reachable from the CLI.
    /// </summary>
    [Fact]
    public void An_explicit_empty_argument_survives()
    {
        Assert.Equal(new[] { "" }, TargetArguments.Split("\"\""));
    }

    /// <summary>
    /// The mirror guard. Both surfaces feed the same CreateProcessW call, so a rule that drifted would
    /// mean one string launching an application two different ways depending on which surface was used -
    /// and nobody would report it, because nobody runs both with the same input.
    /// <para>
    /// This cannot compare behaviour across languages, so it compares COVERAGE: if the Rust test grows a
    /// case, this fails and someone has to decide whether C# needs it too.
    /// </para>
    /// </summary>
    [Fact]
    public void The_rust_split_has_no_cases_this_test_does_not_mirror()
    {
        var main = Path.Combine(TestPaths.RepoRoot(), "crates", "cli", "src", "main.rs");
        Assert.True(File.Exists(main), $"expected the CLI source at '{main}'");

        // Assertions inside the Rust unit test, not the two call sites in the parser.
        var cases = Regex.Matches(File.ReadAllText(main), @"split_args\(""")
            .Count
            + Regex.Matches(File.ReadAllText(main), @"split_args\(\\?""\\?""\)").Count;

        const int MirroredHere = 7; // "a b c", "  x   y ", "out 100 10", "\"a b\" c", "", "\"\"", plus the empty-check
        Assert.True(
            cases <= MirroredHere,
            $"crates/cli/src/main.rs now exercises split_args with {cases} literal inputs, but this file "
                + $"mirrors {MirroredHere}. Check whether the new case behaves the same in C# and add it here.");
    }
}

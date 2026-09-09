using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The one place this application hands an address to the Windows shell, and the two things that have to
/// stay true about it: the destination is the one the button promises, and a shell that refuses says so
/// instead of taking the window down with it.
/// </summary>
public class ExternalLinkTests
{
    private const string LinksFile = "ExternalLinks.cs";

    /// <summary>The call that takes a caller-supplied address, which only the links file may make.</summary>
    private const string OpenCall = "TryOpen(";

    [Fact]
    public void The_support_address_is_the_one_the_button_promises()
    {
        // Pinned rather than derived. The tooltip names this host to the user before they press anything,
        // so a typo here is a promise broken in the interface, not just a bad link.
        Assert.Equal("https://donislawdev.com/support/", ExternalLinks.Support);

        // https rather than http is the part worth asserting separately: handing the shell a plaintext
        // address would be this application's only insecure outbound step, and it would be invisible.
        Assert.StartsWith("https://", ExternalLinks.Support, StringComparison.Ordinal);
    }

    [Fact]
    public void An_address_the_shell_refuses_is_reported_rather_than_thrown()
    {
        // A real refusal rather than a stand-in. The path does not exist, so ShellExecute fails before it
        // ever looks for an association, which is deterministic and opens no window and no browser. This
        // is the path a machine with no browser associated with https takes, and the button depends on it
        // returning rather than throwing - an unhandled exception in a Click handler ends the process.
        var missing = Path.Combine(Path.GetTempPath(), $"chrono-mock-no-such-file-{Guid.NewGuid():N}.nothing");
        Assert.False(File.Exists(missing), "the test needs an address the shell cannot possibly take");

        Assert.False(ExternalLinks.TryOpen(missing));
    }

    [Fact]
    public void Nothing_outside_the_links_file_chooses_its_own_address()
    {
        // ShellExecute runs whatever string it is handed, so an address built anywhere else - from a
        // recent target, a dropped file, a preset - would be a way to start something. The type is shaped
        // to prevent it (callers ask for a named destination), and this keeps it that way after the next
        // person adds a second link.
        var offenders = FindOpenCalls(inLinksFile: false);

        Assert.True(
            offenders.Count == 0,
            "an address handed to the shell from outside " + LinksFile
                + " (add a named destination there instead):\n"
                + string.Join("\n", offenders.Select(o => $"  {o.File}:{o.Line}")));
    }

    [Fact]
    public void The_scan_actually_reaches_the_file_it_claims_to_cover()
    {
        // The canary. A walk that reaches no files, or a rename that makes the search term match nothing,
        // satisfies the assertion above perfectly and looks exactly like a guard that works. Three is the
        // count the links file actually has - its declaration and one call per named destination - so
        // losing the file or a destination is something somebody has to look at rather than inherit.
        Assert.Equal(3, FindOpenCalls(inLinksFile: true).Count);
    }

    private static IReadOnlyList<(string File, int Line)> FindOpenCalls(bool inLinksFile)
    {
        // The walk names a directory rather than files, because a guard that reads sources BY PATH goes
        // quiet the moment the code it guards moves.
        var app = TestPaths.AppDirectory();
        var hits = new List<(string, int)>();

        foreach (var file in Directory.EnumerateFiles(app, "*.cs", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(app, file).Replace('\\', '/');
            if (relative.StartsWith("bin/", StringComparison.OrdinalIgnoreCase)
                || relative.StartsWith("obj/", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            if (relative.EndsWith(LinksFile, StringComparison.Ordinal) != inLinksFile)
            {
                continue;
            }

            var lines = File.ReadAllLines(file);
            for (int i = 0; i < lines.Length; i++)
            {
                if (lines[i].Contains(OpenCall, StringComparison.Ordinal))
                {
                    hits.Add((relative, i + 1));
                }
            }
        }

        return hits;
    }
}

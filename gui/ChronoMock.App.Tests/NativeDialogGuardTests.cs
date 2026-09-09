using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App.Tests;

/// <summary>
/// Keeps the native message box where it was deliberately left, and out of everywhere else.
///
/// A native box is a light-grey window on a dark app, and worse, WINDOWS labels its buttons: an English
/// interface answers in the system language, and no translation key of ours is involved (rule 15). Every
/// question and notice of normal operation therefore goes through <c>Views.MessageDialog</c>.
///
/// The exception is App.xaml.cs, whose three dialogs report that the strings, the startup path or the
/// dispatcher have failed - which is to say, the very things a themed window needs in order to draw. A
/// dialog that cannot draw itself is a worse way to report a failure than a plain box that always can.
///
/// The scan walks the app directory rather than naming files, because a guard that reads sources BY PATH
/// goes quiet the moment the code moves.
/// </summary>
public class NativeDialogGuardTests
{
    private const string Allowed = "App.xaml.cs";
    private const string Native = "MessageBox.Show";

    private static IReadOnlyList<(string File, int Line)> FindNativeDialogs(bool inAllowedFile)
    {
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

            if (relative.EndsWith(Allowed, StringComparison.Ordinal) != inAllowedFile)
            {
                continue;
            }

            var lines = File.ReadAllLines(file);
            for (int i = 0; i < lines.Length; i++)
            {
                if (lines[i].Contains(Native, StringComparison.Ordinal))
                {
                    hits.Add((relative, i + 1));
                }
            }
        }

        return hits;
    }

    [Fact]
    public void Normal_operation_never_opens_a_native_message_box()
    {
        var hits = FindNativeDialogs(inAllowedFile: false);
        Assert.True(
            hits.Count == 0,
            "a native message box outside the last-resort dialogs (use Views.MessageDialog):\n"
                + string.Join("\n", hits.Select(h => $"  {h.File}:{h.Line}")));
    }

    [Fact]
    public void The_last_resort_dialogs_are_still_there_and_still_native()
    {
        // The canary this guard needs. A scan that reaches no files, or a rename that makes the search
        // term match nothing, satisfies the assertion above perfectly and looks exactly like a guard
        // that works. The literal three is the count these dialogs actually have, so losing one of them
        // silently, or growing a fourth, is a change somebody has to look at rather than inherit.
        var hits = FindNativeDialogs(inAllowedFile: true);
        Assert.Equal(3, hits.Count);
    }
}

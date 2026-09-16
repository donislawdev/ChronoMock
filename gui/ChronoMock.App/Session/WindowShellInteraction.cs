using System.Runtime.InteropServices;
using System.Windows;
using Microsoft.Win32;
using ChronoMock.App.Views;

namespace ChronoMock.App;

/// <summary>
/// The real <see cref="IShellInteraction"/>: it wraps the live window so the session's window-dependent
/// commands can open a picker, touch the clipboard, or ask a yes/no question. The window hands one to
/// <see cref="SessionCommands.AttachShell"/> once, so the view model never holds a UI type (GUI rules 15
/// and 16). The behaviour here is the released panel's, lifted from the code-behind handlers it replaces so
/// the pickers, the clipboard retry and the confirm dialog act exactly as they did.
/// </summary>
internal sealed class WindowShellInteraction : IShellInteraction
{
    private readonly Window _window;

    internal WindowShellInteraction(Window window) => _window = window;

    public string? PickExecutable()
    {
        var dialog = new OpenFileDialog
        {
            Title = Text("target.dialog_title"),
            Filter = Text("target.dialog_filter"),
            CheckFileExists = true,
        };

        return dialog.ShowDialog(_window) == true ? dialog.FileName : null;
    }

    public string? PickFolder(string? initialDirectory)
    {
        // Seeded with whatever is already typed, so browsing from a filled field starts where the tester was
        // rather than at the shell root. No Directory.Exists probe: that is filesystem I/O on the UI thread
        // and can block on a disconnected path, and the picker already falls back to its default location
        // when the seed does not resolve.
        var dialog = new OpenFolderDialog { Title = Text("launch.cwd_label") };
        if (!string.IsNullOrWhiteSpace(initialDirectory))
        {
            dialog.InitialDirectory = initialDirectory;
        }

        return dialog.ShowDialog(_window) == true ? dialog.FolderName : null;
    }

    public bool CopyToClipboard(string text)
    {
        // Another process can hold the clipboard lock - retry once, then report the failure rather than
        // swallowing it (rule 6).
        for (int attempt = 0; attempt < 2; attempt++)
        {
            try
            {
                Clipboard.SetText(text);
                return true;
            }
            catch (ExternalException)
            {
                // The lock was held by another process - one more try, then false.
            }
        }

        return false;
    }

    public bool Confirm(string headingKey, string messageKey, string affirmativeKey)
        => MessageDialog.Ask(_window, Text(headingKey), Text(messageKey), Text(affirmativeKey));

    public string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;
}

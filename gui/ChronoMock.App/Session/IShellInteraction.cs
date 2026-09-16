namespace ChronoMock.App;

/// <summary>
/// The window-dependent things a command sometimes has to do: open a picker, read or write the clipboard,
/// ask a yes/no question, resolve a translation key. They all need a live window as an owner or the running
/// application's resources, which is exactly what a view model must not hold (GUI rules 15 and 16), so they
/// sit behind this interface. The real one wraps the window (added when the window hosts the phases), a test
/// passes a fake and asserts what a command asked for without a UI thread.
/// <para>
/// A command reaches this only through <see cref="SessionCommands"/>, and only after the shell is attached.
/// When it is absent - a phase rendered for the state sheet, or a view model built in a test with no shell -
/// the command's CanExecute still follows its data gate (so the drawing matches the shipped one), and Execute
/// is a quiet no-op rather than a crash.
/// </para>
/// </summary>
public interface IShellInteraction
{
    /// <summary>Ask for an executable to run (a file picker). Null when the user cancelled.</summary>
    string? PickExecutable();

    /// <summary>Ask for a working folder (a folder picker), starting at <paramref name="initialDirectory"/>
    /// when it is a real path. Null when the user cancelled.</summary>
    string? PickFolder(string? initialDirectory);

    /// <summary>Put text on the clipboard. False when another process held it and it could not be set.</summary>
    bool CopyToClipboard(string text);

    /// <summary>Ask a destructive-action question with the effect spelled out and the affirmative named after
    /// what it does. True when confirmed. The arguments are translation keys, resolved by the shell.</summary>
    bool Confirm(string headingKey, string messageKey, string affirmativeKey);

    /// <summary>Resolve a translation key to text, for a summary built in the user's language.</summary>
    string Text(string key);
}

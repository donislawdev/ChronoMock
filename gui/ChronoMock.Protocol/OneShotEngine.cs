using System.Diagnostics;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text;

namespace ChronoMock.Protocol;

/// <summary>
/// Where the engine the window asks one-shot questions lives: the x64 core (a calculation, the catalogue and
/// the component register are pure computation, so bitness does not matter). One place for the path the
/// calculator, the preset catalogue and the licence query all use.
/// </summary>
public static class EnginePaths
{
    /// <summary>A dev checkout: the build named by its explicit target triple, like
    /// <see cref="CoreLocator.ForRepo"/> - <c>target/release/</c> is only written by a build without
    /// <c>--target</c>, so it goes stale silently (R2-X3).</summary>
    public static string Repo(string repoRoot)
        => Path.Combine(repoRoot, "target", "x86_64-pc-windows-msvc", "release", "chrono.exe");

    /// <summary>A dev checkout's <c>calendars/</c> and <c>presets/</c>: they live with the date library, in
    /// <c>crates/dates/catalogue</c>, so the package that goes to crates.io carries them too.</summary>
    public static string RepoCatalogue(string repoRoot)
        => Path.Combine(repoRoot, "crates", "dates", "catalogue");

    /// <summary>The shipped portable layout (Stage 5): the x64 core beside the window.</summary>
    public static string Portable(string baseDir) => Path.Combine(baseDir, "core", "x64", "chrono.exe");
}

/// <summary>What one run of the engine left behind: its exit code and both streams, read to the end.</summary>
internal sealed record EngineRun(int ExitCode, string Stdout, string Stderr);

/// <summary>The engine could not be started at all - a missing, quarantined or blocked executable. The
/// message names the file and the system's reason, which is what somebody fixing the install needs.</summary>
internal sealed class EngineLaunchException(string executable, string reason)
    : Exception($"cannot launch '{executable}': {reason}");

/// <summary>The engine ran past its limit and was killed - or could not be, which <see cref="Stopped"/> says.</summary>
internal sealed class EngineTimeoutException(TimeSpan limit, bool stopped)
    : Exception($"the engine did not finish within {limit.TotalSeconds:0} s")
{
    public bool Stopped { get; } = stopped;
}

/// <summary>
/// Runs the engine once as a child process and hands back what it said: the one way the window asks the
/// engine a question that is not a session - a calculation, the preset catalogue, the component register.
/// <para>
/// It was written out in full in <see cref="CalcClient"/> and again in <see cref="LicenseClient"/>, and
/// the preset catalogue would have been the third copy. The copies had already parted once: the licence
/// query redirected stderr without reading it, so a core that wrote more than a pipe buffer there stopped
/// on the write until the limit killed it (R4-N50). One place now, and the three clients decide only what
/// a failure means for their caller.
/// </para>
/// </summary>
internal static class OneShotEngine
{
    private static readonly UTF8Encoding Utf8NoBom = new(encoderShouldEmitUTF8Identifier: false);

    /// <summary>
    /// Run <paramref name="executable"/> with <paramref name="arguments"/>, in
    /// <paramref name="workingDirectory"/> when one is given, for at most <paramref name="limit"/>.
    /// Throws <see cref="EngineLaunchException"/> when it cannot be started,
    /// <see cref="EngineTimeoutException"/> when it ran past the limit, and
    /// <see cref="OperationCanceledException"/> when <paramref name="ct"/> was cancelled first - in the last
    /// two cases after killing it when it had started: <c>using</c> disposes the managed wrapper, NOT the
    /// running process, and a cancelled question used to leave the engine running, so a burst of typing left
    /// a pile of orphans.
    /// </summary>
    internal static Task<EngineRun> RunAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string? workingDirectory,
        TimeSpan limit,
        CancellationToken ct)
        => RunAsync(executable, arguments, workingDirectory, limit, static process => process.Start(), ct);

    /// <summary>
    /// The same, with <paramref name="start"/> as the step that starts the process: <see cref="Process.Start()"/>
    /// in the window, and in a test one that holds until the test lets it go, which is how a start made on
    /// the caller's thread shows - the call does not come back until it did.
    /// </summary>
    internal static async Task<EngineRun> RunAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string? workingDirectory,
        TimeSpan limit,
        Func<Process, bool> start,
        CancellationToken ct)
    {
        ArgumentNullException.ThrowIfNull(executable);
        ArgumentNullException.ThrowIfNull(arguments);
        ArgumentNullException.ThrowIfNull(start);

        var psi = new ProcessStartInfo
        {
            FileName = executable,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardOutputEncoding = Utf8NoBom,
            StandardErrorEncoding = Utf8NoBom,
        };
        foreach (var argument in arguments)
        {
            psi.ArgumentList.Add(argument);
        }

        if (workingDirectory is not null)
        {
            psi.WorkingDirectory = workingDirectory;
        }

        using var process = new Process { StartInfo = psi };

        // 🔴 Started on a pool thread, never on the caller's (R5-WY1). The caller is the window's thread, and
        // creating a process holds it for as long as the system takes - the loader, and an antivirus scanning
        // the engine before it may run, which on a machine with one is the larger part. The calculator asks
        // on every edit, so that was a pause on every edit. A question superseded before the pool got to it
        // starts nothing and comes back cancelled.
        await Task.Run(() => StartProcess(process, executable, start), ct).ConfigureAwait(false);

        using var attempt = CancellationTokenSource.CreateLinkedTokenSource(ct);
        attempt.CancelAfter(limit);

        // Both pipes are drained concurrently from the start, so a full buffer on either cannot hold the
        // engine on a write - the stderr of the licence query was redirected and never read (R4-N50). On
        // the attempt's token, so the readers stop with the limit too.
        var stdoutTask = process.StandardOutput.ReadToEndAsync(attempt.Token);
        var stderrTask = process.StandardError.ReadToEndAsync(attempt.Token);
        try
        {
            await process.WaitForExitAsync(attempt.Token).ConfigureAwait(false);
            var stdout = await stdoutTask.WaitAsync(attempt.Token).ConfigureAwait(false);
            var stderr = await stderrTask.WaitAsync(attempt.Token).ConfigureAwait(false);
            return new EngineRun(process.ExitCode, stdout, stderr);
        }
        catch (OperationCanceledException)
        {
            var stopped = ChildProcesses.KillQuietly(process);
            await ChildProcesses.ObserveQuietly(stdoutTask, stderrTask).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                throw; // the caller superseded this question - its own concern, not an error
            }

            throw new EngineTimeoutException(limit, stopped);
        }
        catch (IOException)
        {
            // A pipe that broke under the reader. Not a limit reached, so not said as one - the caller's
            // own handling of an I/O failure answers it, after the process is stopped.
            _ = ChildProcesses.KillQuietly(process);
            await ChildProcesses.ObserveQuietly(stdoutTask, stderrTask).ConfigureAwait(false);
            throw;
        }
    }

    /// <summary>Start the process, or throw <see cref="EngineLaunchException"/> naming the system's reason.</summary>
    private static void StartProcess(Process process, string executable, Func<Process, bool> start)
    {
        try
        {
            start(process);
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException
                                      or ObjectDisposedException or PlatformNotSupportedException)
        {
            // The system's own reason only: the process message repeats the path and adds a working
            // directory, which on the rendered panel made one sentence of four lines (R4-S26).
            var reason = e is System.ComponentModel.Win32Exception native
                ? new System.ComponentModel.Win32Exception(native.NativeErrorCode).Message
                : e.Message;
            throw new EngineLaunchException(executable, reason);
        }
    }
}

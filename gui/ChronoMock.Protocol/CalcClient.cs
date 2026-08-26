using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace ChronoMock.Protocol;

/// <summary>Raised when <c>chrono calc</c> exits non-zero: the calculator surface reports a usage or
/// build error on stderr and returns a result only on success (docs/08 section 9a). Carries the exit
/// code so a caller can tell a usage error (1) from a not-built-yet operation (5).</summary>
public sealed class CalcException : Exception
{
    public int ExitCode { get; }

    public CalcException(string message, int exitCode)
        : base(message) => ExitCode = exitCode;
}

/// <summary>
/// Runs the calculator engine as a one-shot child process (<c>chrono calc &lt;args&gt; --json</c>) and
/// parses its <c>chronomock.calc/1</c> output. The GUI is a thin client of the same engine the CLI and
/// the substitution core use (ADR-6), so the date logic lives in one place (chrono-core), never
/// re-implemented in C#. Unlike <see cref="CoreClient"/> there is no session: each call is independent.
/// </summary>
public sealed class CalcClient
{
    private static readonly UTF8Encoding Utf8NoBom = new(encoderShouldEmitUTF8Identifier: false);

    private readonly Func<string> _chronoPath;
    private readonly string? _workingDirectory;

    /// <param name="workingDirectory">Where calc looks for <c>calendars/</c> and <c>presets/</c> (it checks
    /// <c>./calendars</c> and <c>&lt;exe&gt;/calendars</c>). Set it so <c>--calendar</c> / <c>--preset</c> resolve.</param>
    public CalcClient(Func<string> chronoPath, string? workingDirectory = null)
    {
        _chronoPath = chronoPath ?? throw new ArgumentNullException(nameof(chronoPath));
        _workingDirectory = workingDirectory;
    }

    /// <summary>Dev-checkout factory: the host-default core build (the path the conformance tests and
    /// <see cref="CoreLocator"/> use for x64; calc is pure computation, so its bitness does not matter),
    /// run from the repo root so its <c>calendars/</c> and <c>presets/</c> resolve.</summary>
    public static CalcClient ForRepo(string repoRoot)
        => new(() => Path.Combine(repoRoot, "target", "release", "chrono.exe"), repoRoot);

    /// <summary>
    /// Evaluate a calc invocation. <paramref name="calcArgs"/> are the flags after <c>calc</c> (e.g.
    /// <c>--base</c>, <c>--shift</c>, <c>--calendar</c>, <c>--analyze</c>); <c>calc</c> and <c>--json</c>
    /// are added here. Throws <see cref="CalcException"/> on a non-zero exit (stderr as the message).
    /// </summary>
    public async Task<CalcResult> EvaluateAsync(IReadOnlyList<string> calcArgs, CancellationToken ct = default)
    {
        ArgumentNullException.ThrowIfNull(calcArgs);

        var exe = _chronoPath();
        var psi = new ProcessStartInfo
        {
            FileName = exe,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardOutputEncoding = Utf8NoBom,
            StandardErrorEncoding = Utf8NoBom,
        };
        psi.ArgumentList.Add("calc");
        foreach (var arg in calcArgs)
        {
            psi.ArgumentList.Add(arg);
        }

        psi.ArgumentList.Add("--json");
        if (_workingDirectory is not null)
        {
            psi.WorkingDirectory = _workingDirectory;
        }

        using var process = new Process { StartInfo = psi };
        try
        {
            process.Start();
        }
        catch (Exception e) when (e is not OperationCanceledException)
        {
            throw new CalcException($"cannot launch '{exe}': {e.Message}", -1);
        }

        // Drain both pipes concurrently before awaiting exit, so a full pipe buffer cannot deadlock.
        var stdoutTask = process.StandardOutput.ReadToEndAsync(ct);
        var stderrTask = process.StandardError.ReadToEndAsync(ct);
        await process.WaitForExitAsync(ct).ConfigureAwait(false);
        var stdout = await stdoutTask.ConfigureAwait(false);
        var stderr = await stderrTask.ConfigureAwait(false);

        if (process.ExitCode != 0)
        {
            var message = stderr.Trim();
            throw new CalcException(
                message.Length > 0 ? message : $"calc exited with code {process.ExitCode}", process.ExitCode);
        }

        CalcResult? result;
        try
        {
            result = JsonSerializer.Deserialize<CalcResult>(stdout, ProtocolJson.Options);
        }
        catch (JsonException e)
        {
            throw new CalcException($"calc output was not valid JSON: {e.Message}", 0);
        }

        return result ?? throw new CalcException("calc produced no JSON output", 0);
    }
}

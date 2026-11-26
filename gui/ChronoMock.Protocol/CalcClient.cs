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
public sealed class CalcClient : ICalcEngine
{
    /// <summary>How long one calc invocation may take before it is killed. Calc is pure computation and
    /// finishes in well under a second, so this is a safety net, not a budget: without it a core that
    /// cannot finish (a degenerate calendar used to spin forever - see the business-day walk) left the
    /// calculator frozen with no explanation and a CPU-burning orphan behind it.</summary>
    private static readonly TimeSpan CalcTimeout = TimeSpan.FromSeconds(10);

    private readonly Func<string> _chronoPath;
    private readonly string? _workingDirectory;

    /// <param name="workingDirectory">Where calc looks for <c>calendars/</c> and <c>presets/</c> (it checks
    /// <c>./calendars</c> and <c>&lt;exe&gt;/calendars</c>). Set it so <c>--calendar</c> / <c>--preset</c> resolve.</param>
    public CalcClient(Func<string> chronoPath, string? workingDirectory = null)
    {
        _chronoPath = chronoPath ?? throw new ArgumentNullException(nameof(chronoPath));
        _workingDirectory = workingDirectory;
    }

    /// <summary>Dev-checkout factory: the x64 core build (calc is pure computation, so its bitness does not
    /// matter), run from the checkout's catalogue folder so its <c>calendars/</c> and <c>presets/</c>
    /// resolve. Named by its explicit target triple, like <see cref="CoreLocator.ForRepo"/> -
    /// <c>target/release/</c> is only written by a build without <c>--target</c>, so it goes stale
    /// silently (R2-X3).</summary>
    public static CalcClient ForRepo(string repoRoot)
        => new(() => EnginePaths.Repo(repoRoot), EnginePaths.RepoCatalogue(repoRoot));

    /// <summary>Portable-install factory (the shipped layout, Stage 5): the x64 core at
    /// <paramref name="baseDir"/>/core/x64/chrono.exe (calc is pure computation, bitness does not matter),
    /// run from <paramref name="baseDir"/> so its root-level <c>calendars/</c> and <c>presets/</c> resolve
    /// via the <c>./</c> lookup.</summary>
    public static CalcClient ForPortable(string baseDir) => new(() => EnginePaths.Portable(baseDir), baseDir);

    /// <summary>
    /// Evaluate a calc invocation. <paramref name="calcArgs"/> are the flags after <c>calc</c> (e.g.
    /// <c>--base</c>, <c>--shift</c>, <c>--calendar</c>, <c>--analyze</c>) - <c>calc</c> and <c>--json</c>
    /// are added here. Throws <see cref="CalcException"/> on a non-zero exit (stderr as the message).
    /// </summary>
    public async Task<CalcResult> EvaluateAsync(IReadOnlyList<string> calcArgs, CancellationToken ct = default)
    {
        ArgumentNullException.ThrowIfNull(calcArgs);

        var stdout = await RunKeyedAsync(_chronoPath(), ["calc", .. calcArgs, "--json"], _workingDirectory, ct)
            .ConfigureAwait(false);

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

    /// <summary>
    /// The engine this client runs, asked the same question from a terminal. A shipped engine finds its
    /// calendars and presets by where it stands, so the command works from any folder. A development build's
    /// engine finds them through the folder it runs in, which this client sets and a pasted command does not -
    /// there it works from the repository root.
    /// </summary>
    public string? CommandLine(IReadOnlyList<string> calcArgs)
    {
        ArgumentNullException.ThrowIfNull(calcArgs);
        return PowerShellCommand.For(_chronoPath(), ["calc", .. calcArgs]);
    }

    /// <summary>
    /// Run one engine question and return its standard output, or throw <see cref="CalcException"/> carrying
    /// the engine's own sentence - or, for a launch that failed or a limit reached, a sentence in the engine's
    /// shape ending in a stable key (<c>calc.launch_failed</c>, <c>calc.timeout</c>), so the interface can
    /// translate it the way it translates the engine's refusals. This library knows nothing about languages
    /// and must not - the key is the seam that keeps it that way (rule 15). Shared by the calculator and the
    /// preset catalogue, which ask the same engine and fail the same ways.
    /// </summary>
    internal static async Task<string> RunKeyedAsync(
        string executable, IReadOnlyList<string> arguments, string? workingDirectory, CancellationToken ct)
    {
        EngineRun run;
        try
        {
            run = await OneShotEngine.RunAsync(executable, arguments, workingDirectory, CalcTimeout, ct)
                .ConfigureAwait(false);
        }
        catch (EngineLaunchException e)
        {
            // With the path, which is the part somebody fixing the install needs (R4-S26).
            throw new CalcException($"{e.Message} (calc.launch_failed)", -1);
        }
        catch (EngineTimeoutException e)
        {
            // Whether the process is really gone: a kill the system refused leaves it running, and "stopped"
            // would not be true.
            var outcome = e.Stopped ? "was stopped" : "could not be stopped";
            throw new CalcException(
                $"calc did not finish within {CalcTimeout.TotalSeconds:0} s and {outcome} (calc.timeout)", -1);
        }

        if (run.ExitCode != 0)
        {
            var message = run.Stderr.Trim();
            throw new CalcException(
                message.Length > 0 ? message : $"calc exited with code {run.ExitCode}", run.ExitCode);
        }

        return run.Stdout;
    }
}

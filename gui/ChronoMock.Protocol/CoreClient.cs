using System.Collections.Concurrent;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using System.Threading.Channels;

namespace ChronoMock.Protocol;

/// <summary>
/// Drives one core process over the machine protocol (ADR-6): spawns <c>chrono __core</c>, sends the
/// <c>start</c> command first, then relays the event stream.
/// <para>
/// Start-first is deliberate and mirrors the core: <c>core_mode</c> reads the <c>start</c> line before it
/// emits <c>ready</c>, so a client that waited for <c>ready</c> before sending <c>start</c> would deadlock.
/// (docs/08 section 3 describes the opposite order - that is a doc-vs-code drift; the code is the contract.)
/// </para>
/// The GUI is a client of this protocol, not FFI, so it stays AnyCPU and lets the core match the target's bitness.
/// </summary>
public sealed class CoreClient : IAsyncDisposable
{
    private static readonly UTF8Encoding Utf8NoBom = new(encoderShouldEmitUTF8Identifier: false);

    /// <summary>How many diagnostic lines are kept. The core's stderr is unbounded in principle (a chatty
    /// target, a long session), and the whole queue is later joined into one string for the diagnostics
    /// block, so it is capped and the OLDEST lines go - a failure is explained by what happened last.</summary>
    private const int MaxDiagnostics = 2000;

    private readonly Process _process;
    private readonly Channel<ChronoEvent> _events =
        Channel.CreateUnbounded<ChronoEvent>(new UnboundedChannelOptions { SingleWriter = true });
    private readonly ConcurrentQueue<string> _diagnostics = new();
    private int _diagnosticsDropped;
    private readonly object _stdinLock = new();
    private readonly Task _readLoop;
    private readonly Task _stderrDrain;
    private int _disposed;

    private CoreClient(Process process)
    {
        _process = process;
        _readLoop = Task.Run(ReadEventsAsync);
        _stderrDrain = Task.Run(DrainStderrAsync);
    }

    /// <summary>The core's event stream. Completes when the core closes its stdout (it exited).</summary>
    public ChannelReader<ChronoEvent> Events => _events.Reader;

    /// <summary>
    /// Human-side diagnostics: the core's stderr lines and any per-line parse errors. Never on the
    /// protocol path - surfaced for logging, never parsed. Draining stderr on its own task also keeps a
    /// full stderr pipe buffer from deadlocking the core.
    /// </summary>
    public IReadOnlyCollection<string> Diagnostics => _diagnostics;

    /// <summary>
    /// Spawn the core WITHOUT sending a command, so the client can gate on <c>ready</c> - check the protocol
    /// version and bitness (<see cref="HandshakeGate"/>) - before it commits to launching the target. The core
    /// emits <c>ready</c> before it reads its first command (docs/08 section 3, fixed in 97eae17), so awaiting
    /// <c>ready</c> here cannot deadlock.
    /// </summary>
    public static CoreClient Connect(string coreExePath)
    {
        ArgumentException.ThrowIfNullOrEmpty(coreExePath);

        var psi = new ProcessStartInfo
        {
            FileName = coreExePath,
            ArgumentList = { "__core" },
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardInputEncoding = Utf8NoBom,
            StandardOutputEncoding = Utf8NoBom,
            StandardErrorEncoding = Utf8NoBom,
        };

        var process = Process.Start(psi)
            ?? throw new InvalidOperationException($"failed to start core '{coreExePath}'");
        return new CoreClient(process);
    }

    /// <summary>
    /// Spawn the core and send <c>start</c> immediately (start-first). A convenience over <see cref="Connect"/>
    /// for callers that do not gate on <c>ready</c> first - the conformance tests use it.
    /// </summary>
    public static CoreClient Launch(string coreExePath, StartCommand start)
    {
        ArgumentNullException.ThrowIfNull(start);
        var client = Connect(coreExePath);
        try
        {
            client.Send(start);
        }
        catch
        {
            // The core died right after spawning, so Send threw. Dispose the client we just created before
            // the exception propagates - the reference never escapes this method, so otherwise its process,
            // handles, and read tasks would leak (L-12).
            client.DisposeAsync().AsTask().GetAwaiter().GetResult();
            throw;
        }

        return client;
    }

    /// <summary>Send a command as one NDJSON line. The core reads one line per command.</summary>
    public void Send(Command command)
    {
        ArgumentNullException.ThrowIfNull(command);
        // One documented exception after dispose, thrown here rather than surfacing from deep inside a
        // closed stream - callers that guard on a running session (the GUI's in-flight controls) can race
        // Dispose, and a predictable type is what lets them catch it.
        ObjectDisposedException.ThrowIf(Volatile.Read(ref _disposed) != 0, this);
        SendLine(command);
    }

    /// <summary>
    /// Write one command without the disposed guard, for the ONE caller that must still be able to
    /// speak after the guard has closed: <see cref="DisposeAsync"/>, whose whole job starts by asking
    /// the core to end.
    /// <para>
    /// This split is not decoration. <c>_disposed</c> carries two jobs - the idempotency latch for
    /// dispose, and the guard on the public API - and routing shutdown through the public
    /// <see cref="Send"/> made the second job veto the first: the latch was already set, so the `end`
    /// command threw <see cref="ObjectDisposedException"/> and took the whole graceful path with it
    /// (R2-K1). Keep the two jobs apart.
    /// </para>
    /// </summary>
    private void SendLine(Command command)
    {
        var line = command.ToNdjson();
        lock (_stdinLock)
        {
            var writer = _process.StandardInput;
            // Explicit '\n' (not Environment.NewLine); the core trims line endings on its side either way.
            writer.Write(line);
            writer.Write('\n');
            writer.Flush();
        }
    }

    /// <summary>
    /// Wait for the core process to exit and return its exit code - the session verdict (docs/08 section 8).
    /// </summary>
    public async Task<int> WaitForExitAsync(CancellationToken cancellationToken = default)
    {
        await _process.WaitForExitAsync(cancellationToken).ConfigureAwait(false);
        return _process.ExitCode;
    }

    private async Task ReadEventsAsync()
    {
        try
        {
            var stdout = _process.StandardOutput;
            string? line;
            while ((line = await stdout.ReadLineAsync().ConfigureAwait(false)) is not null)
            {
                if (line.Length == 0)
                {
                    continue;
                }

                ChronoEvent? evt;
                try
                {
                    evt = EventParser.Parse(line);
                }
                catch (JsonException ex)
                {
                    AddDiagnostic($"parse error: {ex.Message} :: {line}");
                    continue;
                }

                if (evt is not null)
                {
                    await _events.Writer.WriteAsync(evt).ConfigureAwait(false);
                }
            }
        }
        finally
        {
            _events.Writer.TryComplete();
        }
    }

    private async Task DrainStderrAsync()
    {
        var stderr = _process.StandardError;
        string? line;
        while ((line = await stderr.ReadLineAsync().ConfigureAwait(false)) is not null)
        {
            AddDiagnostic($"core stderr: {line}");
        }
    }

    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        try
        {
            if (!_process.HasExited)
            {
                // Graceful: end the session, then close stdin so the core sees EOF and shuts down.
                // SendLine, not Send: the disposed latch is already set above, and the public guard would
                // reject our own shutdown command (R2-K1).
                try
                {
                    SendLine(new EndCommand { Id = 0 });
                }
                catch (Exception ex) when (ex is IOException or ObjectDisposedException)
                {
                    // Core already gone, or its stdin stream is closed - nothing to end.
                }

                try
                {
                    _process.StandardInput.Close();
                }
                catch (Exception ex) when (ex is IOException or ObjectDisposedException)
                {
                    // Stream already closed.
                }

                // Give the core a moment to end cleanly, then take down just the core (not the target
                // tree). The hook self-detaches when the core dies (plasterek 10), so the target reverts
                // to real time on its own - we never kill the application under test.
                using var grace = new CancellationTokenSource(TimeSpan.FromSeconds(2));
                try
                {
                    await _process.WaitForExitAsync(grace.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    // Did not end within the grace period: take down just the core, never the target tree.
                    _process.Kill(entireProcessTree: false);
                }
            }
        }
        catch (InvalidOperationException)
        {
            // Process exited between the HasExited check and here - fine.
        }

        // The core is gone. Complete the event stream NOW instead of waiting for the read loop to see EOF
        // on stdout. That EOF does not arrive when the core dies: the target holds the write end of the
        // core's stdout pipe, so it lands only once the APPLICATION UNDER TEST exits - which after a Stop
        // is whenever the tester happens to close it. Measured on this path: the core exited 21 ms after
        // `end`, and the read loop stayed parked until the target was killed, to the millisecond.
        //
        // Consumers gate "the session is over" on this stream (the GUI keeps showing "stopping" until it
        // ends, then falls back to its 15 s idle watchdog), so the target's lifetime must not be what
        // decides when a stopped session looks stopped. Already-written events stay readable - completing
        // a channel closes it to WRITERS, not to a reader draining what is left.
        _events.Writer.TryComplete();

        // Bounded join for the same reason: the read loop can be parked on ReadLineAsync for as long as
        // the target holds that pipe, and dispose must not inherit the target's lifetime. Disposing the
        // process below closes the stream underneath it either way.
        await AwaitQuietly(_readLoop, JoinTimeout).ConfigureAwait(false);
        await AwaitQuietly(_stderrDrain, JoinTimeout).ConfigureAwait(false);
        _process.Dispose();
    }

    /// <summary>Append one diagnostic line, dropping the oldest past the cap and leaving a single marker
    /// so a truncated block never reads as a complete one.</summary>
    private void AddDiagnostic(string line)
    {
        _diagnostics.Enqueue(line);
        while (_diagnostics.Count > MaxDiagnostics && _diagnostics.TryDequeue(out _))
        {
            if (Interlocked.Exchange(ref _diagnosticsDropped, 1) == 0)
            {
                _diagnostics.Enqueue($"[older diagnostics dropped - keeping the last {MaxDiagnostics} lines]");
            }
        }
    }

    /// <summary>How long dispose waits to join a background reader before giving up on it. The read loop
    /// can be parked on a pipe the target still holds, so this is a bound on OUR shutdown, not on the
    /// reader - the process dispose that follows closes the stream underneath it.</summary>
    private static readonly TimeSpan JoinTimeout = TimeSpan.FromSeconds(2);

    private async Task AwaitQuietly(Task task, TimeSpan? timeout = null)
    {
        try
        {
            if (timeout is { } limit)
            {
                var finished = await Task.WhenAny(task, Task.Delay(limit)).ConfigureAwait(false);
                if (!ReferenceEquals(finished, task))
                {
                    AddDiagnostic($"background task did not finish within {limit.TotalSeconds:0.#}s");
                    return;
                }
            }

            await task.ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            AddDiagnostic($"background task: {ex.Message}");
        }
    }
}

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

    private readonly Process _process;
    private readonly Channel<ChronoEvent> _events =
        Channel.CreateUnbounded<ChronoEvent>(new UnboundedChannelOptions { SingleWriter = true });
    private readonly ConcurrentQueue<string> _diagnostics = new();
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
        client.Send(start);
        return client;
    }

    /// <summary>Send a command as one NDJSON line. The core reads one line per command.</summary>
    public void Send(Command command)
    {
        ArgumentNullException.ThrowIfNull(command);
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
                    _diagnostics.Enqueue($"parse error: {ex.Message} :: {line}");
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
            _diagnostics.Enqueue($"core stderr: {line}");
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
                try
                {
                    Send(new EndCommand { Id = 0 });
                }
                catch (IOException)
                {
                    // Core already gone - nothing to end.
                }

                try
                {
                    _process.StandardInput.Close();
                }
                catch (IOException)
                {
                    // Stream already closed.
                }

                // Give the core a moment to end cleanly, then take down just the core (not the target
                // tree). The hook self-detaches when the core dies (plasterek 10), so the target reverts
                // to real time on its own - we never kill the application under test.
                if (!_process.WaitForExit(2000))
                {
                    _process.Kill(entireProcessTree: false);
                }
            }
        }
        catch (InvalidOperationException)
        {
            // Process exited between the HasExited check and here - fine.
        }

        await AwaitQuietly(_readLoop).ConfigureAwait(false);
        await AwaitQuietly(_stderrDrain).ConfigureAwait(false);
        _process.Dispose();
    }

    private async Task AwaitQuietly(Task task)
    {
        try
        {
            await task.ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            _diagnostics.Enqueue($"background task: {ex.Message}");
        }
    }
}

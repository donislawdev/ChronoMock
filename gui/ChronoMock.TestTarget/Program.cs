// Chrono Mock conformance test target: a quiet, long-lived process that reads a covered wall-clock
// channel (DateTime.UtcNow -> GetSystemTimeAsFileTime) so the core's verdict is `works`. It must
// outlive the ADR-4 guard window and several 1 s heartbeats, so it runs for a few seconds. It writes
// nothing to stdout, to avoid polluting anything the core might inherit.
using System.Diagnostics;

var stopwatch = Stopwatch.StartNew();
while (stopwatch.Elapsed < TimeSpan.FromSeconds(5))
{
    // Read a wall-clock channel the core covers. The getter has an observable side effect (it calls the
    // hooked export), so the JIT cannot elide it even though the value is discarded.
    _ = DateTime.UtcNow;
    Thread.Sleep(100);
}

return 0;

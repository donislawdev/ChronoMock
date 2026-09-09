using System.Collections.Concurrent;
using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// The diagnostics buffer is capped, so a chatty target cannot grow it without bound - and a capped
/// buffer has to be able to say it is a TAIL.
///
/// 🔴 It used to say so with a marker LINE, enqueued once when the first drop happened. Enqueue appends
/// and the trimming dequeues from the front, so after another cap's worth of lines the marker reached
/// the front and was dropped itself - and the flag guarding it was already set, so it never came back.
/// The block then read as complete again, which is exactly what the marker existed to prevent. A count
/// cannot fall out of the queue, and it says HOW MANY lines went, which the marker never could.
/// </summary>
public sealed class DiagnosticsBufferTests
{
    private static ConcurrentQueue<string> QueueOf(int count)
    {
        var q = new ConcurrentQueue<string>();
        for (var i = 0; i < count; i++)
        {
            q.Enqueue($"line {i}");
        }

        return q;
    }

    [Fact]
    public void Nothing_is_dropped_while_the_buffer_is_within_its_cap()
    {
        var q = QueueOf(5);

        Assert.Equal(0, CoreClient.TrimToCap(q, 10));
        Assert.Equal(5, q.Count);
    }

    [Fact]
    public void The_oldest_lines_go_first_and_the_count_says_how_many()
    {
        var q = QueueOf(12);

        Assert.Equal(2, CoreClient.TrimToCap(q, 10));
        Assert.Equal(10, q.Count);
        Assert.True(q.TryPeek(out var oldest));
        Assert.Equal("line 2", oldest); // 0 and 1 went, so what is left really is the tail
    }

    /// <summary>
    /// The regression itself: keep pushing past the cap and the record of what was lost has to survive.
    /// A marker line does not - it ages out like any other entry. A running total does.
    /// </summary>
    [Fact]
    public void The_record_of_what_was_dropped_survives_far_past_the_cap()
    {
        var q = new ConcurrentQueue<string>();
        var dropped = 0;
        for (var i = 0; i < 500; i++)
        {
            q.Enqueue($"line {i}");
            dropped += CoreClient.TrimToCap(q, 10);
        }

        Assert.Equal(10, q.Count);
        Assert.Equal(490, dropped);
    }
}

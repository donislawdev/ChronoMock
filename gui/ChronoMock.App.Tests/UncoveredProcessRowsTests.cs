using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The fold behind the process table: the children a family verdict names in, one ordered list of counted
/// rows out. Pure, so it is tested without a binding or a window.
/// </summary>
public class UncoveredProcessRowsTests
{
    private static UncoveredChild Child(uint pid, string? image, string? role = null)
        => new() { Pid = pid, ParentPid = 1, Image = image, Role = role };

    [Fact]
    public void One_row_per_executable_and_role_counted_with_the_renderer_leading_its_executable()
    {
        var rows = UncoveredProcessRowsConverter.Fold(
        [
            Child(10, "engine.exe", "utility"),
            Child(11, "engine.exe", "gpu-process"),
            Child(12, "engine.exe", "utility"),
            Child(13, "engine.exe", "renderer"),
        ]);

        Assert.Equal(
            [("engine.exe", "renderer", 1), ("engine.exe", "utility", 2), ("engine.exe", "gpu-process", 1)],
            rows.Select(r => (r.Image, r.Role, r.Count)));
        Assert.True(rows[0].IsRenderer);
        Assert.All(rows.Skip(1), r => Assert.False(r.IsRenderer));
        Assert.All(rows, r => Assert.False(r.IsUnnamed));
    }

    [Fact]
    public void Executables_are_ordered_by_what_they_tell_renderer_first_then_by_size_then_by_name()
    {
        var rows = UncoveredProcessRowsConverter.Fold(
        [
            Child(10, "b-helper.exe"),
            Child(11, "a-helper.exe"),
            Child(12, "big.exe"),
            Child(13, "big.exe"),
            Child(14, "pages.exe", "renderer"),
        ]);

        // The one that hosted a renderer first even though it is the smallest, then the largest, then the
        // two single ones by name - and a row of one executable never interleaves with another's.
        Assert.Equal(["pages.exe", "big.exe", "a-helper.exe", "b-helper.exe"], rows.Select(r => r.Image));
        Assert.Equal([1, 2, 1, 1], rows.Select(r => r.Count));
    }

    [Fact]
    public void Two_spellings_of_one_executable_are_one_executable_in_the_spelling_seen_first()
    {
        var rows = UncoveredProcessRowsConverter.Fold(
        [
            Child(10, "Engine.exe", "utility"),
            Child(11, "engine.exe", "utility"),
            Child(12, "ENGINE.EXE", "utility"),
        ]);

        var row = Assert.Single(rows);
        Assert.Equal("Engine.exe", row.Image);
        Assert.Equal(3, row.Count);
    }

    [Fact]
    public void The_children_gone_before_they_could_be_named_are_one_row_last_and_a_missing_role_is_empty()
    {
        var rows = UncoveredProcessRowsConverter.Fold(
        [
            Child(10, null),
            Child(11, "helper.exe"),
            Child(12, null, "renderer"),
        ]);

        Assert.Equal(2, rows.Count);
        Assert.Equal("helper.exe", rows[0].Image);
        Assert.Equal(string.Empty, rows[0].Role);
        Assert.False(rows[0].IsUnnamed);
        // A child with no image has no name to group under, whatever else it carried - the role of a
        // process nobody can name is not a row of its own.
        Assert.True(rows[1].IsUnnamed);
        Assert.Equal(2, rows[1].Count);
        Assert.False(rows[1].IsRenderer);
        Assert.Equal(string.Empty, rows[1].Image);
    }

    [Fact]
    public void A_role_is_matched_exactly_and_a_count_prints_in_invariant_digits()
    {
        var rows = UncoveredProcessRowsConverter.Fold(
            Enumerable.Range(0, 1234).Select(i => Child((uint)i, "many.exe", i % 2 == 0 ? "renderer" : "Renderer")).ToList());

        // "Renderer" is not the token the engine writes, so it is a role of its own and not the one in the
        // failure ink.
        Assert.Equal(2, rows.Count);
        Assert.Equal(("renderer", true, "617"), (rows[0].Role, rows[0].IsRenderer, rows[0].CountText));
        Assert.Equal(("Renderer", false, "617"), (rows[1].Role, rows[1].IsRenderer, rows[1].CountText));
    }

    [Fact]
    public void A_renderer_whose_engine_the_session_reached_is_told_apart_from_one_it_did_not()
    {
        // 🔴 THE ROW USED TO SAY THE SAME THING IN BOTH CASES, and one of them was wrong. A renderer the
        // session could not reach means the application's pages read the real clock. A renderer whose
        // ENGINE was reached means its pages ran on the session clock and only its own native reads did
        // not - and the screen painted both in the failure ink, contradicting the family verdict beside
        // it, which calls that case partial.
        //
        // The join is the parent: the core records the hooked process that spawned each uncovered child,
        // and a renderer is spawned by the browser process that holds the debugging port. Measured on
        // three recorded sessions across two engines before this was written.
        //
        // Reversal probe: pass no engines and both rows come back PagesReached false.
        var rows = UncoveredProcessRowsConverter.Fold(
            [Child(10, "engine.exe", "renderer"), Child(11, "engine.exe", "renderer")],
            // The pid the Child helper records as the parent - that is the join being exercised.
            [new ReachedEngine { Pid = 1, Port = 61868, Browser = "Engine/1.0" }]);

        // Both children name that pid as their parent, so both were reached.
        var row = Assert.Single(rows);
        Assert.True(row.PagesReached);
        Assert.Equal(2, row.Count);
    }

    [Fact]
    public void Two_renderers_of_one_executable_split_when_only_one_engine_was_reached()
    {
        // Counted into one row they would put a number against a claim true of only half of it.
        var rows = UncoveredProcessRowsConverter.Fold(
            [
                new UncoveredChild { Pid = 10, ParentPid = 4242, Image = "engine.exe", Role = "renderer" },
                new UncoveredChild { Pid = 11, ParentPid = 9999, Image = "engine.exe", Role = "renderer" },
            ],
            [new ReachedEngine { Pid = 4242, Port = 61868, Browser = "Engine/1.0" }]);

        Assert.Equal(2, rows.Count);
        // The one still on the real clock leads: it is the row that changes the answer.
        Assert.False(rows[0].PagesReached);
        Assert.True(rows[1].PagesReached);
        Assert.All(rows, r => Assert.True(r.IsRenderer));
    }

    [Fact]
    public void Only_a_renderer_is_said_to_have_had_its_pages_reached()
    {
        // 🔴 THIS GUARD EXISTS BECAUSE THE RENDER CAUGHT WHAT THESE TESTS DID NOT. Every helper of a
        // reached engine shares its parent pid - the crash handler, the GPU process, the utilities - so
        // a join on the parent alone marked all of them, and the screen told the reader that a crash
        // handler's pages had been reached. It has no pages. The tests passed because they only ever
        // fed this renderers.
        //
        // Reversal probe: drop the role check from the fold and this fails on the first helper.
        var rows = UncoveredProcessRowsConverter.Fold(
            [
                Child(10, "engine.exe", "renderer"),
                Child(11, "engine.exe", "gpu-process"),
                Child(12, "engine.exe", "crashpad-handler"),
                Child(13, "engine.exe", "utility"),
            ],
            [new ReachedEngine { Pid = 1, Port = 61868, Browser = "Engine/1.0" }]);

        Assert.True(rows.Single(r => r.Role == "renderer").PagesReached);
        Assert.All(rows.Where(r => r.Role != "renderer"), r => Assert.False(r.PagesReached));
    }

    [Fact]
    public void With_no_engines_reached_every_row_reads_as_it_always_did()
    {
        // The reversal probe as a test of its own: a session that reached nothing must produce exactly
        // the table this screen showed before any of this existed.
        var rows = UncoveredProcessRowsConverter.Fold([Child(10, "engine.exe", "renderer")]);

        Assert.False(Assert.Single(rows).PagesReached);
    }

    [Fact]
    public void An_empty_list_folds_to_no_rows()
    {
        Assert.Empty(UncoveredProcessRowsConverter.Fold([]));
    }

    [Fact]
    public void An_unset_binding_value_folds_to_no_rows_rather_than_throwing()
    {
        // A view rendered without a data context hands the converter DependencyProperty.UnsetValue.
        var converter = new UncoveredProcessRowsConverter();

        var result = converter.Convert(
            [System.Windows.DependencyProperty.UnsetValue, System.Windows.DependencyProperty.UnsetValue],
            typeof(object), null, System.Globalization.CultureInfo.InvariantCulture);

        Assert.Empty(Assert.IsAssignableFrom<IEnumerable<UncoveredProcessRow>>(result));
    }
}

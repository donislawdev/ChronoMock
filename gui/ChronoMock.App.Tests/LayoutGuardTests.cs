using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using ChronoMock.App;
using ChronoMock.App.Views;

namespace ChronoMock.App.Tests;

/// <summary>
/// Holds both screens to the layout facts that are wrong under any taste, on a real arrange pass.
/// </summary>
/// <remarks>
/// Everything else the suite knows about the interface is a DECLARATION - that a margin comes from the
/// closed scale, that a brush comes from the palette, that a binding is spelled right. None of that is a
/// result. This is the first guard that reads where things actually ended up.
///
/// Each rule carries a probe that builds a deliberately broken tree and watches the rule fire. Without
/// them a rule that reports nothing is indistinguishable from a rule that cannot report.
/// </remarks>
public class LayoutGuardTests
{
    /// <summary>
    /// Floors, as LITERALS rather than counts taken from the same walk. A guard that measures its own
    /// input agrees with itself perfectly over an empty tree.
    ///
    /// Set well under what the screens measure today (557 and 281) on purpose: this catches the failure
    /// that matters, which is a walk coming back with nothing because the layout pass did not happen. A
    /// floor pinned to today's exact shape would go red on every honest redesign and teach the reader to
    /// raise it without looking.
    /// </summary>
    private const int PanelElementsAtLeast = 200;

    /// <summary>See <see cref="PanelElementsAtLeast"/>.</summary>
    private const int CalculatorElementsAtLeast = 100;

    [Fact]
    public void The_substitution_panel_places_every_visible_element_somewhere_reachable()
    {
        var found = WpfTestHost.InvokeSettled(() => Inspect((FrameworkElement)new MainWindow().Content));

        Assert.True(
            found.Elements.Count >= PanelElementsAtLeast,
            $"the walk found only {found.Elements.Count} elements, so the layout pass did not happen and "
                + "every rule below was about to pass over nothing");
        Assert.Empty(found.Complaints);
    }

    [Fact]
    public void The_calculator_places_every_visible_element_somewhere_reachable()
    {
        var found = WpfTestHost.InvokeSettled(() => Inspect(new CalculatorView()));

        Assert.True(
            found.Elements.Count >= CalculatorElementsAtLeast,
            $"the walk found only {found.Elements.Count} elements, so the layout pass did not happen and "
                + "every rule below was about to pass over nothing");
        Assert.Empty(found.Complaints);
    }

    [Fact]
    public void The_off_surface_rule_fires_on_content_placed_off_the_surface()
    {
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var canvas = new Canvas();
            var stray = new TextBlock { Text = "off the edge", Width = 80, Height = 20 };
            Canvas.SetLeft(stray, 2000);
            canvas.Children.Add(stray);
            LayoutProbe.Settle(canvas, 200, 200);
            return LayoutRules.OutsideTheSurface(LayoutProbe.Walk(canvas), new Size(200, 200));
        });

        Assert.NotEmpty(complaints);
    }

    [Fact]
    public void The_off_surface_rule_stays_quiet_when_an_ancestor_scrolls_to_it()
    {
        // The exemption that keeps the rule usable, asserted rather than trusted: the same stray content
        // inside a scroll viewer is content below the fold, and the panel has 27 such elements.
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var tall = new StackPanel();
            tall.Children.Add(new TextBlock { Text = "below the fold", Width = 80, Height = 900 });
            var viewer = new ScrollViewer { Content = tall };
            LayoutProbe.Settle(viewer, 200, 200);
            return LayoutRules.OutsideTheSurface(LayoutProbe.Walk(viewer), new Size(200, 200));
        });

        Assert.Empty(complaints);
    }

    [Fact]
    public void The_arranged_to_nothing_rule_fires_on_visible_text_squeezed_to_zero()
    {
        var (complaints, dump) = WpfTestHost.InvokeSettled(() =>
        {
            // 🔴 What this probe had to be told by measurement, twice. Neither Width = 0 nor
            // MaxWidth = 0 on the TextBlock produces the condition: WPF arranged it 91 wide in both
            // cases, and a zero-width PARENT did not shrink it either - the text simply overflowed the
            // container and went on painting. So the reach of this rule is narrower than it first
            // looks, and saying so is the point: it catches an element whose TRANSFORM takes it to
            // nothing, not one squeezed by its container.
            var grid = new Grid();
            grid.Children.Add(new TextBlock
            {
                Text = "invisible words",
                LayoutTransform = new ScaleTransform(0, 1),
            });
            LayoutProbe.Settle(grid, 200, 200);
            var walk = LayoutProbe.Walk(grid);
            return (LayoutRules.ArrangedToNothing(walk), LayoutReport.Describe(walk));
        });

        Assert.True(complaints.Count > 0, "the rule saw nothing wrong in:\n" + dump);
    }

    [Fact]
    public void The_arranged_to_nothing_rule_stays_quiet_on_text_that_is_merely_collapsed()
    {
        // 217 of the panel's elements are in exactly this state at startup. Without the visibility
        // filter this rule would report a working interface, which is how a guard becomes noise and then
        // gets switched off.
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var grid = new Grid();
            grid.Children.Add(new TextBlock { Text = "switched off", Visibility = Visibility.Collapsed });
            LayoutProbe.Settle(grid, 200, 200);
            return LayoutRules.ArrangedToNothing(LayoutProbe.Walk(grid));
        });

        Assert.Empty(complaints);
    }

    [Fact]
    public void The_hard_clip_rule_fires_on_content_pushed_past_a_clipping_ancestor()
    {
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var canvas = new Canvas { Width = 100, Height = 100, ClipToBounds = true };
            var stray = new TextBlock { Text = "clipped away", Width = 40, Height = 20 };
            Canvas.SetLeft(stray, 300);
            canvas.Children.Add(stray);
            var host = new Grid { Width = 400, Height = 400 };
            host.Children.Add(canvas);
            LayoutProbe.Settle(host, 400, 400);
            return LayoutRules.PastAHardClip(LayoutProbe.Walk(host));
        });

        Assert.NotEmpty(complaints);
    }

    [Fact]
    public void Neither_screen_gives_way_at_any_window_size_the_user_can_drag_it_to()
    {
        var report = WpfTestHost.InvokeSettled(() =>
        {
            var complaints = new List<string>();
            foreach (var size in SizeSweep.Sizes)
            {
                complaints.AddRange(SizeSweep.Inspect((FrameworkElement)new MainWindow().Content, size)
                    .Select(c => $"panel at {SizeSweep.Describe(size)}: {c}"));
                complaints.AddRange(SizeSweep.Inspect(new CalculatorView(), size)
                    .Select(c => $"calculator at {SizeSweep.Describe(size)}: {c}"));
            }

            return complaints;
        });

        // The canary: a sweep that stopped sweeping reports nothing, exactly like a clean one.
        Assert.True(SizeSweep.Sizes.Count >= SizesAtLeast, $"only {SizeSweep.Sizes.Count} sizes swept");
        Assert.Empty(report);
    }

    /// <summary>Five today: the window floor, its declared size, two wider, one tall and narrow.</summary>
    private const int SizesAtLeast = 4;

    [Fact]
    public void The_truncation_rule_fires_on_a_label_given_less_room_than_its_words_need()
    {
        var (complaints, dump) = WpfTestHost.InvokeSettled(() =>
        {
            const string Long = "a sentence far longer than sixty pixels of room";
            var host = new StackPanel();

            var byColumn = new Grid();
            byColumn.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(60) });
            byColumn.Children.Add(new TextBlock { Text = Long, TextTrimming = TextTrimming.CharacterEllipsis });
            host.Children.Add(byColumn);

            LayoutProbe.Settle(host, 400, 200);
            var walk = LayoutProbe.Walk(host);
            return (LayoutRules.TrimmedAway(walk), LayoutReport.Describe(walk));
        });

        Assert.True(complaints.Count > 0, "the rule saw nothing trimmed in:\n" + dump);
    }

    [Fact]
    public void The_spill_rule_fires_on_text_wider_than_the_box_it_was_put_in()
    {
        // The exact shape measured while the truncation probe refused to fire: a Grid declared 60 wide
        // came back holding a TextBlock 285 wide. WPF let it overflow rather than squeezing it, which
        // is how text ends up painted over its neighbour.
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var narrow = new Grid { Width = 60 };
            narrow.Children.Add(new TextBlock
            {
                Text = "a sentence far longer than sixty pixels of room",
                TextWrapping = TextWrapping.NoWrap,
            });
            var host = new Grid();
            host.Children.Add(narrow);
            LayoutProbe.Settle(host, 400, 200);
            return LayoutRules.SpillsOutOfItsParent(LayoutProbe.Walk(host));
        });

        Assert.NotEmpty(complaints);
    }

    [Fact]
    public void The_truncation_rule_stays_quiet_on_text_that_is_allowed_to_wrap()
    {
        // Wrapping text is WIDER than its box by design, and reporting it would bury the real findings.
        var complaints = WpfTestHost.InvokeSettled(() =>
        {
            var narrow = new Grid { Width = 60 };
            narrow.Children.Add(new TextBlock
            {
                Text = "a sentence far longer than sixty pixels of room",
                TextWrapping = TextWrapping.Wrap,
            });
            var host = new Grid();
            host.Children.Add(narrow);
            LayoutProbe.Settle(host, 400, 200);
            return LayoutRules.TrimmedAway(LayoutProbe.Walk(host));
        });

        Assert.Empty(complaints);
    }

    [Fact]
    public void Every_gap_we_laid_out_ourselves_comes_from_the_closed_spacing_scale()
    {
        var (offScale, measured) = WpfTestHost.InvokeSettled(() =>
        {
            var gaps = Gaps((FrameworkElement)new MainWindow().Content)
                .Concat(Gaps(new CalculatorView()))
                .ToList();

            return (gaps.Where(g => g.OffScale && g.Ours)
                .Select(g => $"{g.Size}px under {g.Where} between {g.Before} and {g.After}")
                .ToList(), gaps.Count);
        });

        // Measured today: 52 gaps on the panel and 42 on the calculator, none of ours off the scale.
        Assert.True(measured >= GapsAtLeast, $"only {measured} gaps were measured across both screens");
        Assert.Empty(offScale);
    }

    /// <summary>Well under the 94 measured today, because the floor is here to catch a walk that stopped.</summary>
    private const int GapsAtLeast = 40;

    [Fact]
    public void The_spacing_rule_fires_on_a_gap_that_is_not_on_the_scale()
    {
        var gaps = WpfTestHost.InvokeSettled(() =>
        {
            var panel = new StackPanel();
            panel.Children.Add(new Border { Height = 20, Width = 40, Background = Brushes.Gray });
            panel.Children.Add(new Border
            {
                Height = 20,
                Width = 40,
                Margin = new Thickness(0, 7, 0, 0),
                Background = Brushes.Gray,
            });
            LayoutProbe.Settle(panel, 200, 200);
            return SpacingReport.Measure(LayoutProbe.Walk(panel));
        });

        Assert.Contains(gaps, g => g.OffScale && Math.Abs(g.Size - 7) < 0.5);
    }

    [Fact]
    public void The_ownership_split_answers_both_ways()
    {
        // A canary in BOTH directions. A classifier that answers "not ours" to everything would make the
        // gate above pass over every gap on the screen, and look exactly like a clean interface.
        var (ours, theirs) = WpfTestHost.InvokeSettled(() =>
        {
            static IReadOnlyList<SpacingReport.Gap> Two(string name)
            {
                var panel = new StackPanel { Name = name };
                panel.Children.Add(new Border { Height = 20, Width = 40 });
                panel.Children.Add(new Border { Height = 20, Width = 40, Margin = new Thickness(0, 7, 0, 0) });
                LayoutProbe.Settle(panel, 200, 200);
                return SpacingReport.Measure(LayoutProbe.Walk(panel));
            }

            // TargetBox is declared in MainWindow.xaml. ThisNameIsNowhere is not.
            return (Two("TargetBox"), Two("ThisNameIsNowhere"));
        });

        Assert.Contains(ours, g => g.Ours);
        Assert.DoesNotContain(theirs, g => g.Ours);
    }

    [Fact]
    public void Every_line_of_text_reaches_its_contrast_floor_on_the_surface_it_is_painted_on()
    {
        var faint = WpfTestHost.InvokeSettled(() =>
            ReadText((FrameworkElement)new MainWindow().Content)
                .Concat(ReadText(new CalculatorView()))
                .Where(r => r.TooFaint)
                .Select(r => $"{r.Label} at {r.FontSize}px reads {r.Ratio} against {r.Paper}, needs {r.Required}")
                .ToList());

        Assert.Empty(faint);
    }

    [Fact]
    public void Every_type_size_in_our_own_text_comes_from_the_declared_scale()
    {
        var off = WpfTestHost.InvokeSettled(() =>
            ReadText((FrameworkElement)new MainWindow().Content)
                .Concat(ReadText(new CalculatorView()))
                .Where(r => r.OffScale)
                .Select(r => $"{r.Label} uses {r.FontSize}px, which is not on the scale")
                .ToList());

        Assert.Empty(off);
    }

    [Fact]
    public void The_contrast_rule_fires_on_text_too_faint_to_read()
    {
        var readings = WpfTestHost.InvokeSettled(() =>
        {
            var grid = new Grid { Background = new SolidColorBrush(Color.FromRgb(0x1B, 0x1B, 0x1F)) };
            grid.Children.Add(new TextBlock
            {
                Text = "barely there",
                FontSize = 14,
                Foreground = new SolidColorBrush(Color.FromRgb(0x2A, 0x2A, 0x2E)),
            });
            LayoutProbe.Settle(grid, 200, 200);
            return ContrastReport.Measure(grid, LayoutProbe.Walk(grid), 200, 200);
        });

        Assert.Contains(readings, r => r.TooFaint);
    }

    [Fact]
    public void The_type_scale_rule_fires_on_a_size_nobody_declared()
    {
        var readings = WpfTestHost.InvokeSettled(() =>
        {
            var grid = new Grid { Background = new SolidColorBrush(Color.FromRgb(0x1B, 0x1B, 0x1F)) };
            grid.Children.Add(new TextBlock
            {
                Text = "thirteen",
                FontSize = 13,
                Foreground = new SolidColorBrush(Colors.White),
            });
            LayoutProbe.Settle(grid, 200, 200);
            return ContrastReport.Measure(grid, LayoutProbe.Walk(grid), 200, 200);
        });

        Assert.Contains(readings, r => r.OffScale);
    }

    /// <summary>
    /// Every line of text on a screen, measured against the pixels it was painted on.
    /// </summary>
    /// <remarks>
    /// The floor is a LITERAL for the same reason every other canary here carries one: a screen that
    /// suddenly yields three readings has stopped being measured, and a rule over three readings is
    /// green for a reason that has nothing to do with the interface being right.
    /// </remarks>
    private static IReadOnlyList<SpacingReport.Gap> Gaps(FrameworkElement root)
    {
        LayoutProbe.Settle(root);
        return SpacingReport.Measure(LayoutProbe.Walk(root));
    }

    private static IReadOnlyList<ContrastReport.Reading> ReadText(FrameworkElement root)
    {
        LayoutProbe.Settle(root);
        var readings = ContrastReport.Measure(
            root, LayoutProbe.Walk(root), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);

        Assert.True(
            readings.Count >= TextReadingsAtLeast,
            $"only {readings.Count} pieces of text were measured, so this screen was not really read");
        return readings;
    }

    /// <summary>Measured today: 49 on the panel and 29 on the calculator. The floor is well under both.</summary>
    private const int TextReadingsAtLeast = 20;

    private static (IReadOnlyList<LaidOutElement> Elements, IReadOnlyList<string> Complaints) Inspect(
        FrameworkElement root)
    {
        LayoutProbe.Settle(root);
        var elements = LayoutProbe.Walk(root);
        var surface = new Size(LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);

        var complaints = LayoutRules.OutsideTheSurface(elements, surface)
            .Concat(LayoutRules.ArrangedToNothing(elements))
            .Concat(LayoutRules.PastAHardClip(elements))
            .ToList();

        return (elements, complaints);
    }
}

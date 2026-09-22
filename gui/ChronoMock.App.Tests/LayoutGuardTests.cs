using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using ChronoMock.App;
using ChronoMock.App.Views;
using ChronoMock.Protocol;

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

            // SubstitutionContainer is declared in MainWindow.xaml. ThisNameIsNowhere is not.
            return (Two("SubstitutionContainer"), Two("ThisNameIsNowhere"));
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
    /// Text painted in its own surface's colour is too faint, not unknown.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS WAS A HOLE. A line whose ink sat within 32 of the sampled surface was reported as having no
    /// readable background, so invisible text - the worst contrast there is - never reddened. Found when a
    /// probe painted the notes of both rebuilt phases in the card's colour and every guard stayed green.
    ///
    /// Reversal probe: make PaperBehind return null whenever its most common colour sits within
    /// InkConfusionDistance of the ink, which is what the rule used to do, and this reddens.
    /// </remarks>
    [Fact]
    public void The_contrast_rule_fires_on_text_painted_in_its_own_surface_colour()
    {
        var readings = WpfTestHost.InvokeSettled(() =>
        {
            var surface = Color.FromRgb(0x1B, 0x1B, 0x1F);
            var grid = new Grid { Background = new SolidColorBrush(surface) };
            grid.Children.Add(new TextBlock
            {
                Text = "nobody can read this",
                FontSize = 14,
                Foreground = new SolidColorBrush(surface),
            });
            LayoutProbe.Settle(grid, 200, 200);
            return ContrastReport.Measure(grid, LayoutProbe.Walk(grid), 200, 200);
        });

        Assert.Contains(readings, r => r.TooFaint);
    }

    /// <summary>
    /// A line whose rectangle is mostly ink is read against the surface showing through it, not failed.
    /// </summary>
    /// <remarks>
    /// The case the old exemption existed for. Closing the hole above must not turn every heavy glyph into a
    /// finding, so a box whose most common colour is the ink looks for the next colour that is not.
    /// </remarks>
    [Fact]
    public void The_contrast_rule_reads_a_box_of_mostly_ink_against_the_surface_showing_through_it()
    {
        var readings = WpfTestHost.InvokeSettled(() =>
        {
            var grid = new Grid { Background = new SolidColorBrush(Color.FromRgb(0x1B, 0x1B, 0x1F)) };
            grid.Children.Add(new TextBlock
            {
                Text = "███",
                FontSize = 24,
                LineHeight = 24,
                LineStackingStrategy = LineStackingStrategy.BlockLineHeight,
                // A quarter of the box below the glyphs is surface whatever the font draws the blocks with.
                // Without it the blocks filled nearly the whole box, and a box with no surface in it is
                // rightly read as text nobody can see - which is not the case this canary is for.
                Padding = new Thickness(0, 0, 0, 8),
                Foreground = new SolidColorBrush(Colors.White),
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
            });
            LayoutProbe.Settle(grid, 200, 200);
            return ContrastReport.Measure(grid, LayoutProbe.Walk(grid), 200, 200);
        });

        var block = Assert.Single(readings);
        Assert.True(block.BackgroundKnown, "the box was read as having no surface at all");
        Assert.False(block.TooFaint, $"white on near black read {block.Ratio} against {block.Paper}");
    }

    /// <summary>
    /// What an alignment leaves over in a grid cell is not a gap anybody laid out.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS WAS THE NOISE. The rule measured between the bounds of neighbours, so a shorter label in a
    /// shared column read as its margin plus the difference in width, and a right-aligned summary read as
    /// hundreds of pixels.
    ///
    /// Built the way PartFormRow is: one grid per row, sharing a label column. A single grid of two rows was
    /// the first attempt and measured nothing at all - see the note on SpacingReport.Along - so the gap the
    /// rule must still read is asserted as well as the leftover it must not.
    ///
    /// Reversal probe: make LaidOutGap measure between Bounds instead of between slots and this reddens.
    /// </remarks>
    [Fact]
    public void The_spacing_rule_ignores_what_an_alignment_leaves_over_in_a_grid_cell()
    {
        var gaps = WpfTestHost.InvokeSettled(() =>
        {
            var form = new StackPanel();
            Grid.SetIsSharedSizeScope(form, true);
            form.Children.Add(Row("Speed"));
            form.Children.Add(Row("A much longer label than the first"));

            LayoutProbe.Settle(form, 400, 200);
            return SpacingReport.Measure(LayoutProbe.Walk(form));

            static Grid Row(string label)
            {
                var row = new Grid();
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto, SharedSizeGroup = "Label" });
                row.ColumnDefinitions.Add(new ColumnDefinition());
                AddCell(row, new TextBlock
                {
                    Text = label,
                    Margin = new Thickness(0, 0, 16, 0),
                    HorizontalAlignment = HorizontalAlignment.Left,
                    VerticalAlignment = VerticalAlignment.Center,
                }, 0, 0);
                AddCell(row, new Border { Width = 40, Height = 32, HorizontalAlignment = HorizontalAlignment.Left }, 0, 1);
                return row;
            }
        });

        Assert.Contains(gaps, g => !g.Vertical && Math.Abs(g.Size - 16) < 0.5);
        Assert.DoesNotContain(gaps, g => g.OffScale);
    }

    /// <summary>
    /// A fixed track between two peers is a gap somebody laid out, and it is still read.
    /// </summary>
    /// <remarks>
    /// The other side of the rule above: measuring only margins would pass a spacer column of any width, and
    /// the channel between the two clocks is exactly such a column.
    /// </remarks>
    [Fact]
    public void The_spacing_rule_reads_a_fixed_track_between_two_peers()
    {
        var gaps = WpfTestHost.InvokeSettled(() =>
        {
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition());
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(7) });
            grid.ColumnDefinitions.Add(new ColumnDefinition());
            AddCell(grid, new Border { Height = 20 }, 0, 0);
            AddCell(grid, new Border { Height = 20 }, 0, 2);
            LayoutProbe.Settle(grid, 200, 100);
            return SpacingReport.Measure(LayoutProbe.Walk(grid));
        });

        Assert.Contains(gaps, g => g.OffScale && Math.Abs(g.Size - 7) < 0.5);
    }

    private static void AddCell(Grid grid, UIElement child, int row, int column)
    {
        Grid.SetRow(child, row);
        Grid.SetColumn(child, column);
        grid.Children.Add(child);
    }

    /// <summary>
    /// Every line of text on a screen, measured against the pixels it was painted on.
    /// </summary>
    /// <remarks>
    /// The floor is a LITERAL for the same reason every other canary here carries one: a screen that
    /// suddenly yields three readings has stopped being measured, and a rule over three readings is
    /// green for a reason that has nothing to do with the interface being right.
    /// </remarks>
    private static IReadOnlyList<SpacingReport.Gap> Gaps(FrameworkElement root, int height = LayoutProbe.WindowHeight)
    {
        LayoutProbe.Settle(root, LayoutProbe.WindowWidth, height);
        return SpacingReport.Measure(LayoutProbe.Walk(root));
    }

    private static IReadOnlyList<ContrastReport.Reading> ReadText(
        FrameworkElement root, int height = LayoutProbe.WindowHeight, int textAtLeast = TextReadingsAtLeast)
    {
        LayoutProbe.Settle(root, LayoutProbe.WindowWidth, height);
        var readings = ContrastReport.Measure(root, LayoutProbe.Walk(root), LayoutProbe.WindowWidth, height);

        Assert.True(
            readings.Count >= textAtLeast,
            $"only {readings.Count} pieces of text were measured, so this screen was not really read");
        return readings;
    }

    /// <summary>Measured today: 49 on the panel and 29 on the calculator. The floor is well under both.</summary>
    private const int TextReadingsAtLeast = 20;

    /// <summary>Measured today: 16 in the sparsest state of the rebuilt phases - a session that did not take
    /// effect, with no controls and no audit left to read - and more than twice that in the others. Its own
    /// floor, because the panel's would call that honest state unread.</summary>
    private const int PhaseTextReadingsAtLeast = 8;

    /// <summary>
    /// Every line of text in the rebuilt phases reaches its contrast floor, in every state the sheet draws.
    /// </summary>
    /// <remarks>
    /// 🔴 THE PHASES WERE NEVER READ. The rule above walks the shipped panel and the calculator, and the
    /// setup and session phases were built, reviewed and accepted without passing through it once.
    ///
    /// Reversal probe: give PartNote the card surface as its ink in Themes/Parts.xaml and this reddens on
    /// the notes, while the rule above stays green because the shipped screens do not load the parts library.
    /// </remarks>
    [Fact]
    public void Every_line_of_text_in_the_phases_reaches_its_contrast_floor_in_every_state()
    {
        var faint = WpfTestHost.InvokeSettled(() =>
        {
            var found = new List<string>();
            foreach (var (name, view) in PhaseStatesOnCanvas())
            {
                var readings = ReadText(view, PhaseCanvasHeight, PhaseTextReadingsAtLeast);
                AssertNothingLeftToScroll(name, view);
                found.AddRange(readings
                    .Where(r => r.TooFaint)
                    .Select(r => $"{name}: {r.Label} \"{r.Text}\" at {r.FontSize}px reads {r.Ratio} against {r.Paper}, needs {r.Required}"));
            }

            return found;
        });

        AssertNoFindings(faint);
    }

    /// <summary>
    /// Every type size in the rebuilt phases comes from the declared scale, in every state the sheet draws.
    /// </summary>
    /// <remarks>
    /// Reversal probe: set PartNote's FontSize to 13 in Themes/Parts.xaml and this reddens on the notes.
    /// </remarks>
    [Fact]
    public void Every_type_size_in_the_phases_comes_from_the_declared_scale_in_every_state()
    {
        var off = WpfTestHost.InvokeSettled(() =>
        {
            var found = new List<string>();
            foreach (var (name, view) in PhaseStatesOnCanvas())
            {
                var readings = ReadText(view, PhaseCanvasHeight, PhaseTextReadingsAtLeast);
                AssertNothingLeftToScroll(name, view);
                found.AddRange(readings
                    .Where(r => r.OffScale)
                    .Select(r => $"{name}: {r.Label} \"{r.Text}\" uses {r.FontSize}px, which is not on the scale"));
            }

            return found;
        });

        AssertNoFindings(off);
    }

    /// <summary>
    /// Fails with every finding written out.
    /// </summary>
    /// <remarks>
    /// 🔴 NOT Assert.Empty, which prints the first five items and an ellipsis. Across ten states the items it
    /// hides are whole states, so its first run here showed the counts of five and said nothing about the
    /// other five.
    /// </remarks>
    private static void AssertNoFindings(IReadOnlyCollection<string> findings)
    {
        if (findings.Count > 0)
        {
            Assert.Fail(string.Join(Environment.NewLine, findings));
        }
    }

    /// <summary>
    /// Every gap laid out in the rebuilt phases comes from the closed spacing scale, in every state the
    /// sheet draws.
    /// </summary>
    /// <remarks>
    /// The floor of OUR gaps is asserted per state, as a literal. Summed across ten states, one state whose
    /// walk found nothing would hide behind the other nine - which is exactly the shape of the combined
    /// floor on the rule above.
    ///
    /// Reversal probe: make SectionBodyGap 0,11,0,0 in Themes/Values.xaml and this reddens on the sections.
    /// </remarks>
    [Fact]
    public void Every_gap_we_laid_out_in_the_phases_comes_from_the_closed_spacing_scale_in_every_state()
    {
        var (offScale, thin) = WpfTestHost.InvokeSettled(() =>
        {
            var off = new List<string>();
            var few = new List<string>();
            foreach (var (name, view) in PhaseStatesOnCanvas())
            {
                var gaps = Gaps(view, PhaseCanvasHeight);
                AssertNothingLeftToScroll(name, view);

                var ours = gaps.Count(g => g.Ours);
                if (ours < OurGapsPerPhaseStateAtLeast)
                {
                    few.Add($"{name}: {ours} of our gaps among {gaps.Count}");
                }

                off.AddRange(gaps
                    .Where(g => g.OffScale && g.Ours)
                    .Select(g => $"{name}: {g.Size}px under {g.Where} between {g.Before} and {g.After}"));
            }

            return (off, few);
        });

        AssertNoFindings([.. thin, .. offScale]);
    }

    /// <summary>Measured today: 10 of our gaps in the smallest state (a result that did not start - one card,
    /// one sentence, one folded section), 11 in the next (a result that did not take effect), 26 in the
    /// smallest session state and 43 in the largest setup state. The floor sits at the sparsest honest state
    /// rather than under half of it: the two result states are that sparse because they have that little to
    /// say, and a floor above them would call an honest screen unread.</summary>
    private const int OurGapsPerPhaseStateAtLeast = 10;

    /// <summary>
    /// The canvas both phases are read on: the window's width, and one step of the 1 024 grid the catalogue
    /// uses for height.
    /// </summary>
    /// <remarks>
    /// 🔴 TALL, BECAUSE A READING NEEDS PIXELS. The contrast rule samples the surface behind each line from
    /// the render, and a line below the canvas has no surface to sample - it is skipped as unknown rather
    /// than failed. At the window's 800 px a phase with a section open keeps much of itself under the fold,
    /// so a guard run at the window's size would read the top of each state and pass over the rest. Every
    /// state asserts that its scroll area has nothing left to scroll, so a state that outgrows this reddens
    /// instead of going unread.
    /// </remarks>
    private const int PhaseCanvasHeight = 2048;

    /// <summary>
    /// A session that is over offers no control the reader cannot use, and a live one still has its controls.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. An ended session kept the whole control card on screen,
    /// disabled, with a Stop that could stop nothing.
    ///
    /// The running state is asserted as well, because "no disabled control is visible" is also true of a
    /// screen that hides its controls in every state.
    ///
    /// Reversal probe: take the Visibility binding off ControlsCard and StopButton in SessionPhaseView.xaml and
    /// this reddens on both ended states.
    /// </remarks>
    [Fact]
    public void A_session_that_is_over_offers_no_control_it_cannot_use()
    {
        var (dead, live) = WpfTestHost.InvokeSettled(() =>
        {
            var found = new List<string>();
            foreach (var (name, model) in new[]
            {
                ("session ended", SessionStates.Ended()),
                ("session that did not take effect", SessionStates.TargetVanished()),
            })
            {
                var view = new SessionPhaseView { DataContext = PhaseStates.WithTarget(model) };
                LayoutProbe.Settle(view);
                found.AddRange(LayoutProbe.Walk(view)
                    .Where(e => e.IsVisible && !e.IsEnabled && e.Kind is nameof(Button) or nameof(TextBox))
                    .Select(e => $"{name}: {e.Kind} \"{e.Text}\" is shown and cannot be used"));
            }

            var running = new SessionPhaseView { DataContext = PhaseStates.WithTarget(SessionStates.Running()) };
            LayoutProbe.Settle(running);
            var pressable = LayoutProbe.Walk(running).Count(e => e.IsVisible && e.IsEnabled && e.Kind == nameof(Button));
            return (found, pressable);
        });

        AssertNoFindings(dead);
        Assert.True(live >= LiveSessionButtonsAtLeast, $"a running session shows only {live} buttons it can press");
    }

    /// <summary>Speed presets, Set, the jumps and Stop come to eleven on a running session. A literal under that,
    /// so a view hiding its controls in every state cannot pass the half above by showing none.</summary>
    private const int LiveSessionButtonsAtLeast = 8;

    /// <summary>
    /// A session that ended before any report says so, instead of promising one that will not come.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. The place for the audit kept saying the clocks would be
    /// listed once the tool had checked, under a session whose application had already vanished.
    ///
    /// Reversal probe: make IsAuditPending in SessionViewModel ignore whether the session is over, which is what
    /// the sentence used to be bound to, and this reddens.
    /// </remarks>
    [Fact]
    public void A_session_that_ended_without_a_report_says_so_instead_of_promising_one()
    {
        var (pendingWhenOver, neverWhenOver, pendingWhenLive) = WpfTestHost.InvokeSettled(() =>
        {
            var gone = new SessionPhaseView { DataContext = PhaseStates.WithTarget(SessionStates.TargetVanished()) };
            LayoutProbe.Settle(gone);
            var over = LayoutProbe.Walk(gone);

            var running = new SessionPhaseView { DataContext = PhaseStates.WithTarget(SessionStates.Running()) };
            LayoutProbe.Settle(running);
            var live = LayoutProbe.Walk(running);

            return (
                over.Single(e => e.Name == "AuditPending").IsVisible,
                over.Single(e => e.Name == "AuditNeverArrived").IsVisible,
                live.Single(e => e.Name == "AuditPending").IsVisible);
        });

        Assert.False(pendingWhenOver, "a session that is over still promises a report");
        Assert.True(neverWhenOver, "a session that ended without a report does not say it will not come");
        Assert.True(pendingWhenLive, "a running session no longer says its report is on the way");
    }

    /// <summary>
    /// The form "Set up again" lands on says which field it could not fill. The note used to live on the
    /// result screen only, and Set up again now leaves that screen for the form - so the sentence has to be
    /// on the form, visible, in the reader's words.
    /// </summary>
    /// <remarks>
    /// Measured on the render, because a count of rendered elements stays positive with the note removed or
    /// either of its bindings broken (a review of the sheet test said as much). Reversal probe: drop
    /// HistoryLoadNote from the setup footer, or bind its Text to a key that does not exist, and this reddens.
    /// </remarks>
    [Fact]
    public void The_form_set_up_again_lands_on_names_the_field_it_could_not_fill()
    {
        var (visible, text) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new SetupPhaseView { DataContext = PhaseStates.SetupAfterRepeatWithMissingZone() };
            LayoutProbe.Settle(view);
            var note = LayoutProbe.Walk(view).Single(e => e.Name == "HistoryLoadNote");
            return (note.IsVisible, note.Text);
        });

        Assert.True(visible, "the note about the zone the load could not fill is not on the form");
        Assert.Equal(TranslationKeyConverter.Resolve("history.load_zone_missing"), text);
    }

    /// <summary>The rebuilt phases in every state the sheet draws, with the section that holds the state opened.</summary>
    /// <remarks>Lazy on purpose: every view is created inside the caller's dispatcher call.</remarks>
    private static IEnumerable<(string Name, FrameworkElement View)> PhaseStatesOnCanvas()
    {
        yield return ("setup at startup", SetupView(PhaseStates.SetupStartup()));
        yield return ("setup with the scenarios open", SetupView(PhaseStates.SetupStartup(), "ScenarioSection"));
        yield return ("setup searching for nothing", SetupView(PhaseStates.SetupSearchingForNothing(), "ScenarioSection"));
        yield return ("setup with every option", SetupView(PhaseStates.SetupWithEveryOption(), "SpeedSection"));
        yield return ("setup with a bad date", SetupView(PhaseStates.SetupWithBadDate()));
        yield return ("setup configured", SetupView(PhaseStates.SetupConfigured()));
        yield return ("session running", SessionView(SessionStates.Running()));
        yield return ("session with the audit open", SessionView(SessionStates.RunningWithCoverageWarnings(), "AuditSection"));
        yield return ("session with a failed command", SessionView(SessionStates.InFlightError()));
        yield return ("session ended", SessionView(SessionStates.Ended()));
        yield return ("session that did not take effect", SessionView(SessionStates.TargetVanished()));
        yield return ("result that worked", ResultView(PhaseStates.ResultWorks()));
        yield return ("result that partly worked", ResultView(PhaseStates.ResultPartial()));
        yield return ("result refused", ResultView(PhaseStates.ResultRefused()));
        yield return ("result that did not take effect", ResultView(PhaseStates.ResultVanished()));
        yield return ("result that did not start", ResultView(PhaseStates.ResultNotStarted()));
        yield return ("result with the history open", ResultView(PhaseStates.ResultWithHistoryChosen(), "HistorySection"));
    }

    private static FrameworkElement SetupView(SessionViewModel model, string? openSection = null)
        => Opened(new SetupPhaseView { DataContext = model }, openSection);

    private static FrameworkElement SessionView(SessionViewModel model, string? openSection = null)
        => Opened(new SessionPhaseView { DataContext = PhaseStates.WithTarget(model) }, openSection);

    private static FrameworkElement ResultView(SessionViewModel model, string? openSection = null)
        => Opened(new ResultPhaseView { DataContext = model }, openSection);

    /// <summary>Opens the named section, and fails loudly when it is not there - a state that silently stayed
    /// folded would be read as the startup state under another name.</summary>
    private static FrameworkElement Opened(FrameworkElement view, string? openSection)
    {
        if (openSection is not null)
        {
            // Through the logical tree, not FindName: the audit block is a control of its own now, and
            // FindName on the phase does not see into it.
            var section = LayoutProbe.FindNamed(view, openSection) as Expander
                ?? throw new InvalidOperationException($"{view.GetType().Name} has no section called {openSection}");
            section.IsExpanded = true;
        }

        return view;
    }

    private static void AssertNothingLeftToScroll(string name, FrameworkElement view)
    {
        var scroll = (ScrollViewer)view.FindName("FormScroll");
        Assert.True(
            scroll.ExtentHeight <= scroll.ViewportHeight + 0.5,
            $"{name} needs {scroll.ExtentHeight:F0} px of scroll area and the canvas gives it {scroll.ViewportHeight:F0} - "
                + "raise PhaseCanvasHeight so the rest of it is read");
    }

    /// <summary>
    /// One section, one label column - measured on the arrange pass rather than declared.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. When the launch fields were merged into the speed
    /// section they were given an IsSharedSizeScope of their own, so they agreed with each other and not
    /// with the row above: the mode list started at x=96 and both launch fields at x=150. Two label
    /// columns inside one section is the exact thing PartFormRow exists to remove, and nothing in the
    /// suite noticed - the sheet that would have shown it is Category=Integration, which the gate skips.
    ///
    /// Reversal probe: put Grid.IsSharedSizeScope="True" back on the inner StackPanel in
    /// SetupPhaseView.xaml and this reddens with two columns instead of one.
    ///
    /// The count is asserted as a literal beside the agreement, because "all the x values agree" is true
    /// of an empty set, and a filter that matched nothing would pass silently.
    /// </remarks>
    [Fact]
    public void Every_field_in_the_options_section_starts_at_one_x()
    {
        var columns = WpfTestHost.InvokeSettled(() =>
        {
            var view = new SetupPhaseView { DataContext = new SessionViewModel() };
            if (view.FindName("SpeedSection") is Expander section)
            {
                section.IsExpanded = true;
            }

            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var band = elements.Single(e => e.Name == "SpeedSection").Bounds;

            return elements
                .Where(e => e.IsVisible
                    && (e.Kind == nameof(ComboBox) || e.Kind == nameof(TextBox))
                    && e.Bounds.Top >= band.Top
                    && e.Bounds.Bottom <= band.Bottom)
                .Select(e => (int)Math.Round(e.Bounds.X))
                .ToList();
        });

        Assert.Equal(OptionsSectionFields, columns.Count);
        Assert.Single(columns.Distinct());
    }

    /// <summary>The speed list, the arguments box and the working-folder box. A literal, so the agreement
    /// assertion above cannot be satisfied by a filter that matched nothing.</summary>
    private const int OptionsSectionFields = 3;

    /// <summary>
    /// A folded audit still states every count that can change what the reader concludes, each read off
    /// its own list.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. The folded header carried the covered, uncovered and
    /// warning counts and nothing for the channels the session could not watch, so a session with an
    /// unwatched channel said nothing about it until the section was opened (untouchable rule 4).
    ///
    /// The five counts differ on purpose. With two lists of the same length, a chip bound to the wrong
    /// list would show the right number and pass.
    ///
    /// Reversal probes: delete UnobservedChip from AuditSummary in SessionPhaseView.xaml and this reddens on
    /// the chip count. Bind that chip to Uncovered.Count instead and it reddens on the number.
    /// </remarks>
    [Fact]
    public void A_folded_audit_counts_every_list_that_can_change_what_the_reader_concludes()
    {
        var chips = WpfTestHost.InvokeSettled(() =>
        {
            var model = SessionStates.Running();
            model.Apply(new CoverageEvent
            {
                V = ProtocolJson.ProtocolVersion,
                Pid = 4242,
                Covered =
                [
                    new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 10 },
                    new CoveredChannel { Channel = "GetLocalTime", Calls = 20 },
                    new CoveredChannel { Channel = "GetTickCount64", Calls = 30 },
                ],
                Uncovered = ["QueryPerformanceCounter"],
                Unobserved = ["NtQuerySystemTime", "timeGetTime"],
                WarningKeys =
                [
                    "wait.timeout_collapsed",
                    "coverage.channel_installed_late",
                    "coverage.pid_registry_full",
                    "source.network_at_start",
                ],
            });
            // The fifth count: processes the hook never got into, on the family verdict - which also raises
            // a warning of its own, so the warnings go to five and the processes to six. That verdict opens
            // the section by itself, so the reader who folds it is the one this guard stands for.
            PhaseStates.WithUncoveredProcesses(model, total: 6);

            var view = new SessionPhaseView { DataContext = model };
            LayoutProbe.Settle(view);
            ((Expander)LayoutProbe.FindNamed(view, "AuditSection")!).IsExpanded = false;
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);

            return elements
                .Where(e => e.IsVisible && e.Name.EndsWith("Chip", StringComparison.Ordinal))
                .ToDictionary(
                    chip => chip.Name,
                    chip => elements.Single(e => e.Kind == nameof(TextBlock) && chip.Bounds.Contains(e.Bounds)).Text);
        });

        Assert.Equal(AuditChips, chips.Count);
        Assert.EndsWith(" 3", chips["CoveredChip"], StringComparison.Ordinal);
        Assert.EndsWith(" 1", chips["UncoveredChip"], StringComparison.Ordinal);
        Assert.EndsWith(" 2", chips["UnobservedChip"], StringComparison.Ordinal);
        Assert.EndsWith(" 5", chips["WarningsChip"], StringComparison.Ordinal);
        Assert.EndsWith(" 6", chips["ProcessesChip"], StringComparison.Ordinal);
    }

    /// <summary>Covered, uncovered, could not be watched, warnings, processes on the real clock. A literal,
    /// so a walk that found no chips cannot pass by never reaching the number checks.</summary>
    private const int AuditChips = 5;

    /// <summary>
    /// A session whose family spawned processes the hook never got into names them under the audit
    /// table: one row per executable and role, the true total in the heading, and the count the report
    /// could not name said in words.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. The verdict's reason said "the ones this application
    /// started without it ran on the real clock", the warning said "this application started processes that
    /// never got the fake clock", and the screen never said which - the model dropped the list the family
    /// verdict carried (untouchable rule 4, and the CLI report had named them all along).
    ///
    /// The fixture is the shape two applications with an embedded web engine produced: one runtime in
    /// four roles, a second with a renderer, two children gone before they could be named, a total above
    /// the list. The numbers differ from each other on purpose, so a heading bound to the row count (8) or
    /// the list length (8) instead of the total (11) reddens on the number.
    ///
    /// Reversal probes: delete ProcessBlock from AuditSectionView.xaml and this reddens on visibility. Bind
    /// ProcessHeading to UncoveredChildren.Count and it reddens on "(11)". Drop the DataTrigger on IsUnnamed
    /// from ProcessRowTemplate and it reddens on the unnamed sentence.
    /// </remarks>
    [Fact]
    public void A_session_that_spawned_processes_without_the_hook_names_them_under_the_audit_table()
    {
        var (block, heading, rows, more) = WpfTestHost.InvokeSettled(() =>
        {
            var view = ResultView(PhaseStates.ResultPartialWithUncoveredProcesses());
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var table = elements.Single(e => e.Name == "ProcessTable");
            var cells = elements
                .Where(e => e.Kind == nameof(TextBlock) && e.IsVisible && table.Bounds.Contains(e.Bounds))
                .Select(e => e.Text)
                .ToList();
            return (
                elements.Single(e => e.Name == "ProcessBlock").IsVisible,
                elements.Single(e => e.Name == "ProcessHeading").Text,
                cells,
                elements.Single(e => e.Name == "ProcessesMore"));
        });

        Assert.True(block, "the processes the hook never got into are not on the result screen");
        Assert.EndsWith("(11)", heading, StringComparison.Ordinal);
        Assert.Equal(4, rows.Count(t => t == "msedgewebview2.exe"));
        Assert.Equal(1, rows.Count(t => t == "QtWebEngineProcess.exe"));
        Assert.Equal(2, rows.Count(t => t == "renderer"));
        Assert.Equal(1, rows.Count(t => t == TranslationKeyConverter.Resolve("audit.process_unnamed")));
        Assert.True(more.IsVisible, "the count the report could not name is not said under the table");
        Assert.Contains(" 3 ", more.Text, StringComparison.Ordinal);
    }

    /// <summary>
    /// A session that reached the pages inside its application shows those pages' reads as rows of the
    /// audit table, under the parent's rows, on the fake clock - beside the process table that still
    /// names the engine's helper processes on the real clock (docs/09 section 12.11). Measured on the
    /// render: the page rows have to survive the parent's final snapshot, which arrives before them.
    /// </summary>
    /// <remarks>
    /// Reversal probe: fold context rows into the parent's snapshot in SessionViewModel.Apply (assign
    /// Covered from the parent event alone) and this reddens on the page row.
    /// </remarks>
    [Fact]
    public void A_session_that_reached_the_pages_inside_the_app_lists_their_reads_under_the_parent()
    {
        var (cells, processBlock) = WpfTestHost.InvokeSettled(() =>
        {
            var view = ResultView(PhaseStates.ResultPartialWithEmbeddedPages());
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var table = elements.Single(e => e.Name == "AuditTable");
            var texts = elements
                .Where(e => e.Kind == nameof(TextBlock) && e.IsVisible && table.Bounds.Contains(e.Bounds))
                .Select(e => e.Text)
                .ToList();
            return (texts, elements.Single(e => e.Name == "ProcessBlock").IsVisible);
        });

        Assert.Contains("page Date.now", cells);
        Assert.Contains("worker Date.now", cells);
        Assert.Contains("GetSystemTimeAsFileTime", cells);
        Assert.True(
            cells.IndexOf("GetSystemTimeAsFileTime") < cells.IndexOf("page Date.now"),
            "the parent's rows lead, the pages' rows follow");
        Assert.True(processBlock, "the engine's helper processes still ran on the real clock and stay in their table");
    }

    /// <summary>
    /// An executable name longer than the name column trims to the column's ceiling, and the count beside
    /// it stays on the card.
    /// </summary>
    /// <remarks>
    /// The table's well does not scroll sideways and the core lets two hundred characters of a name
    /// through, so an uncapped shared column would grow to the name and carry the role and the count off
    /// the card with no ellipsis anywhere. Measured on the render, not read off the attribute: a MaxWidth on
    /// a shared-size column is the kind of property a toolkit can ignore quietly (GUI rule 10).
    ///
    /// The first build of the fix put the ceiling on the shared-size COLUMN, and this guard read the name
    /// cell at 1 346 px under a 268 px ceiling: WPF ignores MaxWidth there. The ceiling lives on the cell.
    ///
    /// Reversal probe: drop MaxWidth from the name cell in ProcessRowTemplate and this reddens on the name
    /// cell's width, and again on the count cell's right edge.
    /// </remarks>
    [Fact]
    public void A_process_name_longer_than_its_column_trims_and_keeps_the_count_on_the_card()
    {
        var (ceiling, nameWidth, countRight, tableRight) = WpfTestHost.InvokeSettled(() =>
        {
            // A finished session given the verdict again, now naming one process with a two-hundred
            // character name - the longest the core lets through.
            var model = PhaseStates.WithUncoveredProcesses(PhaseStates.ResultPartial(), total: 1, image: new string('x', 200) + ".exe");
            var view = ResultView(model);
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var table = elements.Single(e => e.Name == "ProcessTable");
            // Found by their text and their row, NOT by lying inside the table: a cell that overflowed the
            // table is exactly the failure this guard reads for, and a filter on containment would turn
            // that failure into "no such element".
            var cells = elements
                .Where(e => e.Kind == nameof(TextBlock) && e.IsVisible
                    && e.Bounds.Top >= table.Bounds.Top && e.Bounds.Bottom <= table.Bounds.Bottom)
                .ToList();
            var name = cells.Single(e => e.Text.StartsWith("xxxx", StringComparison.Ordinal));
            var count = cells.Single(e => e.Text == "1");
            return (
                (double)view.FindResource("ProcessImageColumnMaxWidth"),
                name.Bounds.Width,
                count.Bounds.Right,
                table.Bounds.Right);
        });

        Assert.True(nameWidth <= ceiling, $"the name cell is {nameWidth:F0} px wide against a ceiling of {ceiling:F0}");
        Assert.True(countRight <= tableRight + 0.5, $"the count ends at {countRight:F0}, past the table's edge at {tableRight:F0}");
    }

    /// <summary>A session whose family spawned nothing the hook missed shows no process table at all - an
    /// empty table under a heading with a zero would be a question nobody asked.</summary>
    [Fact]
    public void A_session_with_every_process_covered_shows_no_process_table()
    {
        var visible = WpfTestHost.InvokeSettled(() =>
        {
            var view = ResultView(PhaseStates.ResultPartial());
            LayoutProbe.Settle(view);
            return LayoutProbe.Walk(view).Single(e => e.Name == "ProcessBlock").IsVisible;
        });

        Assert.False(visible, "the process table is on screen for a session that had nothing to put in it");
    }

    /// <summary>
    /// A session that reached a web engine inside the application names it and the port it was reached on,
    /// under the process table and above the warnings.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE SCREEN WAS SILENT ABOUT BOTH. The wire has carried
    /// <c>session_verdict.engines</c> since slice C and the CLI report printed "on port N (pid M)" from it,
    /// while the GUI showed the warning that a debugging port stands open to other programs for as long as
    /// the engine runs - and no way to learn which port that was. A warning whose subject cannot be
    /// identified is one nobody can act on.
    ///
    /// The order is part of the claim: the engines sit between the processes that stayed on the real clock
    /// and the warnings, the way the CLI report lays them out, because the warning about the port comes
    /// after the list that names it.
    ///
    /// Reversal probes: delete EngineBlock from AuditSectionView.xaml and this reddens on visibility. Drop
    /// the port cell from EngineRowTemplate and it reddens on the port. Move the block above ProcessBlock
    /// and it reddens on the order.
    /// </remarks>
    [Fact]
    public void A_session_that_reached_a_web_engine_names_it_and_its_port_under_the_process_table()
    {
        var (block, cells, engineTop, processTop, warningsTop) = WpfTestHost.InvokeSettled(() =>
        {
            var view = ResultView(PhaseStates.ResultPartialWithEmbeddedPages());
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var table = elements.Single(e => e.Name == "EngineTable");
            var texts = elements
                .Where(e => e.Kind == nameof(TextBlock) && e.IsVisible && table.Bounds.Contains(e.Bounds))
                .Select(e => e.Text)
                .ToList();
            return (
                elements.Single(e => e.Name == "EngineBlock").IsVisible,
                texts,
                table.Bounds.Top,
                elements.Single(e => e.Name == "ProcessTable").Bounds.Top,
                elements.Single(e => e.Name == "AuditWarnings").Bounds.Top);
        });

        Assert.True(block, "the engine the session reached is not on the result screen");
        Assert.Contains("Engine/153.0", cells);
        Assert.Contains("61868", cells);
        Assert.True(processTop < engineTop, "the engines are not under the processes that stayed on the real clock");
        Assert.True(engineTop < warningsTop, "the warning about the open port comes before the list that names it");
    }

    /// <summary>
    /// An engine name longer than its column trims and leaves the port where the reader can read it.
    /// </summary>
    /// <remarks>
    /// The name comes out of the engine's own version endpoint, so its length is not ours to promise, and
    /// the well does not scroll sideways. Reversal probe: drop MaxWidth from the name cell in
    /// EngineRowTemplate and this reddens on the cell's width, and again on the port's right edge.
    ///
    /// 🔴 The ceiling has to be on the CELL. WPF ignores MaxWidth on a ColumnDefinition in a SharedSizeGroup
    /// - measured on the process table, 1 346 px of cell under a 268 px column ceiling.
    /// </remarks>
    [Fact]
    public void An_engine_name_longer_than_its_column_trims_and_keeps_the_port_on_the_card()
    {
        var (ceiling, nameWidth, portRight, tableRight) = WpfTestHost.InvokeSettled(() =>
        {
            var model = PhaseStates.WithEngine(PhaseStates.ResultPartial(), new string('y', 200) + "/1.0");
            var view = ResultView(model);
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);
            var table = elements.Single(e => e.Name == "EngineTable");
            // Found by their row rather than by lying inside the table: a cell that overflowed the table is
            // the failure this guard reads for, and a containment filter would report it as "no such cell".
            var cells = elements
                .Where(e => e.Kind == nameof(TextBlock) && e.IsVisible
                    && e.Bounds.Top >= table.Bounds.Top && e.Bounds.Bottom <= table.Bounds.Bottom)
                .ToList();
            var name = cells.Single(e => e.Text.StartsWith("yyyy", StringComparison.Ordinal));
            var port = cells.Single(e => e.Text == "61868");
            return (
                (double)view.FindResource("EngineNameColumnMaxWidth"),
                name.Bounds.Width,
                port.Bounds.Right,
                table.Bounds.Right);
        });

        Assert.True(nameWidth <= ceiling, $"the name cell is {nameWidth:F0} px wide against a ceiling of {ceiling:F0}");
        Assert.True(portRight <= tableRight + 0.5, $"the port ends at {portRight:F0}, past the table's edge at {tableRight:F0}");
    }

    /// <summary>A session that reached no web engine shows no engine table - an empty table would be an
    /// answer to a question nobody asked, and worse, would read as "an engine was found and named nothing".</summary>
    [Fact]
    public void A_session_that_reached_no_web_engine_shows_no_engine_table()
    {
        var visible = WpfTestHost.InvokeSettled(() =>
        {
            var view = ResultView(PhaseStates.ResultPartial());
            LayoutProbe.Settle(view);
            return LayoutProbe.Walk(view).Single(e => e.Name == "EngineBlock").IsVisible;
        });

        Assert.False(visible, "the engine table is on screen for a session that reached no engine");
    }

    /// <summary>
    /// The two clocks share the outer edges of the card under them, and the channel between them is wider
    /// than the padding inside either.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. The first pair gave each clock a 4 px margin all
    /// round: 8 px between the clocks against 16 px of padding inside each, so the channel between two
    /// groups was narrower than the space inside one, and both outer edges stood 4 px inside the card below.
    ///
    /// Reversal probe: put back UniformGrid Columns="2" with a SpaceXs margin on each clock in
    /// SessionPhaseView.xaml and this reddens on the left edge.
    /// </remarks>
    [Fact]
    public void The_two_clocks_share_the_edges_of_the_card_under_them_and_stand_further_apart_than_its_padding()
    {
        var (fake, real, card, padding) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new SessionPhaseView { DataContext = SessionStates.Running() };
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);

            return (
                elements.Single(e => e.Name == "FakeClock").Bounds,
                elements.Single(e => e.Name == "RealClock").Bounds,
                elements.Single(e => e.Name == "ControlsCard").Bounds,
                ((Thickness)view.FindResource("CardPadding")).Left);
        });

        Assert.Equal(Math.Round(card.Left), Math.Round(fake.Left));
        Assert.Equal(Math.Round(card.Right), Math.Round(real.Right));
        Assert.Equal(Math.Round(fake.Width), Math.Round(real.Width));
        Assert.True(
            real.Left - fake.Right > padding,
            $"the clocks stand {real.Left - fake.Right} px apart, which is not wider than the {padding} px inside each");
    }

    /// <summary>
    /// Both control rows start at the x the line under them starts at.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. Every button carried a uniform 4 px margin, the first
    /// one included, so both rows began 4 px inside their column while the reported speed and the jump note
    /// began on it.
    ///
    /// Reversal probe: set the ControlButton margin in SessionPhaseView.xaml back to SpaceXs and this reddens
    /// with two x values instead of one.
    ///
    /// The card's left edge is asserted too, because four readings agree perfectly over a tree the layout
    /// pass never reached - every one of them zero.
    /// </remarks>
    [Fact]
    public void Both_control_rows_start_where_the_line_under_them_starts()
    {
        var (starts, cardLeft) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new SessionPhaseView { DataContext = SessionStates.Running() };
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);

            int FirstButton(string row)
            {
                var band = elements.Single(e => e.Name == row).Bounds;
                return elements
                    .Where(e => e.IsVisible && e.Kind == nameof(Button) && band.Contains(e.Bounds))
                    .Min(e => (int)Math.Round(e.Bounds.X));
            }

            int Start(string name) => (int)Math.Round(elements.Single(e => e.Name == name).Bounds.X);

            int[] readings = [FirstButton("SpeedPresets"), Start("SpeedFact"), FirstButton("JumpButtons"), Start("JumpNote")];
            return (readings, elements.Single(e => e.Name == "ControlsCard").Bounds.Left);
        });

        Assert.Single(starts.Distinct());
        Assert.True(starts[0] > cardLeft, $"the rows start at {starts[0]}, which is not inside the card at {cardLeft}");
    }

    /// <summary>
    /// A command that fails while a session runs is explained at the size of the prose beside it, in the
    /// error ink.
    /// </summary>
    /// <remarks>
    /// 🔴 THIS GUARD EXISTS BECAUSE THE FAULT WAS MADE. The line took PartFieldMessage, a caption in the error
    /// colour, and set a sentence of more than a hundred characters at 12 px - the two reductions at once
    /// that PartNote was made to undo. It is compared with the note under Stop rather than with a number,
    /// so the rule is "as large as the prose beside it" and survives a change to the type scale.
    ///
    /// Reversal probe: give InFlightError the PartFieldMessage style again in SessionPhaseView.xaml and this
    /// reddens on the size.
    /// </remarks>
    [Fact]
    public void A_command_that_fails_mid_session_is_explained_at_the_size_of_the_prose_beside_it()
    {
        var (error, note) = WpfTestHost.InvokeSettled(() =>
        {
            var view = new SessionPhaseView { DataContext = SessionStates.InFlightError() };
            LayoutProbe.Settle(view);
            var elements = LayoutProbe.Walk(view);

            return (elements.Single(e => e.Name == "InFlightError"), elements.Single(e => e.Name == "StopNote"));
        });

        Assert.True(error.IsVisible && note.IsVisible, "the fixture no longer shows both lines, so nothing was compared");
        Assert.Equal(note.FontSize, error.FontSize);
        Assert.NotEqual(note.Foreground, error.Foreground);
    }

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

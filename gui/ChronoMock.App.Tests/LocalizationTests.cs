using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Windows;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Tests;

/// <summary>
/// The interface strings mechanism (untouchable rule 15): keys not literals, every language file the same
/// key set, and the language list discovered by scanning the folder rather than a hardcoded list.
/// </summary>
public class LocalizationTests
{
    [Fact]
    public void English_and_Polish_have_the_same_key_set()
    {
        // XamlReader builds WPF objects, so load on the UI thread.
        var en = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("en")));
        var pl = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("pl")));
        Assert.Equal(en, pl);
    }

    [Fact]
    public void Available_cultures_are_discovered_by_scanning_the_folder()
    {
        var cultures = LocalizationService.AvailableCultures();
        Assert.Contains("en", cultures);
        Assert.Contains("pl", cultures);
    }

    // Keys the core sends over the wire that the GUI renders through a translation key. The parity test
    // above only proves EN and PL agree - it cannot see a key that lives in the core and is absent from
    // BOTH files, which then renders as raw jargon (RELEASE-002). This list mirrors the core's emitted keys:
    // crates/cli (describe_reason / describe_warning / session_reason_key, and the start-error emissions) and
    // crates/mech (warning keys). When the core adds a rendered key, add it here and to Strings.{en,pl}.json.
    private static readonly string[] CoreWireKeys =
    [
        // Verdict reason keys (VerdictEvent / SessionVerdict reason_key), shown under a non-works verdict.
        "coverage.time_channels_covered", "coverage.time_channels_partial",
        "coverage.time_channels_uncovered", "coverage.undetermined",
        "session.family_covered", "session.family_partial",
        "session.family_uncovered", "session.family_undetermined",
        "session.family_partial_children", "session.family_uncovered_children",
        "session.family_handed_to_copy",
        "chromium.contexts_covered", "chromium.contexts_partial",
        "chromium.no_time_calls", "chromium.no_contexts",
        // Coverage warning keys (CoverageEvent warning_keys).
        "source.network_at_start", "wait.object_waits_not_scaled", "wait.timeout_collapsed",
        "timer.interval_floored", "wait.network_timeouts_scaled", "wait.thread_without_storage",
        "timer.multimedia_not_scaled", "clock.timegettime_scaled_audio_may_shift",
        "coverage.channel_installed_late",
        "inheritance.ntcreateuserprocess_child_maybe_uncovered",
        "inheritance.child_not_injected",
        // Session-level warning keys (SessionVerdict warning_keys).
        "coverage.pid_registry_full", "coverage.session_clock_never_read",
        "inheritance.children_uncovered", "embedded.web_engine_uncovered",
        "embedded.web_engine_processes_uncovered",
        // The embedded-engine channel (docs/09 section 12): the pages inside the application.
        "embedded.web_engine_reached", "embedded.debug_port_open", "embedded.variables_outlive_session", "embedded.engine_unreachable",
        "embedded.discovery_unavailable", "embedded.qt_port_taken", "embedded.zone_is_host",
        "embedded.registry_arguments_hidden", "embedded.pages_not_released", "embedded.zone_kept",
        "embedded.webview2_not_reached", "embedded.elevated_host",
        // The one WebView2 value a session may write to the machine registry for an application that runs
        // as administrator (docs/09 section 12.19), and what became of it.
        "embedded.policy_value_removed", "embedded.policy_value_left", "embedded.policy_value_recovered",
        "embedded.policy_value_stale", "embedded.policy_value_foreign", "embedded.policy_not_written",
        "embedded.policy_name_mismatch",
        "time.fake_clock_clamped", "time.duration_axis_clamped",
        "time.host_slept", "timer.absolute_due_converted",
        // The application outlived the session and was let go (docs/01 section 8.4), and the session
        // outlived the program it launched and went on for what that program started (ADR-16).
        "session.left_running", "session.followed_family",
        // A process of the session the core could not wait on (R5-D3).
        "session.member_unwatchable", "session.handed_to_copy", "session.copy_search_failed",
        // An application that ended with the exit code of a crash, after the guard window.
        "session.target_crashed",
        // A TZ of the tester's own, not handed on to the application so the session zone reaches it.
        "session.tz_variable_removed",
        "chromium.launched_with_debug_port", "chromium.app_closed_before_audit",
        "chromium.rate_change_affects_running_timers", "chromium.contexts_pooled", "chromium.context_reached_running",
        // A timer the engine's 4 ms floor held back (R5-P6) and a frame the shim did not reach (R5-P5).
        "chromium.timer_rate_capped", "chromium.frames_unreached",
        // Pages gone before the audit read them (R5-F10), and the engine's own stamps and animations at real
        // speed while the timers ran faster (R5-F9).
        "chromium.contexts_unread", "chromium.engine_stamps_real", "chromium.animations_real",
        // A page that took a speed change or a jump late or not at all (R4-S17).
        "chromium.clock_move_missed",
        // The target handed the application over to another program and the session went on with it
        // (R4-S18), and a page that read another zone than the session's (R4/16).
        "chromium.followed_browser", "chromium.zone_is_host",
        // A page hidden while its timers ran faster (R4-N28), and an Electron application's main process
        // on the real clock (R4-S19).
        "chromium.background_timers_slowed", "chromium.main_process_uncovered",
        // Runtime detection warnings the driver appends to coverage (B1): a Python/.NET/Java target whose
        // monotonic/elapsed clock stands on QPC and does not scale.
        // Not a QPC-axis caution like the four below: a clock that is out of reach entirely.
        "runtime.go_wall_clock_unreachable",
        "runtime.python_monotonic_qpc", "runtime.python_perfcounter_qpc",
        "runtime.dotnet_stopwatch_qpc", "runtime.java_nanotime_qpc",
        // QPC scaling render caution (A2), shown when --scale-qpc replaces the runtime.*_qpc warnings.
        "qpc.scaled_render_may_distort",
        // Engine ceiling: a limit inside the target that no clock coverage lifts, so it survives
        // --scale-qpc alongside the caution above rather than being replaced by it.
        "runtime.unity_delta_time_capped",
        // Cleanup residue (EndedEvent residue_keys) - a teardown that could not finish (CDP temp profile,
        // a browser whose processes could not be confirmed to have ended).
        "cleanup.chromium_profile_left", "cleanup.chromium_process_left",
        // Vanish reasons (VanishedEvent reason_key, shown inside report.vanish_detail).
        "target.single_instance_suspected", "target.handed_off_uncovered", "target.crashed",
        // Start/fatal error keys, surfaced as the status headline (RELEASE-001).
        "core.hook_dll_missing", "time.bad_mode", "time.bad_multiplier", "moment.invalid",
        // The session gate (R4-S7, R4-S9): a moment the clock cannot hold, and a zone it cannot carry.
        "moment.out_of_range", "time.bad_zone",
        // In-flight jump rejections (Event::Error answering a jump command). Both were missing from
        // this list AND from both translation files until the wire-key guard in
        // crates/cli/tests/wire_keys.rs found them: `chrono run <target> --jump-after 1:+5bd` emits
        // moment.needs_calendar today, so this was a live gap, not a hypothetical one.
        "moment.needs_calendar", "moment.unsupported_kind",
        "protocol.version_mismatch",
        "session.control_failed", "target.launch_failed", "target.inject_failed",
        "target.cwd_missing",
        // The application's own start failing while Windows loads it, before the hook's turn (R4-S6).
        "target.loader_dll_not_found", "target.loader_entry_missing", "target.loader_bad_image",
        "target.loader_init_failed", "target.died_while_loading",
        "target.attach_failed", "target.bitness_mismatch", "session.already_active",
        "protocol.no_command", "protocol.bad_command",
        "protocol.bad_command_ignored",
        // The command stream itself failing, which ENDS the session - unlike one unreadable command.
        "protocol.line_too_long", "protocol.stream_unreadable",
        "protocol.unsupported_command", "protocol.expected_start",
        // The line the protocol crate writes in place of an event it could not serialize. It sat in a raw
        // JSON string that the wire-key scan could not see into, so it was missing here and from both
        // translation files until the scan learned to read inside one.
        "proto.serialize_failed",
    ];

    [Fact]
    public void Every_core_emitted_wire_key_has_a_non_empty_English_and_Polish_resource()
    {
        var missing = WpfTestHost.Invoke(() =>
        {
            var en = LocalizationService.Load("en");
            var pl = LocalizationService.Load("pl");
            var gaps = new List<string>();
            foreach (var key in CoreWireKeys)
            {
                if (en[key] is not string ev || ev.Length == 0) { gaps.Add($"{key} (en)"); }
                if (pl[key] is not string pv || pv.Length == 0) { gaps.Add($"{key} (pl)"); }
            }

            return gaps;
        });

        Assert.True(missing.Count == 0, "core wire keys with no resource: " + string.Join(", ", missing));
    }

    // A text that tells the user when an option matters names that option by its label, so it can be
    // found in the window. A label renamed later leaves such a text pointing at a control that no
    // longer exists under that name, and nothing else notices. Each pair is (the text, the label it
    // quotes), checked in every language the folder holds.
    private static readonly (string Text, string Label)[] QuotedLabels =
    [
        ("runtime.dotnet_stopwatch_qpc", "setup.opt_duration"),
        // The advice to take the option, and the line about a value a crashed session left that only that
        // option removes: both point at the checkbox, so both quote it.
        ("embedded.elevated_host", "setup.opt_elevated_embedded"),
        ("embedded.policy_value_stale", "setup.opt_elevated_embedded"),
    ];

    [Fact]
    public void A_text_that_names_an_option_quotes_its_current_label()
    {
        var cultures = LocalizationService.AvailableCultures();
        var stale = WpfTestHost.Invoke(() =>
        {
            var gaps = new List<string>();
            foreach (var culture in cultures)
            {
                var strings = LocalizationService.Load(culture);
                foreach (var (text, label) in QuotedLabels)
                {
                    // An empty label is contained in every text, so it would pass without quoting anything.
                    if (strings[text] is not string body || strings[label] is not string name
                        || name.Length == 0 || !body.Contains(name, StringComparison.Ordinal))
                    {
                        gaps.Add($"{text} does not quote {label} ({culture})");
                    }
                }
            }

            return gaps;
        });

        // Over no languages at all the loop above finds nothing wrong, which is not the same as right.
        Assert.True(cultures.Count >= 2, "expected at least English and Polish, found: " + string.Join(", ", cultures));
        Assert.True(stale.Count == 0, "texts quoting a label that has changed: " + string.Join(", ", stale));
    }

    private static HashSet<string> KeysOf(ResourceDictionary dictionary)
        => dictionary.Keys.Cast<object>().Select(key => key.ToString()!).ToHashSet(StringComparer.Ordinal);

    /// <summary>
    /// S-31 regression. The culture is sliced out of the file name, and the glob can match a name too
    /// short to slice - "Strings.xaml" beside the real ones. That threw instead of skipping the file,
    /// which matters because this list feeds the language picker.
    /// </summary>
    [Fact]
    public void A_file_name_too_short_to_carry_a_culture_is_skipped_not_thrown_on()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-loc-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            File.WriteAllText(Path.Combine(dir, "Strings.json"), "{}");
            File.WriteAllText(Path.Combine(dir, "Strings.de.json"), "{}");
            var cultures = LocalizationService.AvailableCulturesIn(dir);
            Assert.Equal(new[] { "de" }, cultures);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    /// <summary>
    /// S-18. The strings files are loose files beside the exe, and the documented contract invites people
    /// to add them. They were XAML, parsed by XamlReader - a full deserializer that instantiates the types
    /// a document names, so such a file was executable code in the application's context. Validating the
    /// RESULT could never have fixed that, because the code runs while parsing. They are data now: a file
    /// that is not a flat string map is refused, and nothing in it can be anything but a string.
    /// </summary>
    [Fact]
    public void The_strings_files_are_data_and_a_non_string_document_is_refused()
    {
        foreach (var culture in new[] { "en", "pl" })
        {
            var path = Path.Combine(
                AppContext.BaseDirectory, "Localization", $"Strings.{culture}.json");
            Assert.True(File.Exists(path), $"{culture} strings must ship as JSON");

            var text = File.ReadAllText(path);
            Assert.DoesNotContain("ResourceDictionary", text, StringComparison.Ordinal);
            Assert.DoesNotContain("ObjectDataProvider", text, StringComparison.Ordinal);

            // Every value the file can express is a string - the type says so, and it parses.
            var options = new System.Text.Json.JsonSerializerOptions
            {
                ReadCommentHandling = System.Text.Json.JsonCommentHandling.Skip,
                AllowTrailingCommas = true,
            };
            var map = System.Text.Json.JsonSerializer
                .Deserialize<Dictionary<string, string>>(text, options);
            Assert.NotNull(map);
            Assert.NotEmpty(map);
        }
    }

    /// <summary>A file that is not a string map at all names itself instead of failing later as a pile of
    /// missing keys.</summary>
    [Fact]
    public void A_malformed_strings_file_is_reported_with_its_path()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-loc-bad", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            var path = Path.Combine(dir, "Strings.xx.json");
            File.WriteAllText(path, "{ \"a\": { \"nested\": 1 } }");
            var options = new System.Text.Json.JsonSerializerOptions
            {
                ReadCommentHandling = System.Text.Json.JsonCommentHandling.Skip,
            };
            Assert.ThrowsAny<System.Text.Json.JsonException>(
                () => System.Text.Json.JsonSerializer.Deserialize<Dictionary<string, string>>(
                    File.ReadAllText(path), options));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
    /// <summary>
    /// A damaged strings file must degrade the interface, not stop the application. Before this,
    /// `OnStartup` caught the load failure, showed a box and called `Shutdown(1)`: the app could not
    /// be opened at all because one loose JSON file next to the exe was missing - and adding files
    /// there is exactly what the product tells people to do. The fallback was already written and
    /// tested (a missing key renders AS the key), it simply was not allowed to run (R3-11).
    /// </summary>
    [Fact]
    public void A_missing_strings_file_degrades_the_interface_instead_of_stopping_the_app()
    {
        var reason = WpfTestHost.Invoke(() =>
        {
            try
            {
                // A well-formed culture tag with no file behind it - the shape of a user who added
                // a language to the picker and not the file, or deleted the English one.
                return LocalizationService.ApplyOrDegrade(System.Windows.Application.Current, "zz-ZZ");
            }
            finally
            {
                // Restore the real dictionary: Application.Current is process-wide, and every
                // window-rendering test after this one resolves its labels through it.
                LocalizationService.Apply(System.Windows.Application.Current, LocalizationService.DefaultCulture);
            }
        });

        Assert.NotNull(reason);
        Assert.Contains("zz-ZZ", reason, StringComparison.Ordinal);
    }

    /// <summary>The healthy path still reports success, so "degrade" cannot quietly become the
    /// normal case - a fallback that always fires is indistinguishable from a broken loader.</summary>
    [Fact]
    public void A_present_strings_file_reports_no_problem()
    {
        var reason = WpfTestHost.Invoke(() =>
            LocalizationService.ApplyOrDegrade(System.Windows.Application.Current, LocalizationService.DefaultCulture));

        Assert.Null(reason);
    }

    /// <summary>
    /// R4-N51: a culture formats in its default calendar, which for these three is not the Gregorian one -
    /// measured, 2026 came out as 2569, 1448 and 1405. Any of them can be the interface language (a strings
    /// file dropped beside the exe), and the moments the window states are the Gregorian dates the core
    /// works in. The names of days and months stay the culture's own.
    /// </summary>
    [Theory]
    [InlineData("th")]
    [InlineData("ar-SA")]
    [InlineData("fa-IR")]
    [InlineData("pl")]
    public void A_moment_is_formatted_in_the_gregorian_calendar_whatever_the_language(string tag)
    {
        var culture = LocalizationService.FormatCultureFor(tag);
        var moment = new DateTime(2026, 10, 4, 12, 0, 0, DateTimeKind.Unspecified);

        Assert.Equal("2026", string.Create(culture, $"{moment:yyyy}"));
        Assert.Equal(
            System.Globalization.CultureInfo.GetCultureInfo(tag).TextInfo.CultureName,
            culture.TextInfo.CultureName);
    }

    [Fact]
    public void The_window_states_a_moment_through_that_calendar()
    {
        // The wiring, not only the helper: a marker for a Buddhist-calendar culture in front of the applied
        // strings, read through the one formatter every stated moment goes through.
        var text = WpfTestHost.Invoke(() =>
        {
            var merged = Application.Current.Resources.MergedDictionaries;
            var marker = new ResourceDictionary { [LocalizationService.MarkerKey] = "th" };
            merged.Insert(0, marker);
            try
            {
                return ClockView.FormatMoment("2026-10-04T12:00:00", "UTC");
            }
            finally
            {
                merged.Remove(marker);
            }
        });

        Assert.Contains("2026", text, StringComparison.Ordinal);
        Assert.DoesNotContain("2569", text, StringComparison.Ordinal);
    }
}

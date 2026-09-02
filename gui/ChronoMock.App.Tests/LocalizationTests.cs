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
    // crates/mech (warning keys). When the core adds a rendered key, add it here and to Strings.{en,pl}.
    private static readonly string[] CoreWireKeys =
    [
        // Verdict reason keys (VerdictEvent / SessionVerdict reason_key), shown under a non-works verdict.
        "coverage.time_channels_covered", "coverage.time_channels_partial",
        "coverage.time_channels_uncovered", "coverage.undetermined",
        "session.family_covered", "session.family_partial",
        "session.family_uncovered", "session.family_undetermined",
        "chromium.contexts_covered", "chromium.contexts_partial",
        "chromium.no_time_calls", "chromium.no_contexts",
        // Coverage warning keys (CoverageEvent warning_keys).
        "source.network_at_start", "wait.object_waits_not_scaled",
        "timer.multimedia_not_scaled", "inheritance.ntcreateuserprocess_child_maybe_uncovered",
        "inheritance.child_vanished_before_audit",
        "chromium.launched_with_debug_port", "chromium.app_closed_before_audit",
        "chromium.rate_change_affects_running_timers",
        // Runtime detection warnings the driver appends to coverage (B1): a Python/.NET/Java target whose
        // monotonic/elapsed clock stands on QPC and does not scale.
        "runtime.python_monotonic_qpc", "runtime.python_perfcounter_qpc",
        "runtime.dotnet_stopwatch_qpc", "runtime.java_nanotime_qpc",
        // QPC scaling render caution (A2), shown when --scale-qpc replaces the runtime.*_qpc warnings.
        "qpc.scaled_render_may_distort",
        // Cleanup residue (EndedEvent residue_keys) - a teardown that could not finish (CDP temp profile).
        "cleanup.chromium_profile_left",
        // Vanish reason (VanishedEvent reason_key, shown inside report.vanish_detail).
        "target.single_instance_suspected",
        // Start/fatal error keys, surfaced as the status headline (RELEASE-001).
        "core.hook_dll_missing", "time.bad_mode", "moment.invalid",
        "session.control_failed", "target.launch_failed", "target.inject_failed",
        "target.attach_failed", "session.already_active",
        "protocol.no_command", "protocol.bad_command", "protocol.expected_start",
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
            File.WriteAllText(Path.Combine(dir, "Strings.xaml"), "<x/>");
            File.WriteAllText(Path.Combine(dir, "Strings.de.xaml"), "<x/>");
            var cultures = LocalizationService.AvailableCulturesIn(dir);
            Assert.Equal(new[] { "de" }, cultures);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
}

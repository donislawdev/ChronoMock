using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Runtime.InteropServices;
using System.Text;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Everything a session leaves behind as text or as a record: the summary copied into a ticket, the
/// diagnostics block written beside the exe, and the history entry (R5-D9, R5-D62). Pure functions over a
/// <see cref="SessionResult"/> and the <see cref="SessionSetup"/> it was started with, so each one is tested
/// without a view model, a window or a core process.
/// <para>
/// They lived in <see cref="SessionViewModel"/>, which reached three thousand lines and stood on its
/// coupling ceiling with them - a string builder, a culture, a file path and the wire records of engines
/// and processes are what composing a report needs, and none of them is what projecting a session onto a
/// panel needs.
/// </para>
/// </summary>
internal static class SessionReport
{
    /// <summary>
    /// Compose the paste-into-ticket session summary (chrono-mock 7.2, 8.8) in the interface language. It
    /// mirrors the CLI evidence export (crates/cli render_evidence): a session that is anything other than a
    /// clean "works" ALWAYS leads with an unreliable-evidence banner - evidence that hides doubt is worse
    /// than none. The caller supplies the key resolver (rule 15), never Application.Current directly, so this
    /// stays testable without WPF.
    /// </summary>
    /// <param name="heartbeatWall">The fake clock of the last heartbeat - the session line names it when the
    /// core never sent its authoritative end wall.</param>
    internal static string Summary(
        SessionResult r, SessionSetup setup, string heartbeatWall, Func<string, string> translate)
    {
        var sb = new StringBuilder();

        if (!r.IsReliable)
        {
            sb.Append(translate("report.unreliable_banner")).Append("\n\n");
        }

        sb.Append(translate("report.title")).Append('\n');
        // The target the session RAN ON, not the one now in the form - see SessionSetup.
        sb.Append("  ").Append(translate("report.target")).Append(": ")
          .Append(Path.GetFileName(setup.TargetPath)).Append('\n');
        // Under the target, where the CLI report names it (R5-D44): a page behaves by its engine, so the
        // version is part of the evidence.
        AppendValue(sb, translate, "report.browser", r.Browser);

        // A session the core never closed says so above the verdict it qualifies (R4-W3), as the CLI report
        // does: the verdict below is the one from the start, not the session's.
        if (r.IsCutShort)
        {
            sb.Append("  ").Append(translate("report.cut_short")).Append('\n');
        }

        // Verdict headline: a vanish is an honest non-effect first, then the family/parent verdict, else none.
        if (r.StatusKind == SessionStatusKind.DidNotTakeEffect)
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ")
              .Append(translate("report.did_not_take_effect")).Append('\n');
            if (r.VanishReasonKey.Length > 0)
            {
                sb.Append("    ")
                  .Append(Fmt(translate, "report.vanish_detail", translate(r.VanishReasonKey), r.LivedMs))
                  .Append('\n');
            }
        }
        else if (r.VerdictKnown)
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ").Append(translate(r.VerdictLabelKey));
            if (r.IsFamily)
            {
                sb.Append("  ").Append(Fmt(translate, "report.processes", r.ProcessCount));
            }

            sb.Append('\n');
            if (r.VerdictHasReason)
            {
                sb.Append("    ").Append(translate(r.VerdictReasonKey)).Append('\n');
            }

            if (r.VerdictHasMeaning)
            {
                sb.Append("    ").Append(translate(r.VerdictMeaningKey)).Append('\n');
            }
        }
        else
        {
            sb.Append("  ").Append(translate("report.verdict")).Append(": ")
              .Append(translate("report.no_verdict")).Append('\n');
        }

        // What a refusal could not end, under the verdict it follows from - where the CLI report puts it -
        // after the caveat that some of it may not have been found at all.
        if (r.RefusalIncomplete)
        {
            sb.Append("  ").Append(translate("result.refusal_incomplete")).Append('\n');
        }

        AppendList(sb, translate, "report.left_running", r.LeftRunningRows, translateItems: false);
        // Every error the core sent, under the verdict and the refusal, as the CLI report lists them (R5-D71).
        // A command it refused while the session ran was on the screen for as long as the session was, and
        // then nowhere - a speed that never took was not in the report about the session it did not take in.
        AppendList(sb, translate, "report.errors", r.Errors, translateItems: true);

        if (r.HasTiming)
        {
            // Prefer the authoritative end wall from `ended` - fall back to the last heartbeat's fake clock.
            var fakeWall = r.FakeEndWall.Length > 0 ? r.FakeEndWall : heartbeatWall;
            sb.Append("  ").Append(translate("report.session")).Append(": ")
              .Append(Fmt(translate, "report.session_reached", fakeWall)).Append('\n');
            sb.Append("    ")
              .Append(Fmt(translate, "report.elapsed", Seconds(r.ElapsedRealMs), Seconds(r.ElapsedFakeMs)))
              .Append('\n');
        }

        AppendSleep(sb, r, translate);

        // The target's own exit code, if the app exited on its own (informational, not a verdict).
        if (r.TargetExitCode is int code)
        {
            sb.Append("  ").Append(translate("report.target_exit")).Append(' ')
              .Append(SessionResult.ExitCodeLabel(code)).Append('\n');
        }

        // What the session went on for after the application closed, under the line that says it closed -
        // where the CLI report puts it, for the reason SessionViewModel.FollowedRows gives.
        AppendList(sb, translate, "report.followed", r.FollowedRows, translateItems: false);
        // The copies the application most likely passed its work to, where the CLI report puts them (ADR-4).
        AppendList(sb, translate, "report.copies_outside", r.CopiesOutsideRows, translateItems: false);

        // Channel names are raw API identifiers (not translated) - warnings are keys the core raised.
        AppendList(sb, translate, "coverage.covered", r.Covered.Select(ch => FormatReadRow(ch, translate)).ToList(), translateItems: false);
        AppendList(sb, translate, "coverage.observed", r.Observed.Select(ch => FormatReadRow(ch, translate)).ToList(), translateItems: false);
        AppendList(sb, translate, "coverage.uncovered", r.Uncovered, translateItems: false);
        // The channels the session meant to watch and could not: on screen since the audit table, and a
        // copied report that left them out claimed a watch it was not running (untouchable rule 4).
        AppendList(sb, translate, "coverage.unobserved", r.Unobserved, translateItems: false);
        // The channels hooked only after their module loaded - shown in the in-app audit, so the copied
        // report must carry them too, or the warning below references a list the reader cannot see (rule 4).
        AppendList(sb, translate, "coverage.installed_late", r.InstalledLate, translateItems: false);
        // The processes the hook never got into, where the CLI report puts them: after the channel lists
        // and before the warnings that talk about them. A copied report that left them out claimed less
        // than the screen shows and less than the CLI says (untouchable rule 4).
        AppendUncoveredChildren(sb, r, translate);
        // The engines whose pages the session reached, in the same place the CLI report puts them: right
        // after the processes that stayed on the real clock, because the two lists answer one question
        // between them - what inside this application ran on which clock.
        AppendEngines(sb, r, translate);
        AppendList(sb, translate, "coverage.warnings", r.Warnings, translateItems: true);
        // Cleanup residue the core could not remove (ended.residue_keys) - reported, never hidden (rule 6).
        AppendList(sb, translate, "report.cleanup", r.ResidueKeys, translateItems: true);

        // The "requested" line is the START request throughout - moment, zone and mode alike.
        sb.Append("  ")
          .Append(Fmt(
              translate,
              "report.requested",
              setup.Moment,
              setup.Zone.Label,
              translate(setup.Mode.LabelKey)))
          .Append('\n');

        return sb.ToString();
    }

    /// <summary>Compose the diagnostics block: a header naming the status, the window and the system, the core
    /// and the mode, the target, the requested moment and the exception the window caught, then the core's
    /// stderr and parse-error lines verbatim, each with its time since the connection. English and stable,
    /// like the core's own stderr and the CLI report - it is a technical artifact for a bug report, not
    /// interface text (rule 15 governs the UI - this is data).</summary>
    internal static string Diagnostics(
        SessionResult r, SessionSetup setup, IEnumerable<string> lines, int dropped, int? coreExit, DateTime utcNow)
    {
        var mode = setup.Mode;
        var modeToken = mode.Mode switch { "frozen" => "frozen", "flow" => "flow", _ => $"x{mode.Multiplier ?? 1}" };

        var sb = new StringBuilder();
        sb.Append("Chrono Mock diagnostics\n");
        sb.Append("  when:      ")
          .Append(utcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture)).Append('\n');
        sb.Append("  status:    ").Append(r.StatusKind).Append(" (").Append(r.StatusKey).Append(")\n");
        // Which window, on which system, drove which core, and how (R5/6b, D2). A block that said none of it
        // left support to ask, and the answer is the first thing a fault in one build and not another needs.
        sb.Append("  gui:       ").Append(AppVersion.Current)
          .Append(" (").Append(Arch(RuntimeInformation.ProcessArchitecture)).Append(" process)\n");
        sb.Append("  os:        ").Append(RuntimeInformation.OSDescription)
          .Append(" (").Append(Arch(RuntimeInformation.OSArchitecture)).Append(")\n");
        sb.Append("  core:      ").Append(CoreLine(r)).Append('\n');
        sb.Append("  mode:      ").Append(r.CorePath.Length == 0 ? "(no plan)" : r.IsCdp ? "chromium" : "native").Append('\n');
        if (r.Browser.Length > 0)
        {
            sb.Append("  browser:   ").Append(r.Browser).Append('\n');
        }
        // The code the core itself ended with, which is what tells a crash from a kill once the core sent
        // nothing more (R4-S27). A negative one is an NTSTATUS, so it is shown in hex as well, as the CLI does.
        if (coreExit is { } code)
        {
            sb.Append("  core exit: ").Append(SessionResult.ExitCodeLabel(code)).Append('\n');
        }

        var target = setup.TargetPath;
        sb.Append("  target:    ").Append(target.Length > 0 ? target : "(none)").Append('\n');
        sb.Append("  requested: ").Append(setup.Moment)
          .Append(" (zone ").Append(setup.Zone.Label).Append(", mode ").Append(modeToken).Append(")\n");
        AppendFault(sb, r.Fault);

        sb.Append("  core output:\n");
        var any = false;
        foreach (var line in lines)
        {
            sb.Append("    ").Append(line).Append('\n');
            any = true;
        }

        if (!any)
        {
            sb.Append("    (no diagnostic output)\n");
        }

        // A block that is a TAIL has to say so, and say how much is missing. The line count is capped
        // so a chatty target cannot grow it without bound, and a reader who takes the tail for the
        // whole of it draws conclusions from an absence that was never there (rule 6).
        if (dropped > 0)
        {
            sb.Append("    [")
              .Append(dropped.ToString(CultureInfo.InvariantCulture))
              .Append(" earlier line(s) dropped - this is the tail of the core's output]\n");
        }

        return sb.ToString();
    }

    /// <summary>
    /// The core the session asked for and what became of it: its version and bitness from the handshake, or
    /// that it never shook hands, or that it never started at all - then its path, which is what a missing or
    /// quarantined core is diagnosed by.
    /// </summary>
    private static string CoreLine(SessionResult r)
    {
        if (r.CorePath.Length == 0)
        {
            return "(none - the session stopped before choosing one)";
        }

        var state = !r.CoreStarted ? "did not start"
            : r.CoreVersion.Length == 0 ? "no handshake"
            : r.CoreVersion + ", " + r.CoreBitness;
        return state + ", " + r.CorePath;
    }

    /// <summary>The exception the window caught, whole - type, message, stack and the faults inside it - under
    /// its own heading, each further line indented under the first. Nothing when there was none.</summary>
    private static void AppendFault(StringBuilder sb, Exception? fault)
    {
        if (fault is null)
        {
            return;
        }

        var lines = fault.ToString().Split('\n');
        sb.Append("  error:     ").Append(lines[0].TrimEnd('\r')).Append('\n');
        foreach (var line in lines.Skip(1))
        {
            sb.Append("             ").Append(line.TrimEnd('\r')).Append('\n');
        }
    }

    /// <summary>An architecture the way the rest of the block writes it: <c>x64</c>, <c>x86</c>, <c>arm64</c>.</summary>
    private static string Arch(Architecture architecture) => architecture.ToString().ToLowerInvariant();

    /// <summary>A history record of the START setup and the session's final verdict (docs/04 section 6). The
    /// GUI's own clock is real (only the target is faked), so the caller's <paramref name="utcNow"/> is the
    /// true end time.</summary>
    internal static SessionRecord Record(SessionResult r, SessionSetup setup, DateTime utcNow) => new()
    {
        TargetPath = setup.TargetPath,
        MomentLocal = setup.Moment,
        TzBiasMin = setup.Zone.BiasMinutes,
        Mode = setup.Mode.Mode,
        Multiplier = setup.Mode.Multiplier,
        // Everything else that decides what the session DID, so repeating it repeats the session rather
        // than a partial copy of it.
        TargetArgs = setup.TargetArgs,
        WorkingFolder = setup.WorkingFolder,
        ScaleDuration = setup.ScaleDuration,
        ScaleQpc = setup.ScaleQpc,
        KeepBackgroundTimers = setup.KeepBackgroundTimers,
        GeckoSandboxOff = setup.GeckoSandboxOff,
        Force = setup.Force,
        Embedded = setup.ReachEmbedded,
        ElevatedEmbedded = setup.ReachElevated,
        Verdict = r.RecordedVerdict(),
        EndedAtUtc = utcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", CultureInfo.InvariantCulture),
    };

    /// <summary>The followed processes as the CLI report spells them (report.rs render_followed): the name and
    /// the pid, or the pid alone for one the process list could not name - the pid is what a reader looks
    /// for in the task manager to close a program that keeps the session open. The screen prints the same
    /// lines, so one formatter cannot disagree with itself.</summary>
    internal static IReadOnlyList<string> ProcessLines(IReadOnlyList<FollowedProcess> followed)
        => [.. followed.Select(p => string.IsNullOrEmpty(p.Image)
            ? "pid " + p.Pid.ToString(CultureInfo.InvariantCulture)
            : p.Image + " (pid " + p.Pid.ToString(CultureInfo.InvariantCulture) + ")")];

    /// <summary>
    /// A covered or observed channel as one line for the copy-summary, its read count folded in as prose so
    /// it does not read as a speed. The screen shows the same count in its own column of the audit table,
    /// in the same invariant digits. InvariantCulture keeps the count stable across machines, and
    /// CDP no longer prefixes a raw context id (P6): two contexts reading one channel differ by their count.
    /// </summary>
    private static string FormatReadRow(CoveredChannel channel, Func<string, string> translate)
    {
        // A count that stopped at the hook's ceiling is a floor, and says so (R5-WY11).
        var key = channel.Capped ? "coverage.reads_at_least"
            : channel.Calls == 1 ? "coverage.reads_one"
            : "coverage.reads";
        // Guarded like every other translated template here: a loose file with a bad placeholder falls back
        // to the default language instead of throwing out of the summary being built (rule 6).
        return Fmt(translate, key, channel.Channel, channel.Calls);
    }

    private static string Seconds(long ms) => (ms / 1000.0).ToString("0.0", CultureInfo.InvariantCulture);

    // Fill a translated template safely: a bad translation file falls back to the default language's
    // template, and to the raw one only when that fails too, instead of taking down the summary being
    // built. The rule lives in TextFormat, shared with the calculator and the window title (R4-N46).
    private static string Fmt(Func<string, string> translate, string key, params object[] args)
        => Localization.TextFormat.Translate(translate, key, args);

    /// <summary>
    /// The processes the hook never got into, one line per pid the way the CLI report prints them (pid,
    /// executable, role, the parent that spawned it). The screen groups them by executable because a reader
    /// scanning a table wants the shape, and the clipboard keeps the pids because a ticket about one runaway
    /// child wants the fact. A count past the named list is said in words, never left as a mismatch.
    /// </summary>
    private static void AppendUncoveredChildren(StringBuilder sb, SessionResult r, Func<string, string> translate)
    {
        if (r.UncoveredChildrenTotal <= 0)
        {
            return;
        }

        sb.Append("  ").Append(translate("report.processes_uncovered"))
          .Append(" (").Append(r.UncoveredChildrenTotal).Append("):\n");
        foreach (var child in r.UncoveredChildren)
        {
            // Empty like null: a path that ends in a separator names nothing, and the table already reads
            // an empty name as the unnamed row - the clipboard must not print a blank where it prints words.
            var image = string.IsNullOrEmpty(child.Image) ? translate("audit.process_unnamed") : child.Image;
            var line = child.Role is { Length: > 0 } role
                ? Fmt(translate, "report.process_line_role", child.Pid, image, role, child.ParentPid)
                : Fmt(translate, "report.process_line", child.Pid, image, child.ParentPid);
            sb.Append("    - ").Append(line).Append('\n');
        }

        if (r.UncoveredChildrenUnnamed > 0)
        {
            sb.Append("    - ").Append(Fmt(translate, "report.processes_more", r.UncoveredChildrenUnnamed)).Append('\n');
        }
    }

    /// <summary>
    /// The web engines the session reached inside the application, one line each, with the pid beside the
    /// port exactly as the CLI report prints them. The screen leaves the pid out - there is nothing on the
    /// screen to match it against, since the process table names executables rather than pids - but a
    /// ticket about one engine wants it, and a clipboard that says less than the CLI would be the same
    /// report disagreeing with itself.
    /// </summary>
    private static void AppendEngines(StringBuilder sb, SessionResult r, Func<string, string> translate)
    {
        if (r.Engines.Count == 0)
        {
            return;
        }

        sb.Append("  ").Append(translate(r.EnginePagesReached ? "report.engines_reached" : "report.engines_reached_none")).Append(":\n");
        for (var i = 0; i < r.Engines.Count; i++)
        {
            // One endpoint once, the same rule the table folds by. The screen deduplicated and this did
            // not, so an endpoint reported twice drew one row and printed TWO lines - one session
            // described two ways, which is the disagreement this whole slice exists to remove.
            if (!IsFirstMention(r.Engines, i))
            {
                continue;
            }

            var engine = r.Engines[i];
            // Whitespace like empty: an engine that named nothing gets the word, never a blank where the
            // report prints a name. The core sanitises that text but does not trim it.
            var name = string.IsNullOrWhiteSpace(engine.Browser) ? translate("audit.engine_unnamed") : engine.Browser;
            sb.Append("    - ").Append(Fmt(translate, "report.engine_line", name, engine.Port, engine.Pid)).Append('\n');
        }
    }

    /// <summary>
    /// Whether the engine at <paramref name="index"/> is the first mention of its endpoint - pid and port
    /// together, because one port number held by two processes in turn is two engines.
    /// </summary>
    /// <remarks>
    /// The rule is written twice, here and in <see cref="EngineRowsConverter"/>. What keeps the pair from
    /// drifting is a test that counts the engines on the screen and in the copied summary and requires the
    /// two numbers to agree.
    /// </remarks>
    private static bool IsFirstMention(IReadOnlyList<ReachedEngine> engines, int index)
    {
        for (var earlier = 0; earlier < index; earlier++)
        {
            if (engines[earlier].Pid == engines[index].Pid && engines[earlier].Port == engines[index].Port)
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>One labelled line, or nothing when there is no value to name - a line that says "browser:" and
    /// nothing after it claims a fact it does not have.</summary>
    private static void AppendValue(StringBuilder sb, Func<string, string> translate, string labelKey, string value)
    {
        if (value.Length > 0)
        {
            sb.Append("  ").Append(translate(labelKey)).Append(": ").Append(value).Append('\n');
        }
    }

    /// <summary>
    /// How long the machine slept, in the session's block (R5-D71). The CLI says it inside the warning line,
    /// and the summary's warnings are the window's texts, which carry no number - so the length goes where
    /// the session's other durations are, written as the screen writes them. A session with no timing still
    /// gets it, under its own heading, rather than a line hanging under nothing.
    /// </summary>
    private static void AppendSleep(StringBuilder sb, SessionResult r, Func<string, string> translate)
    {
        if (r.HostSleptMs <= 0)
        {
            return;
        }

        sb.Append(r.HasTiming ? "    " : "  " + translate("report.session") + ": ")
          .Append(Fmt(translate, "report.slept", ClockView.FormatDuration(r.HostSleptMs)))
          .Append('\n');
    }

    private static void AppendList(
        StringBuilder sb, Func<string, string> translate, string headerKey,
        IReadOnlyList<string> items, bool translateItems)
    {
        if (items.Count == 0)
        {
            return;
        }

        sb.Append("  ").Append(translate(headerKey)).Append(" (").Append(items.Count).Append("):\n");
        foreach (var item in items)
        {
            sb.Append("    - ").Append(translateItems ? translate(item) : item).Append('\n');
        }
    }
}

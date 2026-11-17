using System;
using System.IO;
using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The panel's mirror of the CLI's installer starter (crate::squirrel): it must find the program the core
/// starts in the starter's place, so the panel picks the core by that program's bitness and routes it by
/// that program's mechanism. The version cases are the file the CLI's test reads.
/// </summary>
public class SquirrelStarterTests
{
    private static TimeSpec AnyTime() => new()
    {
        Moment = new MomentSpec { Kind = "absolute", Local = "2038-01-19T03:14:07", TzBiasMin = 0 },
        Mode = "flow",
    };

    [Fact]
    public void The_newest_version_is_picked_as_the_shared_cases_say()
    {
        var cases = File.ReadAllLines(Path.Combine(TestPaths.RepoRoot(), "tests", "squirrel", "newest-version.txt"))
            .Select(line => line.Trim())
            .Where(line => line.Length > 0 && !line.StartsWith('#'))
            .ToList();
        Assert.True(cases.Count >= 20, $"the shared cases were read: {cases.Count}");
        foreach (var line in cases)
        {
            var parts = line.Split("=>");
            var picked = SquirrelStarter.NewestVersion(parts[0].Split(' ', StringSplitOptions.RemoveEmptyEntries)) ?? "-";
            Assert.True(string.Equals(parts[1].Trim(), picked, StringComparison.Ordinal), $"{line} - picked {picked}");
        }
    }

    [Fact]
    public void A_starter_starts_the_program_of_its_name_in_the_newest_finished_version_folder()
    {
        var dir = Scratch();
        try
        {
            var starter = Installed(dir, "Tool.exe", "app-3.6.9", "app-3.6.11", "app-3.7.0");
            File.WriteAllText(Path.Combine(dir, "app-3.7.0", ".not-finished"), string.Empty);
            File.WriteAllText(Path.Combine(dir, "app-9.0.0"), "a file, not a folder");
            Assert.Equal(Path.Combine(dir, "app-3.6.11", "Tool.exe"), SquirrelStarter.ProgramBehind(starter));
            Assert.Null(SquirrelStarter.ProgramBehind(Path.Combine(dir, "Update.exe")));

            // The newest version without the program: the starter would fail, it gets no older one.
            Directory.CreateDirectory(Path.Combine(dir, "app-4.0.0"));
            Assert.Null(SquirrelStarter.ProgramBehind(starter));
            Assert.Equal(starter, SquirrelStarter.SessionProgram(starter));
            Directory.Delete(Path.Combine(dir, "app-4.0.0"));

            File.Delete(Path.Combine(dir, "Update.exe"));
            Assert.Null(SquirrelStarter.ProgramBehind(starter));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// The case met on an installed application: a 32-bit starter in front of a 64-bit Electron program. The
    /// plan takes the program's bitness and mechanism, and sends the starter as chosen, for the core to find
    /// the same program. Without the updater beside it, the same file is planned as itself.
    /// </summary>
    [Fact]
    public void A_plan_on_a_starter_takes_the_bitness_and_the_mechanism_of_the_program_it_starts()
    {
        var dir = Scratch();
        try
        {
            var starter = Path.Combine(dir, "Tool.exe");
            File.Copy(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.SystemX86), "whoami.exe"), starter);
            File.WriteAllText(Path.Combine(dir, "Update.exe"), string.Empty);
            var version = Path.Combine(dir, "app-1.0.0");
            Directory.CreateDirectory(version);
            File.Copy(Path.Combine(Environment.SystemDirectory, "whoami.exe"), Path.Combine(version, "Tool.exe"));
            File.WriteAllText(Path.Combine(version, "icudtl.dat"), string.Empty);
            File.WriteAllText(Path.Combine(version, "snapshot_blob.bin"), string.Empty);

            var plan = SessionPlan.Build(starter, AnyTime());
            Assert.Equal((PeReader.Machine.X64, true), (plan.Machine, plan.IsCdp));
            Assert.Equal(starter, plan.Start.Target.Path);
            Assert.Null(plan.Start.Target.Cwd);

            File.Delete(Path.Combine(dir, "Update.exe"));
            var itself = SessionPlan.Build(starter, AnyTime());
            Assert.Equal((PeReader.Machine.X86, false), (itself.Machine, itself.IsCdp));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    private static string Scratch()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-squirrel-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        return dir;
    }

    /// <summary>A starter's folder: Update.exe, the starter, and the version folders named, each holding the
    /// program.</summary>
    private static string Installed(string dir, string starter, params string[] versions)
    {
        File.WriteAllText(Path.Combine(dir, "Update.exe"), "x");
        File.WriteAllText(Path.Combine(dir, starter), "x");
        foreach (var version in versions)
        {
            Directory.CreateDirectory(Path.Combine(dir, version));
            File.WriteAllText(Path.Combine(dir, version, starter), "x");
        }

        return Path.Combine(dir, starter);
    }
}

# Security Policy

## Supported versions

Chrono Mock has a single line of development. Security fixes are made against the
**latest release** and the `main` branch. Please confirm you can reproduce a
problem on the latest version before reporting it.

**Older releases receive nothing.** When a new version is published, the one before
it stops being supported that day: no security updates, no backports, no patched
builds. The supported version is whichever release is currently the latest, for as
long as it is the latest. There is no long-term support line and none is planned,
so the upgrade path for a security fix is always to move to the newest release.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Use GitHub's private vulnerability reporting: open the
[Security tab](https://github.com/donislawdev/ChronoMock/security/advisories/new)
of this repository and choose **Report a vulnerability**. That keeps the report
private until a fix is available.

Please include:

- the version and your Windows version,
- whether you used the window (`ChronoMock.exe`) or the command line (`chrono.exe`),
- whether it was native injection or Chromium mode, and whether the target was
  32-bit or 64-bit,
- a clear description and the smallest steps to reproduce, ideally one command line,
- the impact you believe it has.

You can expect an initial response within 14 days or fewer. Once a fix is ready it
ships in the next release, and the advisory is published crediting the reporter
unless you prefer to stay anonymous.

## What this tool does, and what that means for scope

**Chrono Mock injects a library into a program you point it at. That is the
product, not a vulnerability.** Everything below follows from that sentence, so
it is worth reading before writing a report.

The tool loads `chrono_hook.dll` into the target process and rewrites what that
one process reads from the Windows time APIs. This is the same technique malware
uses, which is why antivirus software flags it, and it is why a report saying "this
program injects code into other processes" describes the feature rather than a
defect. What is in scope is the tool doing that to a process **you did not choose**,
or reaching further than the process you did choose.

Three properties are worth stating because a report may depend on them:

**It never changes the system clock.** Not in any mode, not temporarily, not as a
fallback. The whole point is that the rest of the machine keeps the real time.
A guard in the test suite refuses any code in this repository that sets the clock,
and every guard here is pointed at code it must reject before it is trusted.

**It needs no administrator rights**, and asks for none. A Chrono Mock that
required elevation, or acquired it, would be a different and much more dangerous
tool. Note the corollary: run it elevated yourself and it can inject into elevated
processes, because Windows lets a process of equal integrity do that. That is the
operating system's model rather than a hole in this tool.

**It makes no connections off the machine.** No telemetry, no update check, nothing
downloaded while it runs. Guarded rather than promised: a test scans every Rust and
C# source in the workspace for network APIs, and the only ones it permits are listed
by file with the reason, so a new way out fails the build until somebody writes down
why it is there. The same test refuses a dependency that could speak to a network -
the tree is 51 packages and none of them can - and refuses a networking feature of
the `windows` crate.

A second test reads the built binaries themselves. Every Windows executable carries
an import table: the list of DLLs the loader resolves before the program runs, written
by the linker from what the code actually calls rather than from what anyone claims
about it. That test pins every module all six of our binaries link - five to seven
each - so a new one fails the build until somebody writes down why it is there. Two of
those entries are the point. The core links Winsock and nothing else that touches a
network, for the loopback debug port described below. The injected library links no
networking DLL at all, `ws2_32` included, even though intercepting `connect` is one of
its jobs: it looks that module up only when the target has already loaded it, so the
library that ends up inside somebody else's process has nothing to reach the network
with.

Neither layer is a proof of silence, and both say so in their own headers. A module
resolved by name while the program runs is invisible to the second, which is why the
first exists, and the managed GUI is invisible to it too, which is why the first scans
C# as well. There is one loopback exception and it is deliberate:
Chromium mode does not inject at all, and instead launches the browser with
`--remote-debugging-port=0` - Chromium picks a free port - reads the chosen port
from the profile that the tool created for the session, and drives the browser over
a local WebSocket. A port or profile flag supplied by the user is refused rather
than silently overridden, and the endpoint the tool connects to is checked to be
the loopback host and the port it was given. Both behaviours are covered by tests.

**It cleans up after itself, and cannot clean up after the target.** Chrono Mock
leaves no persistent hooks and no registry entries. It writes the session history
and a `diagnostics-*.log` beside itself, or under `%LOCALAPPDATA%\ChronoMock\` when
its own folder is not writable, and for a Chromium target a throwaway browser
profile under `%TEMP%` that is deleted when the session ends and reported in the
summary when it could not be. An application run in the future writes future dates
into its own data, and no tool can undo that - the README says so before you start.

### In scope

- A way to make Chrono Mock inject into a process the user did not select.
- A way for the tool to acquire privileges it was not started with.
- A way to make the audit report a channel as covered when it was not. Honest
  reporting is a function of this product rather than a nicety, so an audit that
  can be made to lie is a real vulnerability even when the substitution worked.
- A preset, calendar or session-history file that causes code execution, or that
  makes the tool write outside its own folder and `%LOCALAPPDATA%\ChronoMock\`.
- A way to reach the Chromium mode debug port from outside the machine, or for
  another local process to take over a session's browser through it.
- A crash or a hook failure that corrupts the target process rather than stopping
  the session.

### Not in scope

- **Injecting into a program you chose.** That is the product. So is loading the
  hook into the child processes that target spawns, which the session covers on
  purpose.
- **Antivirus flagging the injected library.** A documented false positive of the
  technique, explained in the README.
- **Damage the application under test does to its own data** when you run it at a
  future date, including a licence that invalidates itself on clock rollback. The
  README warns about this before your first session, and backing up the target's
  data directory is the mitigation.
- **What a user with administrator rights can do on their own machine.** Chrono
  Mock does not raise privileges, and it is not a sandbox.
- **Using the tool against software you are not responsible for.** That is a
  licensing and legal question rather than a vulnerability in this code, and it is
  addressed directly in the README.

## Verifying a download

From the release after v0.1.0, each one carries four things beside the archives:

- **`SHA256SUMS`** - the hash of every published file.
- **A bill of materials per package** (`*.spdx.json`, SPDX 2.3) listing every
  third-party component in that package with its version and licence.
- **An attestation of that bill of materials** (`*.sigstore.json`), signed by
  GitHub's infrastructure.
- **An Authenticode signature with an RFC 3161 timestamp** on every binary this
  project builds, including `chrono_hook.dll` - the library the tool injects into
  other processes, and so the file worth checking most.

The two verification commands are in [the README](README.md#download-and-run), and
they are run verbatim by a workflow every time a release is published, so a command
that stops working turns red here rather than in your terminal.

### What is signed and what is not

The window package ships around 240 assemblies that Microsoft already signed, and
they are left exactly as they arrived - re-signing somebody else's binary would both
destroy their signature and assert that we produced it. Two files in it
(`Wpf.Ui.dll`, `Wpf.Ui.Abstractions.dll`) are third-party and unsigned by their own
publisher, and they stay that way for the same reason. The bill of materials
declares all of them.

### What the attestation does and does not claim

It says what is inside the archive. It does **not** claim a workflow built it,
because none did: the archive is signed by a person on a machine holding a hardware
key that cannot be exported, which is the whole point of that key. The unsigned
build the workflow produced does carry build provenance, and the signing step
verifies it before touching anything - but those are different bytes, and saying
otherwise would be the one lie an attestation must never carry.

🔴 **This is why both commands pass `--predicate-type` explicitly.** The tooling asks
for build provenance by default. Without the flag, one spelling reports "no
attestation found" and another returns 404, and both look like a broken release when
nothing is wrong.

🔴 **The v0.1.0 archives have none of this.** They were built on a developer machine
and uploaded by hand, before the release moved into a workflow, and neither a
signature nor an attestation can be granted after the fact - which is precisely what
makes them worth having. For v0.1.0 the honest verification path remains building
from source: the project is GPL-3.0, `chrono_hook.dll` is built from the code in
this repository, and CI builds both the 64-bit and 32-bit targets on every push.

### If a release turns out to be broken

Releases are not edited in place. An asset that is already published has been
downloaded, and replacing it would break verification for everyone who has it while
taking the file off nobody's disk. So a bad release is **marked as a pre-release** -
which drops it from "latest", so the download button stops offering it - a line is
added at the top of its notes, and the fix ships as a new release through the same
ritual. The tag and the assets stay where they are.

## Secrets and permissions in this repository

**There are no repository secrets.** Measured on 2026-09-07: zero. Every workflow
runs on the per-job token GitHub issues for the run, and nothing else is stored
here.

**Access is scoped per workflow.** All five workflows declare `contents: read` at
the top. Two jobs raise anything on top of that: the CodeQL analysis takes
`security-events: write`, because writing results to the Security tab is the entire
point of the job, and the single job that publishes the website takes `pages: write`
and `id-token: write`.

**Every action is pinned to a commit** rather than to a tag somebody else can
repoint, with the release it corresponds to in a comment beside it. Dependabot
updates the SHA and the comment together, so pinning does not mean freezing.

**Dependencies are checked on every push.** The dependency gate runs four
`cargo-deny` checks - licences, advisories, bans and sources - and a licence that
cannot be identified blocks the build, because for a GPL-3.0 project that ships a
binary an unidentified licence is the one answer nobody can act on. CodeQL analyses
the code on the same schedule, and secret scanning with push protection is on.

## Code of conduct

Behaviour in this repository is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

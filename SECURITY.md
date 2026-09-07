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
the `windows` crate. It reads source rather than the built binaries, and it says so
in its own header along with the rest of what it cannot prove. There is one loopback
exception and it is deliberate:
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

## Downloads are not signed yet

**The released archives carry no Authenticode signature and no published
checksums.** Windows will show an unknown-publisher warning, and there is at
present no first-party file you can verify a download against. This is a known gap
rather than an oversight, code signing is planned, and this section will be
rewritten when it lands rather than quietly deleted.

Until then the honest verification path is to build from source. The project is
GPL-3.0, the injected library is `chrono_hook.dll` built from the code in this
repository, and the CI workflow builds both the 64-bit and 32-bit targets on every
push.

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

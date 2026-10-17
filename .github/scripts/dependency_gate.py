#!/usr/bin/env python3
"""Decide a pull request from GitHub's dependency review data.

    gh api "repos/$REPO/dependency-graph/compare/$BASE...$HEAD" > deps.json
    python .github/scripts/dependency_gate.py deps.json

Why this exists next to `actions/dependency-review-action`
----------------------------------------------------------
The action already fails a pull request that ADDS a dependency with a known
vulnerability, and it does that well. It cannot do the other half. Its own
documentation is explicit: if it cannot detect the licence for a dependency it
will say so, and the action will not fail.

For a GPL-3.0 project that ships a binary, an unidentified licence is not an
informational note - it is the one case where nobody can say whether the thing
may be distributed at all. So the same data is read here and an unknown licence
blocks, exactly like a denied one. The REST field is documented as string or
null, and null is what "not determined" looks like.

Where the allowed licences come from
------------------------------------
From `deny.toml`, and from nowhere else. That file is already the licence gate
for everything this project ships (`cargo deny check licenses`), and a second
list here would be a second source of truth for one question. This repository
has paid for that kind of drift before, in a metric ceiling that two files
disagreed about. Widening the gate therefore means editing `deny.toml`, which is
the same act rule 8 asks for anyway: a new dependency is a decision with a
reason, not a line in a workflow.

A consequence worth stating: that list is short on purpose, so a dependency
under a licence which is perfectly GPL-compatible but simply not used here yet -
BSD-3-Clause, for instance - will block. That is the intended answer. Blocking
means a person decides, not that the answer is no.

Scope
-----
Only dependencies that the pull request ADDS (`change_type == "added"`).
Removing something never creates an obligation, and re-checking what is already
in the tree would make every pull request answer for decisions taken years ago.
"""
import argparse
import json
import os
import sys
import tomllib

# Packages a person has already looked at: a licence GitHub cannot resolve, or
# an expression whose choices are not all allowed although the one taken is. A
# name here is a decision with a reason, not a way to make a red build green -
# and it names the package, never a whole ecosystem.
EXCEPTIONS = {
    # "MIT OR Apache-2.0 OR LGPL-2.1-or-later", and `verdict` reads an OR as
    # "every part", so the LGPL choice alone blocks it. It reaches the tree under
    # getrandom, under proptest, which only the tests build (the components guard
    # in crates/cli/tests/components.rs lists that tree as test-only), and
    # getrandom asks for it on UEFI targets only, which this project never
    # builds. Taken under MIT. Decided 2026-10-17, with the random properties.
    "r-efi": "a UEFI-only dependency of the test-only proptest tree, taken under MIT",
}


# 🔴 GitHub Actions are dependencies in this data too, and GitHub reports a null
# licence for every one of them. They are skipped, and the reason is not
# convenience. An action is CI machinery that never reaches a user, so it creates
# no distribution obligation - the thing an unknown licence is dangerous for.
# Keeping them would mean blocking every pull request that touches a workflow,
# for ever, and a gate that always fires is a gate people learn to bypass. What
# actions ARE checked for lives elsewhere and is stricter: every one is pinned to
# a commit SHA, and Dependabot moves the SHA and its version comment together.
SKIPPED_ECOSYSTEMS = frozenset({"actions"})

# The same reason for a manifest that only CI installs from, named one file at a
# time and never a directory, so a manifest added next to one of these is checked
# as usual. What such a file pins runs on a runner, reads this repository and
# reaches no user. The vulnerability half (dependency-review-action) still reads
# it, and the gate says in its log what it left out here, so nothing is skipped
# silently.
CI_ONLY_MANIFESTS = frozenset({
    # The scanner behind the Semgrep merge gate (security.yml), pinned in a file of
    # its own so Dependabot can propose the next version. LGPL-2.1-or-later, which
    # deny.toml does not allow and should not: that list also guards what the
    # binary ships.
    ".github/semgrep/requirements.txt",
})


def manifest_path(dependency):
    """The manifest a dependency came from, as a path from the repository root."""
    path = str(dependency.get("manifest", "")).replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path.lstrip("/")


def allowed_licences(deny_toml):
    """The `[licenses] allow` list out of deny.toml, as a set of SPDX ids."""
    with open(deny_toml, "rb") as handle:
        document = tomllib.load(handle)
    allow = ((document.get("licenses") or {}).get("allow")) or []
    if not allow:
        raise ValueError("deny.toml declares no [licenses] allow list")
    return frozenset(str(item).strip() for item in allow)


def added(review):
    return [d for d in review
            if str(d.get("change_type", "")) == "added"
            and str(d.get("ecosystem", "")).lower() not in SKIPPED_ECOSYSTEMS
            and manifest_path(d) not in CI_ONLY_MANIFESTS]


def added_for_ci_only(review):
    """What the pull request adds through a CI-only manifest - left out of the
    licence check, and named in the log for that reason."""
    return [d for d in review
            if str(d.get("change_type", "")) == "added"
            and manifest_path(d) in CI_ONLY_MANIFESTS]


def verdict(dependency, allowed):
    """``("ok" | "unknown" | "denied", licence)`` for one added dependency."""
    licence = dependency.get("license")
    name = str(dependency.get("name", "?"))
    if name in EXCEPTIONS:
        return "ok", licence
    if licence is None or not str(licence).strip():
        return "unknown", licence
    # A compound expression ("MIT OR Apache-2.0") passes only if every part is
    # allowed. Being generous with an OR would mean accepting the worse half.
    parts = [p.strip("() ") for p in str(licence).replace(" AND ", " OR ").split(" OR ")]
    if all(part in allowed for part in parts if part):
        return "ok", licence
    return "denied", licence


def split(review, allowed):
    blocked, passed = [], []
    for dependency in added(review):
        state, licence = verdict(dependency, allowed)
        row = (state, str(dependency.get("name", "?")),
               str(dependency.get("version", "?")), licence,
               str(dependency.get("scope", "?")))
        (passed if state == "ok" else blocked).append(row)
    return blocked, passed


def report_lines(blocked, passed, allowed):
    lines = []
    if blocked:
        lines.append("blocked - a licence that is denied or could not be determined:")
        for state, name, version, licence, scope in blocked:
            lines.append("  %-8s %s %s  licence=%s  scope=%s"
                         % (state, name, version, licence, scope))
        lines.append("")
        lines.append("An unknown licence blocks on purpose: this project is GPL-3.0 and ships a")
        lines.append("binary, so 'we could not tell' is the one answer nobody can act on.")
        lines.append("Allowed today, from deny.toml: %s" % ", ".join(sorted(allowed)))
        lines.append("Widen that list in deny.toml, or record the package in EXCEPTIONS here.")
    if passed:
        lines.append("allowed (%d): %s" % (
            len(passed), ", ".join("%s %s (%s)" % (n, v, lic) for _s, n, v, lic, _sc in passed)))
    lines.append("dependency gate: %d blocked, %d allowed" % (len(blocked), len(passed)))
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("review", help="JSON from the dependency review API")
    parser.add_argument("--deny", default=None,
                        help="path to deny.toml (default: the repository root)")
    args = parser.parse_args(argv)

    deny_toml = args.deny or os.path.join(
        os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
        "deny.toml")
    try:
        allowed = allowed_licences(deny_toml)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as exc:
        # A gate that cannot read its own source of truth has not passed anything.
        print("dependency gate: cannot read the allow list from %s: %s" % (deny_toml, exc),
              file=sys.stderr)
        return 2

    try:
        with open(args.review, encoding="utf-8") as handle:
            review = json.load(handle)
    except (OSError, ValueError) as exc:
        # The API answers 403 for some repository shapes, and that must look like
        # a failure rather than an empty list of problems.
        print("dependency gate: cannot read %s: %s" % (args.review, exc), file=sys.stderr)
        return 2
    if not isinstance(review, list):
        print("dependency gate: expected a list of dependencies, got %s"
              % type(review).__name__, file=sys.stderr)
        return 2

    blocked, passed = split(review, allowed)
    for dependency in added_for_ci_only(review):
        print("ci only, licence not checked: %s %s  licence=%s  manifest=%s"
              % (dependency.get("name", "?"), dependency.get("version", "?"),
                 dependency.get("license"), manifest_path(dependency)))
    for line in report_lines(blocked, passed, allowed):
        print(line)
    return 1 if blocked else 0


if __name__ == "__main__":
    raise SystemExit(main())

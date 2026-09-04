#!/usr/bin/env python3
"""Enforce the inward-only dependency rule from docs/the workspace layout.

The rule is stated in prose in the architecture and the inward-only dependency rule, which means
it is enforced by whoever happens to be reviewing. This makes it mechanical.

Checks direct dependencies from `cargo metadata`, not the resolved tree: a
transitive appearance of sqlx under `infra` is fine and expected, whereas a
domain crate *declaring* sqlx is the violation we care about.
"""

import json
import subprocess
import sys

CORE = "falkr-core"
EVENTS = "falkr-events"
DOMAIN = {
    "falkr-cost-spine",
    "falkr-research-graph",
}
INFRA = "falkr-infra"
BINARIES = {"api"}
# The golden-ledger corpus. It reads the domain in order to test it, so it is
# allowed to depend on anything; the constraint runs the other way.
TESTKIT = "falkr-testkit"

# Persistence libraries that must never be a direct dependency of the domain
# layer. Domain crates define traits; `infra` implements them.
PERSISTENCE = {"sqlx", "sea-orm", "sea-query", "sea-schema", "diesel", "tokio-postgres"}


def main() -> int:
    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        print("cargo metadata failed; the workspace manifests are not valid:", file=sys.stderr)
        print(proc.stderr.strip(), file=sys.stderr)
        return 1
    meta = json.loads(proc.stdout)

    pkgs = {p["name"]: p for p in meta["packages"]}
    workspace = set(pkgs)
    violations: list[str] = []

    def deps_of(name: str) -> set[str]:
        return {
            d["name"]
            for d in pkgs[name]["dependencies"]
            if d["kind"] is None  # normal deps only, not dev/build
        }

    def forbid(crate: str, banned: set[str], why: str) -> None:
        for bad in sorted(deps_of(crate) & banned):
            violations.append(f"{crate} -> {bad}: {why}")

    # core depends on nothing in this workspace.
    forbid(CORE, workspace - {CORE}, "core must depend on nothing in this workspace")

    # events depends only on core.
    forbid(
        EVENTS,
        workspace - {EVENTS, CORE},
        "events may depend only on falkr-core",
    )

    # Domain crates: core + events only, no persistence, no siblings.
    for crate in sorted(DOMAIN):
        forbid(
            crate,
            (workspace - {crate, CORE, EVENTS}),
            "domain crates may depend only on falkr-core and falkr-events",
        )
        forbid(
            crate,
            PERSISTENCE,
            "domain crates define traits; infra implements them",
        )

    # core itself must stay persistence-free by default. sqlx is allowed only as
    # an optional, non-default dependency (the `sqlx` feature that infra enables).
    for d in pkgs[CORE]["dependencies"]:
        if d["name"] in PERSISTENCE and d["kind"] is None and not d.get("optional"):
            violations.append(
                f"{CORE} -> {d['name']}: must be optional and off by default"
            )

    # infra implements domain traits; it must never depend on the binaries.
    forbid(INFRA, BINARIES, "infra must not depend on the binary crates")

    # Nothing depends on the testkit outside [dev-dependencies].
    #
    # `deps_of` filters to kind == None, i.e. normal dependencies only, so a
    # crate that lists falkr-testkit under [dev-dependencies] passes and one that
    # lists it under [dependencies] does not. That is the whole rule: the corpus
    # is a test artifact, and a production crate that reaches for it has either
    # put test code in a shipped binary or inverted the dependency direction so
    # that the thing being tested depends on its own tests.
    for crate in sorted(workspace - {TESTKIT}):
        forbid(
            crate,
            {TESTKIT},
            "nothing may depend on falkr-testkit outside [dev-dependencies]",
        )

    # Nothing depends on a binary crate.
    for crate in sorted(workspace - BINARIES):
        forbid(crate, BINARIES, "binary crates are leaves; nothing may depend on them")

    if violations:
        print("Dependency direction violations:\n", file=sys.stderr)
        for v in violations:
            print(f"  ✗ {v}", file=sys.stderr)
        print(
            f"\n{len(violations)} violation(s). Arrows point inward only.",
            file=sys.stderr,
        )
        return 1

    print(f"✓ dependency direction OK across {len(workspace)} workspace crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())

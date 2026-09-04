#!/usr/bin/env bash
#
# Scaffold a new golden-ledger corpus scenario (P02 §7).
#
#   scripts/corpus-new.sh <category> <slug> [phase]
#   scripts/corpus-new.sh --list-operations
#
# The generated file does not compile, run, or pass anything until a human has
# filled in the expected numbers and the prose derivation. That is deliberate:
# every placeholder below is a required field, so a scenario left half-written
# fails `cargo test -p falkr-testkit` rather than sitting in the corpus looking
# finished.
#
# What this script will NOT do, ever, is run the code to work out the expected
# trial balance. See corpus/README.md.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
corpus="$repo_root/corpus"

die() { printf '%s\n' "$*" >&2; exit 1; }

if [[ "${1:-}" == "--list-operations" ]]; then
    # The catalog in crates/testkit/src/catalog.rs is the single source of truth
    # for operation kinds and the phase that delivers each one. It is read here
    # rather than duplicated, so the two cannot drift; a kind that is not in the
    # catalog fails corpus validation.
    python3 - "$repo_root/crates/testkit/src/catalog.rs" <<'PYEOF'
import re, sys

source = open(sys.argv[1]).read()
# op( "kind", Phase::PNN, "summary" ) — the arguments may be on one line or
# spread over four, so match across newlines rather than line by line.
pattern = re.compile(
    r'\bop\(\s*"(?P<kind>[a-z_]+)"\s*,\s*Phase::P(?P<phase>\d+)\s*,\s*(?P<summary>"(?:[^"\\]|\\.)*")',
    re.S,
)
rows = [
    (int(m.group("phase")), m.group("kind"), m.group("summary")[1:-1])
    for m in pattern.finditer(source)
]
if not rows:
    sys.exit("could not parse catalog.rs; has OperationSpec changed shape?")
for phase, kind, summary in sorted(rows):
    print(f"  P{phase:02d}  {kind:<32}  {summary}")
PYEOF
    exit 0
fi

category="${1:-}"
slug="${2:-}"
phase="${3:-P03}"

[[ -n "$category" && -n "$slug" ]] || die \
"usage: scripts/corpus-new.sh <category> <slug> [phase]
       scripts/corpus-new.sh --list-operations

categories: $(cd "$corpus" && ls -d */ 2>/dev/null | grep -v '^_' | tr -d '/' | tr '\n' ' ')"

[[ "$category" =~ ^[a-z][a-z0-9-]*$ ]] || die \
    "category must be lower-case-kebab-case: '$category'"

[[ -d "$corpus/$category" ]] || die \
    "unknown category '$category'. Create $corpus/$category/ deliberately if it \
is genuinely a new one — the category list is also a coverage assertion in \
crates/testkit/tests/corpus.rs."

[[ "$slug" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] || die \
    "slug must be lower-case-kebab-case: '$slug'"

[[ "$phase" =~ ^P(0[2-9]|1[0-9]|2[01])$ ]] || die \
    "phase must be P02..P21: '$phase'"

# Scenario ids are <category>-NNN-<slug>, numbered in sequence within the
# category, so the next number is derived rather than guessed.
next=$(find "$corpus/$category" -name "$category-*.toml" -exec basename {} \; \
       | sed -E "s/^$category-([0-9]+)-.*/\1/" | sort -n | tail -1)
next=$(printf '%03d' $(( 10#${next:-0} + 1 )))

id="$category-$next-$slug"
path="$corpus/$category/$id.toml"
[[ -e "$path" ]] && die "$path already exists"

cat > "$path" <<EOF
[meta]
id             = "$id"
title          = "TODO: one line, what this scenario establishes"
# The paragraph the expected numbers come from. Required, and checked by a human,
# not by a string match — cite the paragraph you actually derived from.
standards      = ["TODO: e.g. ASC 606-10-55-46"]
phase          = "$phase"
# 1 (a two-line entry) to 5 (multi-entity, multi-currency, multi-period).
difficulty     = 3

[setup]
entity         = "US_PARENT"
book           = "US_GAAP"
functional_ccy = "USD"
period         = "2026-01"

# Operations run in order. \`scripts/corpus-new.sh --list-operations\` prints every
# kind and the phase that delivers it; an unknown kind fails validation, so a
# typo is a test failure rather than a silently deferred scenario.
[[operations]]
kind        = "post_entry"
date        = "2026-01-15"
description = "TODO"
source      = "MANUAL"
lines = [
    { account = "1000-Cash", debit = "0.00" },
    { account = "4000-Revenue", credit = "0.00" },
]

# Debit-positive, exact decimal strings, hand-computed. Must sum to zero.
[expected.trial_balance]
"1000-Cash"    = "0.00"
"4000-Revenue" = "0.00"

[expected.invariants]
balanced                 = true
replay_stable            = true
subledger_ties           = false
roll_forward             = false
intercompany_eliminated  = false

[expected.notes]
reasoning = """
TODO. Derive every number above from the cited standard, in prose, by hand.

A scenario whose expected numbers cannot be explained here is a scenario whose
expected numbers are guesses. Show the arithmetic, name the judgement, and say
what the wrong answer would be and why somebody would reach it.

Do NOT run the code and copy what it produced. That makes the corpus a snapshot
of current behaviour rather than a specification of correct behaviour, and it is
the single way this phase fails while appearing to succeed.
"""
EOF

printf 'created %s\n' "${path#"$repo_root"/}"
printf '\nnext:\n'
printf '  1. derive the expected numbers by hand and write the reasoning\n'
printf '  2. cargo test -p falkr-testkit --test corpus\n'

# 0002 — Delete the empty `falkr-ledger` crate

**Date:** 2026-08-28
**Status:** Accepted

## Context

`crates/ledger` was a stub: three crate-level `#![deny]` lints and a doc comment
saying the aggregate lived elsewhere. No types, no functions, no tests.

The ledger itself is real and tested — just not there:

- The aggregate, its commands, its events, and the balance-and-replay logic live
  in `crates/events/src/ledger.rs`.
- The Postgres persistence adapter lives in `crates/infra/src/ledger_store.rs`.

The empty crate was still listed in `[workspace.dependencies]`, still declared
as a dependency by `infra` and `api`, still named in `check-deps.py`'s `DOMAIN`
set and in the `ci.yml` persistence-resolve loop. Neither dependent ever wrote
`use falkr_ledger::…` — verified by grep across every `.rs` file before deleting.

The cost was not the two unused dependency edges. It was that a newcomer looking
for the ledger goes to `crates/ledger`, finds a stub, and concludes the ledger is
unbuilt — in a project whose README leads with the ledger.

## Decision

Delete `crates/ledger`. The ledger aggregate's home is `falkr-events`; its
persistence adapter's home is `falkr-infra`.

Explicitly **not** chosen: filling the crate in by moving the aggregate out of
`falkr-events`. The aggregate is correct where it sits, event-sourced state
belongs next to the event definitions that drive it, and two plausible homes for
"the ledger" is a worse outcome than one slightly surprising home.

## Consequences

- Removed from: root `Cargo.toml`; `crates/infra/Cargo.toml`;
  `crates/api/Cargo.toml`; `scripts/check-deps.py`; `.github/workflows/ci.yml`;
  and the architecture diagram in `README.md`.
- `cargo build --workspace` and `check-deps.py` both pass; the workspace went
  from 7 crates to 6. No test changed, because the crate had none — the suite is
  still 100 tests.
- Ledger-specific *reporting and projections* — trial balance, general-ledger
  detail — have no crate today. When they are built, the choice between a new
  crate and an `events` module gets made against real code rather than reserved
  now against a guess.

## Reversal criteria

If ledger logic grows enough that `falkr-events` stops being coherently "event
definitions and aggregates", split it out again — with content, in the same
commit.

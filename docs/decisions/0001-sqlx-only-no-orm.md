# 0001 — SQLx only; no ORM anywhere

**Date:** 2026-08-28
**Status:** Accepted

## Context

An earlier design for this codebase used two persistence libraries: SQLx for the
financial core, and SeaORM's dynamic entities for customer-extensible objects —
a metadata-driven layer where a user could add a custom field to a project or a
model without a migration and a redeploy.

That split was never implemented, and it is not going to be. SeaORM appears
nowhere in the workspace: not in `Cargo.toml`, not in a crate manifest, not in a
single `use`.

The reason it could not work is worth writing down, because "just use an ORM for
the CRUD-shaped parts" is a reasonable-sounding suggestion that will come up
again:

**The crates that were supposed to host the dynamic layer are crates that post
to the ledger.** Once a module produces journal entries, hiding its SQL is a
liability rather than a convenience — the whole premise here is that every
number traces to an immutable source event, and "show me exactly what ran"
should be answerable by reading the code rather than by reverse-engineering an
ORM's generated statement.

`scripts/check-deps.py` had already encoded this before anyone decided it
explicitly: `sea-orm`, `sea-query`, `sea-schema`, `diesel` and `tokio-postgres`
are all in its `PERSISTENCE` ban set, so a domain crate declaring any of them
fails the build.

## Decision

SQLx is the only persistence library. There is no ORM in this workspace and no
plan to add one.

The ban list in `scripts/check-deps.py` stays, and the CI `architecture` job
additionally proves that no domain crate even *resolves* `sqlx` transitively —
domain crates define traits, `infra` implements them.

Customer extensibility, if it is built, is a **typed custom-field mechanism**: a
field-definition table plus a constrained value representation, validated at the
boundary. Not runtime-mutable schema. A typed mechanism can be checked at the
edge and reasoned about in a migration; dynamic entities move those decisions to
runtime in exactly the code paths that touch money.

## Consequences

- One database library to learn, one query style to review.
- Adding a field is a migration. That is a real cost, and it is the intended
  trade: a migration is reviewable, and a runtime schema change is not.
- A PR introducing an ORM will fail `python3 scripts/check-deps.py` before it
  reaches review. That is deliberate — read this ADR rather than editing the ban
  list.

## Reversal criteria

A requirement that genuinely needs runtime-mutable schema, in a crate that
provably cannot reach a journal entry. It would need a new ADR, a change to
`check-deps.py`, and a demonstrated boundary — not just an argument that it
would be convenient.

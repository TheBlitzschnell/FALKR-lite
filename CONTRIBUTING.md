# Contributing to Falkr

Thanks for taking a look. Issues and pull requests are both welcome.

## Before you start

For anything larger than a bug fix, open an issue first. This codebase has a few
invariants that are load-bearing (see below), and it's much less frustrating to
agree on an approach before you've written the code.

Good places to start:

- **An additional cloud connector.** GCP, Azure and CoreWeave are each an
  implementation of `CostConnector`. The AWS one in
  `crates/cost-spine/src/providers/aws.rs` is the worked example.
- **An MLflow tracker.** `TrackerConnector` already has two implementations; the
  W&B one is real and the MLflow one is a mock waiting to be replaced.
- **Telemetry-based attribution.** The hardest and most valuable one — see the
  module docs in `crates/cost-spine/src/attribution/telemetry.rs`, which name the
  four things it needs, including an open policy question. Read
  [`docs/decisions/0003`](docs/decisions/0003-keep-telemetry-attribution-todo.md)
  first: decide the weighting policy and open an issue proposing it *before*
  writing the implementation, because that choice is what makes the numbers mean
  something.

Decisions that are already settled — and why — live in [`docs/decisions/`](docs/decisions/).
Read the relevant ADR before proposing to reverse one; each names its own
reversal criteria.

## The checks

All four must pass. CI runs the same set, so running them locally saves a round
trip:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --include-ignored
python3 scripts/check-deps.py
```

Integration tests need a container runtime. On Colima:

```bash
export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"
```

A full run is **100 passed, 0 failed, 0 ignored**. If the `ignored` count is
above zero, Docker was unreachable and the RLS suites silently did not run —
that is worse than a failure, because it looks like evidence.

## Invariants

These aren't style preferences — breaking one is a correctness bug, and most are
enforced by a lint or a test rather than by review.

1. **Money is never `f64`/`f32`.** Use `Money`, which wraps
   `rust_decimal::Decimal`. `#![deny(clippy::float_arithmetic)]` will stop you.
2. **Dimensions are populated at write time, never backfilled.** A `None` on an
   optional dimension means "not applicable", not "not yet known".
3. **Ledger balances change only through events.** There is no balance table,
   and a database trigger rejects `UPDATE`/`DELETE` on `ledger_events`. A
   correction is a reversing entry.
4. **Dependencies point inward.** Domain crates define traits; `infra`
   implements them. Domain crates never import `sqlx`. `scripts/check-deps.py`
   fails the build otherwise.
5. **Every external ingestion path is idempotent on the external ID.** Polling
   and webhooks both double-deliver.
6. **Every table gets row-level security.** A catalogue test fails if any table
   lacks RLS, `FORCE`, a policy, or a `tenant_id` column.

## Tests

Unit tests live beside the code; integration tests live in `crates/<name>/tests/`
and run against real PostgreSQL via testcontainers. Please don't mock the
database in a test whose subject is the database — row-level security is a
Postgres feature, and a mocked test of it proves nothing.

Where the risk is arithmetic — rating, apportionment, rounding — prefer a
property test over hand-picked examples. `proptest` is already a dev-dependency.

## Licence

Contributions are accepted under the AGPL-3.0-or-later terms in
[LICENSE](LICENSE).

# Falkr

[![CI](https://github.com/TheBlitzschnell/FALKR-lite/actions/workflows/ci.yml/badge.svg)](https://github.com/TheBlitzschnell/FALKR-lite/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Rust 1.95+](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)

**Cost attribution and double-entry accounting for AI infrastructure, in Rust.**

Answer *"what did this training run actually cost?"* — and be able to prove it.

Falkr ingests cloud billing data in [FinOps FOCUS](https://focus.finops.org/) format,
attributes every charge to the project, run, model and team that caused it, and
posts it to an append-only event-sourced ledger where every number traces back to
an immutable source event.

```
AWS Data Exports ──► CostEvent ──► attribution ──► Dimensions ──► Ledger
  (FOCUS/CUR 2.0)                       │                          (append-only)
                                        │
Weights & Biases ──► Run ───────────────┘
```

---

## Why this exists

Cloud cost tools tell you that you spent $1.4M on `AmazonEC2` last month. They
cannot tell you that $380K of it went to a fine-tuning run that was cancelled on
day three, because the information needed to say so was never captured at write
time.

For most companies that's an annoyance. For an AI company — where compute is the
dominant line item and a single experiment can outspend an entire department —
it's the difference between a budget and a guess.

Falkr's premise is that **cost attribution is an accounting problem, not a
reporting problem**. Every charge carries its full dimensional context from the
moment it's created:

```rust
pub struct Dimensions {
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub run_id: Option<RunId>,        // which training run
    pub team_id: TeamId,
    pub customer_id: Option<CustomerId>,
    pub model_id: Option<ModelId>,
    pub dataset_id: Option<DatasetId>,
    pub provider_id: ProviderId,      // AWS, GCP, CoreWeave, on-prem
    pub function_code: FunctionCode,  // COGS | R&D | OpEx
    pub commitment_id: Option<CommitmentId>,
}
```

Never backfilled. Backfilling means re-deriving attribution from invoices after
the fact, which is exactly the problem.

---

## What's in the box

| | |
|---|---|
| **FOCUS-normalized cost ingestion** | AWS Data Exports (CUR 2.0) connector. FOCUS is the FinOps Foundation's vendor-neutral billing schema, so `normalize()` is a column mapping rather than a bespoke translation layer per provider. |
| **Two-tier attribution** | Tag-based fast path from provider tags and Kubernetes labels. Telemetry-based fallback for shared GPU nodes is specified with a documented interface — see [Not built yet](#not-built-yet). |
| **Research object graph** | Experiment → Run → Job → Checkpoint → Model → DatasetVersion, mirroring the W&B/MLflow object model. Real Weights & Biases connector included. |
| **Event-sourced ledger** | Double-entry, append-only. Balances are projections replayed from the log — there is no balance table to `UPDATE`, and the database physically rejects `UPDATE`/`DELETE` on the event log. |
| **Exact decimal money** | `rust_decimal` in a `Money` newtype. No `f64` anywhere near a currency amount, enforced by a `#![deny(clippy::float_arithmetic)]` lint rather than by code review. |
| **Multi-tenant by construction** | Postgres row-level security on every table, `FORCE`d so it applies to the table owner too. A test queries `pg_catalog` and fails if *any* table lacks RLS, a policy, or a `tenant_id`. |
| **HTTP API** | Axum. Tenant identity comes from a verified credential, never from a request header. |

**100 tests**, including integration suites that run against real PostgreSQL in
throwaway containers — RLS is a Postgres feature, and a mocked test of it proves
nothing.

---

## Quick start

**Requirements:** Rust 1.95+, Docker (or Podman/Colima) for the test suite,
PostgreSQL 14+ to run the API.

```bash
git clone https://github.com/TheBlitzschnell/FALKR-lite.git
cd FALKR-lite

# Unit tests need nothing but Rust
cargo test --workspace

# Integration tests spin up throwaway PostgreSQL containers
cargo test --workspace -- --include-ignored
```

<details>
<summary><b>Using Colima instead of Docker Desktop?</b></summary>

Colima doesn't create `/var/run/docker.sock`, so point testcontainers at its socket:

```bash
export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"
```
</details>

### Running the API

```bash
cp .env.example .env          # then edit DATABASE_URL
export $(grep -v '^#' .env | xargs)
cargo run -p api
```

Migrations run automatically on startup.

```bash
curl -s localhost:8080/health

curl -s localhost:8080/v1/runs/$RUN_ID/cost \
  -H "Authorization: Bearer $FALKR_DEV_TOKEN"
# {"run_id":"…","attributed_cost":"58.9860","currency":"USD"}
```

Money is serialized as a **string**, never a JSON number — a JSON number is an
IEEE 754 double to most clients, which would silently undo exact decimal
arithmetic at the last possible moment.

---

## Architecture

Dependencies point **inward only**, and a CI check enforces it mechanically:

```
        ┌──────────────┐
        │     api      │   HTTP layer (binary)
        └──────┬───────┘
               │
   ┌───────────┴────────────┐
   │        infra           │   Postgres, SQLx, RLS, migrations
   └───────────┬────────────┘   (implements the traits below)
               │
   ┌───────────┴────────────────────────────┐
   │  ledger · cost-spine · research-graph  │   domain crates
   └───────────┬────────────────────────────┘   (define traits, no SQL)
               │
        ┌──────┴───────┐
        │    events    │   LedgerEvent, aggregates
        └──────┬───────┘
               │
        ┌──────┴───────┐
        │     core     │   Money, Dimensions, domain IDs
        └──────────────┘   (depends on nothing)
```

Domain crates never import `sqlx`. They define traits; `infra` implements them.
That keeps the ledger provably free of accidental infrastructure coupling and
makes the domain layer testable without a database.

```bash
python3 scripts/check-deps.py   # fails the build if an arrow points outward
```

### Design decisions worth knowing

<details>
<summary><b>Money is never a float</b></summary>

`f64` cannot represent `0.10`. In a ledger that error compounds silently across
millions of postings until a number is wrong in a way nobody can explain to an
auditor. `Money` wraps `rust_decimal::Decimal` and refuses arithmetic across
currencies at the type level.

Rounding defaults to **banker's rounding** (half-to-even), not half-up: this
system rounds millions of sub-cent usage charges, and half-up biases every
midpoint in the same direction until the error becomes a real number.
</details>

<details>
<summary><b>Balances are projections, not a table</b></summary>

Every posting is an immutable event. Balances are computed by replaying the log.
There's no balances table for a stray `UPDATE` to corrupt — and a database
trigger rejects `UPDATE` and `DELETE` on `ledger_events` outright, so a
correction *must* be a reversing entry.
</details>

<details>
<summary><b>Ingestion is idempotent, and knows restatement from duplication</b></summary>

AWS restates the current month's billing data repeatedly until the invoice
finalizes. "Already seen this row" therefore has two meanings: identical content
(a duplicate — ignore) and changed content (a restatement — apply). Collapsing
them means a corrected invoice silently fails to land. Every ingested row carries
a content hash, and the upsert reports `{inserted, restated, duplicate}`.
</details>

<details>
<summary><b>Row-level security, with the pooling caveat handled</b></summary>

Tenant isolation is enforced by Postgres, not by a `WHERE` clause someone might
forget. The tenant context is set with `set_config(..., is_local => true)` —
transaction-scoped, so it dies with the transaction and cannot leak to the next
caller through a pooled connection. `begin_tenant_tx` reads back the value
`set_config` returns and fails loudly if a pooler swallowed it.
</details>

---

## Not built yet

Stated plainly, because a README that implies more than exists wastes your time:

- **Telemetry-based attribution.** Tag-based attribution under-reports on shared
  GPU nodes (fractional GPU, MIG, time-slicing). The DCGM/Prometheus fallback has
  a defined interface and a documented gap — including a weighting-policy decision
  (utilization-seconds vs. memory-seconds vs. allocation-seconds) that materially
  changes the answer for a job that reserves a GPU and leaves it idle.
- **Connectors beyond AWS and W&B.** GCP, Azure, CoreWeave and MLflow are each an
  implementation of an existing trait; nothing else changes.
- **Authentication beyond opaque bearer tokens.** `TenantResolver` is a trait —
  an OIDC/JWT verifier slots in without touching handlers.
- **A tenant registry.** Every table carries a `tenant_id`, but nothing enumerates
  tenants, and enumerating them is precisely what RLS prevents. Scheduled
  cross-tenant work needs a `tenants` table plus a separate admin role with
  `BYPASSRLS`.

---

## Status

Early. The foundations — money, dimensions, the ledger, ingestion, tenancy — are
built and tested. The API surface is deliberately small and will grow.

**Issues and PRs welcome.** Good first areas: an additional cloud connector, an
MLflow tracker, or the telemetry attribution path.

## Contributing

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --include-ignored
python3 scripts/check-deps.py
```

All four must pass. CI runs the same set.

## License

[GNU AGPL v3](LICENSE) or later.

You can run, modify and self-host this freely. The network clause means that if
you offer a modified version to others over a network, you have to publish those
modifications. If that doesn't suit your situation, get in touch.

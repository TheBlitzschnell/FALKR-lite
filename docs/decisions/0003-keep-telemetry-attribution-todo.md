# 0003 — Keep the `todo!()` in the telemetry attribution path

**Date:** 2026-08-28
**Status:** Accepted

## Context

`crates/cost-spine/src/attribution/telemetry.rs` defines `TelemetryAttributor` —
the fallback that splits a shared GPU node's cost across the workloads that
actually ran on it, for the fractional-GPU / MIG / time-slicing cases where
tag-based attribution under-reports.

The trait and its data shapes are written. The DCGM-over-Prometheus
implementation is not: `DcgmPrometheusAttributor::apportion_node_cost` is a
`todo!()` carrying a message naming the four missing pieces. `README.md` lists
this under "Not built yet" and `CONTRIBUTING.md` names it as the hardest and
most valuable place to contribute.

This ADR answers the obvious review question: should a library crate ship a
panicking placeholder at all?

Three things bear on it:

1. **It is unreachable.** `DcgmPrometheusAttributor` is never constructed
   anywhere in the workspace outside its own module — verified by grep across
   every `.rs` file. No ingestion path and no API handler calls
   `apportion_node_cost`. Reaching the panic requires deliberately constructing
   a backend whose own doc comment says it is not implemented.
2. **The sibling already covers the runtime case.** `UnavailableTelemetry`
   returns `AttributionError::TelemetryUnavailable`, which is correct for a
   deployment with no telemetry wired up. The two types are not redundant: one
   means "not configured", the other means "not built".
3. **The alternatives are worse.** Returning `Ok(vec![])` would record a shared
   node's cost as attributable to nobody, silently — the exact failure this
   module exists to prevent. Returning `Err(TelemetryUnavailable)` would make
   "we never wrote this" indistinguishable from "this deployment has no
   Prometheus", and the attribution-coverage metric keys off that distinction.

The blocking item is not engineering effort. It is item 3 of the module's list:
**a weighting-policy decision.** Utilization-seconds, memory-seconds and
allocated-fraction-seconds give materially different answers for a job that
reserves a whole GPU and leaves it idle, and choosing between them is a question
about whether idle reserved capacity is charged to the reserver or to overhead.
Implementing before that is settled encodes an accidental policy in arithmetic.

## Decision

Keep the `todo!()`. The module docs stay as the specification for whoever
implements it.

**If you are picking this up as a contribution:** decide the weighting policy
first, open an issue proposing it with the reasoning, and make it configurable
rather than a constant. The apportionment arithmetic itself is already written
and tested — `crate::pricing::apportion` conserves the total exactly under any
weights — so once the weights are decided, the split is a solved problem.

## Consequences

- A panicking placeholder stays in a library crate. Accepted because it is
  unreachable by construction, and because the type system carries the gap
  rather than a tracking ticket that goes stale.
- Anyone wiring telemetry attribution hits a panic listing what is missing,
  rather than a plausible-looking wrong number. That is the intended failure
  mode.
- `clippy::unwrap_used` / `expect_used` do not cover `todo!()`, so no lint
  suppression is involved and none should be added.

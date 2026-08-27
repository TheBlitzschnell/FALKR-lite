//! # falkr-events
//!
//! Event definitions and the aggregate shape every financial mutation in this
//! workspace goes through. Depends only on `falkr-core` — and on `esrs` with
//! default features, which pulls no persistence library, so this crate stays on
//! the domain side of the workspace layout.
//!
//! ## Why `esrs`
//!
//! Evaluated against `cqrs-es` before committing. `esrs` won on three counts
//! that matter here:
//!
//! - Its `Aggregate` trait is exactly the shape we want —
//!   `handle_command(&State, Command) -> Result<Vec<Event>, Error>` is pure and
//!   synchronous, and `apply_event(State, Event) -> State` is infallible *by
//!   construction* rather than by convention. `cqrs-es` 0.5 moved to
//!   `async fn handle(&mut self, …, sink)`, which invites I/O inside command
//!   validation.
//! - It is SQLx-native, matching the rest of the stack, and cleanly
//!   feature-gated so this crate resolves without `sqlx`.
//! - Its transactional event handlers receive `&mut Transaction`, so a balance
//!   projection commits in the same transaction as the event append and cannot
//!   drift from the log after a crash.
//!
//! ## One deviation from the docs, deliberately
//!
//! `esrs`'s own `PgStore` opens its own transaction (`self.inner.pool.begin()`),
//! which means the tenant GUC that row-level security reads is never set and
//! every ledger write is refused. `infra` therefore implements `esrs`'s
//! `EventStore` trait over a tenant-scoped transaction instead of using
//! `PgStore`. The aggregate plumbing worth not hand-rolling — the
//! `Aggregate` shape, `AggregateState`, sequence numbering, optimistic locking —
//! all still comes from `esrs`; only the storage adapter is ours.

#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod ledger;

pub use ledger::{
    JournalLine, LedgerAggregate, LedgerCommand, LedgerError, LedgerEvent, LedgerState,
};

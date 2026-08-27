// Required at the crate root of every crate where Money math happens. Also
// enforced workspace-wide via [workspace.lints] in Cargo.toml.
#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

//! # falkr-ledger
//!
//! Event-sourced double-entry general ledger with function-code tagging
//!.
//!
//! **Absolute rule:** `LedgerEvent`s are the only way a balance changes. No
//! code path in this crate — or any other — issues an `UPDATE` against a
//! balance table. Balances are projections, rebuilt from the event stream.
//!
//! Persistence is SQLx, but the SQL lives in `infra`: this crate
//! defines traits, `infra` implements them.
//!
//! Currently a thin crate: the ledger aggregate itself lives in `falkr-events`
//! and its Postgres store in `falkr-infra`. Ledger-specific projections and
//! reporting will land here.

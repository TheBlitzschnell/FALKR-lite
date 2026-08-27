//! Re-export of the shared fixture from `falkr-infra`'s `test-support` feature.
//!
//! Kept as a thin alias so the suites in this crate read the same as those in
//! `api` and `worker`, which import it directly.

pub use falkr_infra::test_support::setup;

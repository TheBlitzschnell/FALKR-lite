//! Seeded demo data.
//!
//! One definition of "a plausible small AI company's books", used by the corpus
//! and by any future `--demo` mode of a user interface. Two definitions would
//! mean the demo shows numbers the tests never check, which is how a demo grows
//! a bug that no test can see.
//!
//! Everything here is a pure function of an explicit seed. `demo()` with the
//! same seed produces byte-identical data on every machine, forever; there is no
//! `Utc::now()`, no `Uuid::new_v4()` and no unordered iteration in this module.

use falkr_core::Currency;

use crate::generator::{GeneratedLedger, GeneratorSpec, generate};

/// The seed the demo data ships with.
///
/// A constant rather than a caller's choice: screenshots, documentation and
/// support conversations all refer to the same numbers, and "which seed was
/// that?" is a question nobody should have to answer.
pub const DEMO_SEED: u64 = 0x0FA1_C0DE_2026;

/// Accounts the demo posts to. Codes must exist in
/// `corpus/_reference/accounts.toml`.
pub const DEMO_ACCOUNTS: &[&str] = &[
    "1000", // Cash
    "1100", // Accounts Receivable
    "1400", // Prepaid Expenses
    "1500", // Compute Equipment
    "2000", // Accounts Payable
    "2400", // Deferred Revenue
    "4000", // Revenue
    "5000", // Cost of Revenue — Inference Compute
    "6100", // Research and Development
];

/// A demo ledger: one quarter's worth of small entries in USD.
#[must_use]
pub fn demo() -> GeneratedLedger {
    demo_with_seed(DEMO_SEED)
}

/// The demo ledger from an explicit seed, for tests that want several.
#[must_use]
pub fn demo_with_seed(seed: u64) -> GeneratedLedger {
    generate(seed, &demo_spec())
}

/// The demo generator specification.
#[must_use]
pub fn demo_spec() -> GeneratorSpec {
    GeneratorSpec {
        accounts: DEMO_ACCOUNTS.iter().map(|s| (*s).to_owned()).collect(),
        currency: Currency::Usd,
        entries: 120,
        min_lines: 2,
        max_lines: 5,
        // $50,000.00 — large enough to look like a real month, small enough that
        // the totals are readable on a screenshot.
        max_minor_units: 5_000_000,
        scale: 2,
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    #[test]
    fn the_demo_ledger_is_stable_and_balanced() {
        let a = demo();
        let b = demo();
        assert_eq!(a, b, "the demo ledger must not vary between calls");
        let total: Decimal = a.ground_truth().values().sum();
        assert_eq!(total, Decimal::ZERO);
    }

    #[test]
    fn demo_accounts_are_distinct() {
        let mut sorted = DEMO_ACCOUNTS.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len());
    }
}

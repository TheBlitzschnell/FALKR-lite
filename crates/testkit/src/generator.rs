//! Deterministic scenario generation from a seed.
//!
//! Borrowed in shape from **FinBalance** (arXiv:2606.15949): a human-authored
//! specification layer, a sampler that draws from it, and a fixed double-entry
//! ledger that computes the ground truth. The value is in the separation — the
//! ground truth is computed by [`GeneratedLedger::ground_truth`], a fifteen-line
//! accumulator with no aggregate, no event store and no projection in it, so a
//! property test comparing it against [`falkr_events::LedgerAggregate`] is
//! comparing two independent implementations rather than one implementation
//! against itself.
//!
//! ## The PRNG is ours on purpose
//!
//! This module implements `splitmix64` rather than depending on `rand`. Two
//! reasons, and the second is the real one:
//!
//! 1. It is nine lines, against a dependency tree.
//! 2. **A generator whose output changes when a dependency is upgraded is not
//!    reproducible.** `rand`'s stream is explicitly not covered by semver, so a
//!    minor bump can silently change every generated ledger — and a corpus that
//!    cannot reproduce yesterday's failing case is a corpus you cannot bisect.
//!    Owning nine lines fixes the stream forever.

use std::collections::BTreeMap;

use falkr_core::{AccountId, Currency, Money};
use falkr_events::JournalLine;
use rust_decimal::Decimal;

use crate::runner::account_id;

/// `splitmix64`. Fixed forever; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. Returns 0 for `n == 0` rather than dividing by zero.
    pub const fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A value in `low..=high`.
    pub const fn between(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            low
        } else {
            low + self.below(high - low + 1)
        }
    }
}

/// The human-authored specification layer: what a generated ledger is allowed
/// to contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratorSpec {
    /// Account codes the generator may post to. Codes, not labels.
    pub accounts: Vec<String>,
    pub currency: Currency,
    pub entries: usize,
    /// Inclusive bounds on lines per entry. The minimum is clamped to 2 — a
    /// one-line entry cannot balance.
    pub min_lines: usize,
    pub max_lines: usize,
    /// Largest single line amount, in minor units at `scale`.
    pub max_minor_units: u64,
    /// Decimal places. 0 for JPY, 2 for USD, 3 for BHD — the three cases a
    /// home-grown ledger gets wrong.
    pub scale: u32,
}

impl GeneratorSpec {
    /// A small, safe default: eight accounts, USD, two decimals.
    #[must_use]
    pub fn small() -> Self {
        Self {
            accounts: vec![
                "1000".to_owned(),
                "1100".to_owned(),
                "1400".to_owned(),
                "2000".to_owned(),
                "2400".to_owned(),
                "4000".to_owned(),
                "5000".to_owned(),
                "6000".to_owned(),
            ],
            currency: Currency::Usd,
            entries: 25,
            min_lines: 2,
            max_lines: 6,
            max_minor_units: 5_000_000,
            scale: 2,
        }
    }

    #[must_use]
    pub const fn with_currency(mut self, currency: Currency, scale: u32) -> Self {
        self.currency = currency;
        self.scale = scale;
        self
    }
}

/// One generated entry. Balanced by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedEntry {
    pub lines: Vec<JournalLine>,
}

/// A generated ledger, with its own independently computed balances.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedLedger {
    pub seed: u64,
    pub currency: Currency,
    pub entries: Vec<GeneratedEntry>,
}

impl GeneratedLedger {
    /// Account balances computed *without* the aggregate, under the
    /// debit-positive convention.
    ///
    /// Deliberately trivial. Its job is to be so obviously correct that a
    /// disagreement with the aggregate implicates the aggregate.
    #[must_use]
    pub fn ground_truth(&self) -> BTreeMap<AccountId, Decimal> {
        let mut balances: BTreeMap<AccountId, Decimal> = BTreeMap::new();
        for entry in &self.entries {
            for line in &entry.lines {
                let delta = match (line.debit, line.credit) {
                    (Some(d), None) => d.amount(),
                    (None, Some(c)) => -c.amount(),
                    _ => continue,
                };
                let slot = balances.entry(line.account).or_insert(Decimal::ZERO);
                *slot = slot.checked_add(delta).unwrap_or(*slot);
            }
        }
        balances
    }

    /// Every line across every entry, flattened.
    pub fn all_lines(&self) -> impl Iterator<Item = &JournalLine> {
        self.entries.iter().flat_map(|e| e.lines.iter())
    }
}

/// Generates a balanced ledger from a seed.
///
/// Each entry is built by drawing `n - 1` random lines and making the last one
/// the balancing plug. That guarantees balance by construction rather than by
/// rejection sampling, which matters because a generator that discards
/// unbalanced draws would never produce the large-magnitude entries that stress
/// the summation.
#[must_use]
pub fn generate(seed: u64, spec: &GeneratorSpec) -> GeneratedLedger {
    let mut rng = Rng::new(seed);
    let mut entries = Vec::with_capacity(spec.entries);
    let account_count = spec.accounts.len().max(2) as u64;
    let min_lines = spec.min_lines.max(2) as u64;
    let max_lines = (spec.max_lines as u64).max(min_lines);

    for _ in 0..spec.entries {
        let line_count = rng.between(min_lines, max_lines);
        let mut lines: Vec<JournalLine> = Vec::with_capacity(line_count as usize);
        let mut running = Decimal::ZERO;

        for _ in 0..line_count.saturating_sub(1) {
            let index = rng.below(account_count) as usize;
            let Some(code) = spec.accounts.get(index) else {
                continue;
            };
            let minor = rng.between(1, spec.max_minor_units.max(1));
            let magnitude = Decimal::new(i64::try_from(minor).unwrap_or(i64::MAX), spec.scale);
            let money = Money::new(magnitude, spec.currency);
            let id = account_id(code);
            if rng.next_u64().is_multiple_of(2) {
                running = running.checked_add(magnitude).unwrap_or(running);
                lines.push(JournalLine::debit(id, money));
            } else {
                running = running.checked_sub(magnitude).unwrap_or(running);
                lines.push(JournalLine::credit(id, money));
            }
        }

        // The plug. `running` is the signed sum so far; the closing line is its
        // negation, which is what makes the entry balance exactly.
        let index = rng.below(account_count) as usize;
        if let Some(code) = spec.accounts.get(index) {
            let id = account_id(code);
            let money = Money::new(running.abs(), spec.currency);
            if running.is_sign_negative() {
                lines.push(JournalLine::debit(id, money));
            } else {
                lines.push(JournalLine::credit(id, money));
            }
        }

        entries.push(GeneratedEntry { lines });
    }

    GeneratedLedger {
        seed,
        currency: spec.currency,
        entries,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "the no-unwrap rule permits unwrap in tests"
    )]

    use crate::assertions::assert_balanced;

    use super::*;

    #[test]
    fn the_same_seed_produces_the_same_ledger() {
        let spec = GeneratorSpec::small();
        assert_eq!(generate(42, &spec), generate(42, &spec));
        assert_ne!(generate(42, &spec), generate(43, &spec));
    }

    #[test]
    fn every_generated_entry_balances() {
        let spec = GeneratorSpec::small();
        for seed in 0..64 {
            let ledger = generate(seed, &spec);
            for entry in &ledger.entries {
                assert!(
                    assert_balanced(&entry.lines).is_ok(),
                    "seed {seed} produced an unbalanced entry"
                );
            }
        }
    }

    #[test]
    fn the_generated_trial_balance_is_zero() {
        let ledger = generate(7, &GeneratorSpec::small());
        let total: Decimal = ledger.ground_truth().values().sum();
        assert_eq!(total, Decimal::ZERO);
    }

    #[test]
    fn zero_decimal_currencies_never_produce_a_fraction() {
        let spec = GeneratorSpec::small().with_currency(Currency::Jpy, 0);
        let ledger = generate(11, &spec);
        for line in ledger.all_lines() {
            let amount = line.signed_amount().unwrap().amount();
            assert_eq!(amount.fract(), Decimal::ZERO, "JPY line had a fraction");
        }
    }

    #[test]
    fn the_prng_stream_is_pinned() {
        // If this changes, every generated ledger changes and no previously
        // recorded failing seed reproduces. Pinned so that is a deliberate act.
        let mut rng = Rng::new(0);
        assert_eq!(
            [rng.next_u64(), rng.next_u64(), rng.next_u64()],
            [
                16_294_208_416_658_607_535,
                7_960_286_522_194_355_700,
                487_617_019_471_545_679
            ]
        );
    }
}

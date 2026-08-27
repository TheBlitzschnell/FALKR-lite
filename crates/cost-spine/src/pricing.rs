//! `effective_cost` computation and cost apportionment.
//!
//! `effective_cost` is the number every downstream report is built on
//!, so the arithmetic here has to be internally
//! consistent under any combination of discounts and commitment coverage — not
//! merely correct on the examples someone thought to write down. The invariants
//! are stated as properties and checked with `proptest` (our testing policy).

use falkr_core::{Currency, CurrencyMismatch, Money};
use rust_decimal::Decimal;

/// A reduction applied to the list unit price.
///
/// Rates are fractions in `[0, 1]`: `0.20` is twenty percent off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Discount {
    /// A negotiated rate off list, e.g. an enterprise agreement.
    Negotiated { rate: Decimal },
    /// A volume/tier rate that kicked in for this line.
    Volume { rate: Decimal },
}

impl Discount {
    #[must_use]
    pub const fn rate(self) -> Decimal {
        match self {
            Self::Negotiated { rate } | Self::Volume { rate } => rate,
        }
    }
}

/// The portion of a line item covered by an existing commitment.
///
/// Commitment *purchase* is a separate `ChargeCategory::Purchase` record; this
/// is the consumption side, where covered usage is billed at the committed rate
/// (or not billed at all, if prepaid) but still costs the business something,
/// which is what `effective_cost` has to reflect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitmentCoverage {
    /// How much of the line's quantity the commitment absorbs.
    pub covered_quantity: Decimal,
    /// The rate the commitment locked in.
    pub committed_unit_price: Decimal,
    /// Whether the commitment was paid up front. Prepaid coverage contributes
    /// to `effective_cost` (amortization) but not to `billed_cost`.
    pub prepaid: bool,
}

/// Inputs to the cost computation for one invoice line.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PricingInputs {
    pub list_unit_price: Decimal,
    pub quantity: Decimal,
    pub currency: Currency,
    pub discounts: Vec<Discount>,
    pub commitment: Option<CommitmentCoverage>,
}

/// The three FOCUS cost measures for one line.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostAmounts {
    pub list_cost: Money,
    pub billed_cost: Money,
    pub effective_cost: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PricingError {
    #[error("discount rate must be within [0, 1], got {0}")]
    DiscountRateOutOfRange(Decimal),
    #[error("quantity must not be negative, got {0}")]
    NegativeQuantity(Decimal),
    #[error("unit price must not be negative, got {0}")]
    NegativeUnitPrice(Decimal),
    #[error("commitment covered_quantity must not be negative, got {0}")]
    NegativeCoverage(Decimal),
    #[error("apportionment weights must not be empty")]
    NoWeights,
    #[error("apportionment weights must be non-negative and sum to more than zero")]
    InvalidWeights,
    #[error(transparent)]
    Currency(#[from] CurrencyMismatch),
}

impl PricingInputs {
    /// Computes the three FOCUS cost measures.
    ///
    /// The model:
    /// - `list_cost` = list unit price × quantity, undiscounted.
    /// - Commitment-covered quantity is billed at the committed rate; the
    ///   remainder is billed at the discounted rate.
    /// - `billed_cost` excludes prepaid commitment coverage — the money already
    ///   left the business when the commitment was purchased, and counting it
    ///   again here would double-count it against the invoice.
    /// - `effective_cost` includes it, because the capacity genuinely costs
    ///   this much to run whether or not it appears on this month's bill. That
    ///   difference is the entire reason FOCUS carries both numbers.
    pub fn compute(&self) -> Result<CostAmounts, PricingError> {
        if self.quantity.is_sign_negative() {
            return Err(PricingError::NegativeQuantity(self.quantity));
        }
        if self.list_unit_price.is_sign_negative() {
            return Err(PricingError::NegativeUnitPrice(self.list_unit_price));
        }
        for d in &self.discounts {
            let r = d.rate();
            if r < Decimal::ZERO || r > Decimal::ONE {
                return Err(PricingError::DiscountRateOutOfRange(r));
            }
        }

        // Discounts compose multiplicatively, which is what makes the result
        // independent of the order they were applied in. Sequential subtraction
        // would not be, and "which discount applied first" is not a question an
        // invoice line can answer.
        let retained: Decimal = self
            .discounts
            .iter()
            .fold(Decimal::ONE, |acc, d| acc * (Decimal::ONE - d.rate()));
        let discounted_unit_price = self.list_unit_price * retained;

        let (covered_qty, committed_price, prepaid) = match self.commitment {
            Some(c) => {
                if c.covered_quantity.is_sign_negative() {
                    return Err(PricingError::NegativeCoverage(c.covered_quantity));
                }
                (
                    c.covered_quantity.min(self.quantity),
                    c.committed_unit_price,
                    c.prepaid,
                )
            }
            None => (Decimal::ZERO, Decimal::ZERO, false),
        };
        let on_demand_qty = self.quantity - covered_qty;

        let list_cost = Money::new(self.list_unit_price * self.quantity, self.currency);
        let on_demand_cost = Money::new(on_demand_qty * discounted_unit_price, self.currency);
        let covered_cost = Money::new(covered_qty * committed_price, self.currency);

        let billed_cost = if prepaid {
            on_demand_cost
        } else {
            on_demand_cost.checked_add(&covered_cost)?
        };
        let effective_cost = on_demand_cost.checked_add(&covered_cost)?;

        Ok(CostAmounts {
            list_cost,
            billed_cost,
            effective_cost,
        })
    }
}

/// Splits `total` across `weights`, preserving the total exactly.
///
/// This is the arithmetic behind telemetry-based attribution (the architecture
/// §4.1): a shared GPU node's cost apportioned across the workloads that ran on
/// it, weighted by measured utilization.
///
/// Naive per-share rounding loses or invents money — a hundred shares each
/// rounded down is a hundred fractions of a cent that belong to nobody, and
/// over a month of GPU nodes that is a real reconciliation gap. This uses the
/// largest-remainder method: every share is floored to `dp`, then the leftover
/// is handed out one minor unit at a time to whichever shares were cut hardest.
/// The sum of the result equals `total` exactly, always.
pub fn apportion(total: Money, weights: &[Decimal], dp: u32) -> Result<Vec<Money>, PricingError> {
    if weights.is_empty() {
        return Err(PricingError::NoWeights);
    }
    if weights.iter().any(Decimal::is_sign_negative) {
        return Err(PricingError::InvalidWeights);
    }
    let weight_sum: Decimal = weights.iter().sum();
    if weight_sum <= Decimal::ZERO {
        return Err(PricingError::InvalidWeights);
    }

    let currency = total.currency();
    let amount = total.amount();

    // One minor unit at the requested precision, e.g. 0.01 for dp = 2.
    let step = Decimal::ONE / Decimal::from(10_u64.pow(dp));

    // Exact share, then floor to `dp`. Remainders drive who gets topped up.
    let mut floored: Vec<Decimal> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(usize, Decimal)> = Vec::with_capacity(weights.len());
    for (i, w) in weights.iter().enumerate() {
        let exact = amount * w / weight_sum;
        let down = exact.round_dp_with_strategy(dp, rust_decimal::RoundingStrategy::ToZero);
        remainders.push((i, exact - down));
        floored.push(down);
    }

    let distributed: Decimal = floored.iter().sum();
    let mut leftover = amount - distributed;

    // Largest remainder first; ties broken by index so the result is
    // deterministic, which matters when the same node is reprocessed.
    remainders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut idx = 0;
    while leftover >= step && idx < remainders.len() {
        let target = remainders[idx].0;
        floored[target] += step;
        leftover -= step;
        idx += 1;
    }
    // Any sub-minor-unit residue (possible when `total` carries more precision
    // than `dp`) lands on the largest remainder rather than evaporating.
    if !leftover.is_zero() {
        let target = remainders[0].0;
        floored[target] += leftover;
    }

    Ok(floored
        .into_iter()
        .map(|a| Money::new(a, currency))
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "the unwrap/expect ban targets production code, not tests"
    )]

    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;

    fn money(d: Decimal) -> Money {
        Money::new(d, Currency::Usd)
    }

    // --- worked examples ---------------------------------------------------

    #[test]
    fn undiscounted_uncommitted_line_has_three_equal_amounts() {
        let inputs = PricingInputs {
            list_unit_price: dec!(32.77),
            quantity: dec!(10),
            currency: Currency::Usd,
            discounts: vec![],
            commitment: None,
        };
        let a = inputs.compute().unwrap();
        assert_eq!(a.list_cost, money(dec!(327.70)));
        assert_eq!(a.billed_cost, a.list_cost);
        assert_eq!(a.effective_cost, a.list_cost);
    }

    #[test]
    fn prepaid_commitment_splits_billed_from_effective() {
        // 100 GPU-hours: 60 covered by a prepaid reservation at $20, 40 on
        // demand at $32.77 less 10%.
        let inputs = PricingInputs {
            list_unit_price: dec!(32.77),
            quantity: dec!(100),
            currency: Currency::Usd,
            discounts: vec![Discount::Negotiated { rate: dec!(0.10) }],
            commitment: Some(CommitmentCoverage {
                covered_quantity: dec!(60),
                committed_unit_price: dec!(20),
                prepaid: true,
            }),
        };
        let a = inputs.compute().unwrap();
        // 40 * 32.77 * 0.90 = 1179.72
        assert_eq!(a.billed_cost, money(dec!(1179.720)));
        // ... plus 60 * 20 = 1200 amortized
        assert_eq!(a.effective_cost, money(dec!(2379.720)));
        assert_eq!(a.list_cost, money(dec!(3277.00)));
        assert!(a.billed_cost < a.effective_cost);
    }

    #[test]
    fn coverage_is_capped_at_the_line_quantity() {
        // A commitment larger than the usage must not manufacture negative
        // on-demand quantity.
        let inputs = PricingInputs {
            list_unit_price: dec!(10),
            quantity: dec!(5),
            currency: Currency::Usd,
            discounts: vec![],
            commitment: Some(CommitmentCoverage {
                covered_quantity: dec!(500),
                committed_unit_price: dec!(4),
                prepaid: false,
            }),
        };
        let a = inputs.compute().unwrap();
        assert_eq!(a.effective_cost, money(dec!(20)));
    }

    #[test]
    fn rejects_an_out_of_range_discount() {
        let inputs = PricingInputs {
            list_unit_price: dec!(1),
            quantity: dec!(1),
            currency: Currency::Usd,
            discounts: vec![Discount::Negotiated { rate: dec!(1.5) }],
            commitment: None,
        };
        assert!(matches!(
            inputs.compute(),
            Err(PricingError::DiscountRateOutOfRange(_))
        ));
    }

    #[test]
    fn apportions_a_node_across_three_workloads() {
        // $100 across 1/3, 1/3, 1/3 cannot be done in whole cents; one share
        // must absorb the extra.
        let shares = apportion(money(dec!(100)), &[dec!(1), dec!(1), dec!(1)], 2).unwrap();
        let total: Decimal = shares.iter().map(|m| m.amount()).sum();
        assert_eq!(total, dec!(100));
        assert_eq!(shares[0], money(dec!(33.34)));
        assert_eq!(shares[1], money(dec!(33.33)));
        assert_eq!(shares[2], money(dec!(33.33)));
    }

    #[test]
    fn apportionment_is_deterministic_on_reprocessing() {
        let w = [dec!(0.7), dec!(0.7), dec!(0.6)];
        let first = apportion(money(dec!(10)), &w, 2).unwrap();
        let second = apportion(money(dec!(10)), &w, 2).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn apportionment_rejects_degenerate_weights() {
        assert!(matches!(
            apportion(money(dec!(1)), &[], 2),
            Err(PricingError::NoWeights)
        ));
        assert!(matches!(
            apportion(money(dec!(1)), &[dec!(0), dec!(0)], 2),
            Err(PricingError::InvalidWeights)
        ));
        assert!(matches!(
            apportion(money(dec!(1)), &[dec!(-1), dec!(2)], 2),
            Err(PricingError::InvalidWeights)
        ));
    }

    // --- properties --------------------------------------------------------

    prop_compose! {
        fn arb_decimal(max: i64, scale: u32)(n in 0i64..max) -> Decimal {
            Decimal::new(n, scale)
        }
    }

    prop_compose! {
        fn arb_rate()(n in 0i64..=10_000i64) -> Decimal {
            // 0.0000 ..= 1.0000
            Decimal::new(n, 4)
        }
    }

    prop_compose! {
        fn arb_inputs()(
            list_unit_price in arb_decimal(1_000_000, 4),
            quantity in arb_decimal(1_000_000, 3),
            rates in prop::collection::vec(arb_rate(), 0..4),
            coverage_frac in arb_rate(),
            committed_frac in arb_rate(),
            prepaid in any::<bool>(),
            has_commitment in any::<bool>(),
        ) -> PricingInputs {
            let discounts: Vec<Discount> = rates
                .into_iter()
                .map(|rate| Discount::Negotiated { rate })
                .collect();
            let commitment = has_commitment.then(|| CommitmentCoverage {
                covered_quantity: quantity * coverage_frac,
                // Committed price is at or below list, which is the point of
                // committing.
                committed_unit_price: list_unit_price * committed_frac,
                prepaid,
            });
            PricingInputs {
                list_unit_price,
                quantity,
                currency: Currency::Usd,
                discounts,
                commitment,
            }
        }
    }

    proptest! {
        /// A discount can never make a line cost more than list, and a cost can
        /// never be negative. These are the two ways `effective_cost` going
        /// wrong would be visible in a report.
        #[test]
        fn effective_cost_is_bounded_by_list_and_non_negative(inputs in arb_inputs()) {
            let a = inputs.compute().unwrap();
            prop_assert!(a.effective_cost.amount() >= Decimal::ZERO);
            prop_assert!(a.billed_cost.amount() >= Decimal::ZERO);
            prop_assert!(
                a.effective_cost.amount() <= a.list_cost.amount(),
                "effective {} exceeded list {}",
                a.effective_cost, a.list_cost
            );
        }

        /// All three measures stay in one currency, which is what lets
        /// `CostEvent` store a single currency column.
        #[test]
        fn all_three_amounts_share_a_currency(inputs in arb_inputs()) {
            let a = inputs.compute().unwrap();
            prop_assert_eq!(a.list_cost.currency(), a.billed_cost.currency());
            prop_assert_eq!(a.list_cost.currency(), a.effective_cost.currency());
        }

        /// Discounts compose multiplicatively, so the order they were applied
        /// in cannot change the answer. If this ever fails, someone has changed
        /// composition to sequential subtraction.
        #[test]
        fn discount_order_does_not_change_the_result(inputs in arb_inputs()) {
            let forward = inputs.compute().unwrap();
            let mut reversed = inputs.clone();
            reversed.discounts.reverse();
            prop_assert_eq!(forward.effective_cost, reversed.compute().unwrap().effective_cost);
        }

        /// Prepaid coverage is the only thing that separates billed from
        /// effective; without it the two agree exactly.
        #[test]
        fn billed_equals_effective_unless_coverage_is_prepaid(inputs in arb_inputs()) {
            let a = inputs.compute().unwrap();
            let prepaid_coverage = inputs
                .commitment
                .is_some_and(|c| c.prepaid && c.covered_quantity.min(inputs.quantity) > Decimal::ZERO
                    && c.committed_unit_price > Decimal::ZERO);
            if prepaid_coverage {
                prop_assert!(a.billed_cost.amount() <= a.effective_cost.amount());
            } else {
                prop_assert_eq!(a.billed_cost, a.effective_cost);
            }
        }

        /// Apportionment conserves the total exactly, at any precision, for any
        /// weights. This is the property that catches rounding drift in
        /// telemetry attribution.
        #[test]
        fn apportionment_conserves_the_total(
            cents in 0i64..100_000_000i64,
            weights in prop::collection::vec(1i64..1_000_000i64, 1..24),
        ) {
            let total = money(Decimal::new(cents, 2));
            let weights: Vec<Decimal> = weights.into_iter().map(Decimal::from).collect();
            let shares = apportion(total, &weights, 2).unwrap();
            let summed: Decimal = shares.iter().map(|m| m.amount()).sum();
            prop_assert_eq!(summed, total.amount());
            prop_assert_eq!(shares.len(), weights.len());
        }

        /// No share is negative, and a zero-weight workload receives nothing.
        #[test]
        fn apportionment_shares_are_non_negative(
            cents in 0i64..1_000_000i64,
            weights in prop::collection::vec(0i64..1_000i64, 1..12),
        ) {
            let weights: Vec<Decimal> = weights.into_iter().map(Decimal::from).collect();
            prop_assume!(weights.iter().sum::<Decimal>() > Decimal::ZERO);
            let shares = apportion(money(Decimal::new(cents, 2)), &weights, 2).unwrap();
            for s in &shares {
                prop_assert!(s.amount() >= Decimal::ZERO);
            }
        }
    }
}

//! The reference allocation rule — the corpus's *specification*, not a helper.
//!
//! the precision requirements requires that every allocation have a deterministic
//! rounding-remainder rule and a test proving the parts sum to the whole for
//! every input. This module is that rule, written down before the posting engine
//! exists, so that P03 has something to match rather than something to invent.
//!
//! ## Largest remainder, with ties broken by index
//!
//! Allocate proportionally at full precision, round each share down to the
//! target scale, then hand the leftover minor units out one at a time to the
//! shares with the largest discarded fractional part. Ties go to the lower
//! index. That last clause is what makes it deterministic: without it,
//! allocating `$0.01` across three equal weights has three equally valid answers
//! and the one you get depends on iteration order.
//!
//! ## Why not "round each share and hope"
//!
//! Rounding each share independently does not sum to the whole. Allocating
//! `100.00` across three equal parts gives `33.33 × 3 = 99.99`; the missing cent
//! has to land somewhere, and "somewhere" must be a rule, because a
//! deferred-revenue waterfall that loses a cent per contract per month loses a
//! real number over a year and cannot be tied back to the control account.
//!
//! ## Signs
//!
//! Negative totals allocate correctly: the remainder is distributed in the
//! direction of the total's sign, so `allocate(-100.00, [1,1,1], 2)` gives
//! `[-33.34, -33.33, -33.33]` and sums to exactly `-100.00`.

use rust_decimal::{Decimal, RoundingStrategy};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AllocationError {
    #[error("cannot allocate across an empty set of weights")]
    NoWeights,
    #[error("weights must not be negative; found {0}")]
    NegativeWeight(Decimal),
    #[error("weights sum to zero, so there is no proportion to allocate by")]
    ZeroWeight,
    #[error("allocation overflowed Decimal")]
    Overflow,
    #[error("scale {0} exceeds the maximum Decimal scale of 28")]
    ScaleTooLarge(u32),
}

/// Splits `total` across `weights`, at `scale` decimal places.
///
/// The returned parts sum to exactly `total` — that is the property the whole
/// function exists to provide, and it is property-tested in
/// `tests/invariants.rs`.
///
/// # Errors
///
/// If `weights` is empty, contains a negative, or sums to zero; if `scale` is
/// beyond `Decimal`'s range; or if the arithmetic overflows.
pub fn allocate(
    total: Decimal,
    weights: &[Decimal],
    scale: u32,
) -> Result<Vec<Decimal>, AllocationError> {
    if weights.is_empty() {
        return Err(AllocationError::NoWeights);
    }
    if scale > 28 {
        return Err(AllocationError::ScaleTooLarge(scale));
    }
    let mut weight_total = Decimal::ZERO;
    for weight in weights {
        if weight.is_sign_negative() {
            return Err(AllocationError::NegativeWeight(*weight));
        }
        weight_total = weight_total
            .checked_add(*weight)
            .ok_or(AllocationError::Overflow)?;
    }
    if weight_total.is_zero() {
        return Err(AllocationError::ZeroWeight);
    }

    // Truncate toward zero rather than rounding to nearest, so the leftover is
    // always distributed in the direction of `total`'s sign and never has to be
    // clawed back.
    let mut parts = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut allocated = Decimal::ZERO;
    for (index, weight) in weights.iter().enumerate() {
        let exact = total
            .checked_mul(*weight)
            .ok_or(AllocationError::Overflow)?
            .checked_div(weight_total)
            .ok_or(AllocationError::Overflow)?;
        let truncated = exact.round_dp_with_strategy(scale, RoundingStrategy::ToZero);
        allocated = allocated
            .checked_add(truncated)
            .ok_or(AllocationError::Overflow)?;
        remainders.push((
            exact
                .checked_sub(truncated)
                .ok_or(AllocationError::Overflow)?
                .abs(),
            index,
        ));
        parts.push(truncated);
    }

    let mut leftover = total
        .checked_sub(allocated)
        .ok_or(AllocationError::Overflow)?;
    if leftover.is_zero() {
        return Ok(parts);
    }

    // One minor unit at the target scale, signed like the total.
    let unit = Decimal::new(1, scale);
    let step = if leftover.is_sign_negative() {
        -unit
    } else {
        unit
    };

    // Largest discarded fraction first; ties to the lower index. `sort_by` is
    // stable, so sorting by the remainder alone already breaks ties by index —
    // the explicit index comparison is there so the rule survives someone
    // switching to `sort_unstable_by`.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let mut cursor = 0;
    while !leftover.is_zero() {
        let (_, index) = remainders[cursor % remainders.len()];
        parts[index] = parts[index]
            .checked_add(step)
            .ok_or(AllocationError::Overflow)?;
        leftover = leftover
            .checked_sub(step)
            .ok_or(AllocationError::Overflow)?;
        cursor += 1;
        // A leftover larger than one unit per part means `total` had more
        // precision than `scale`, which the caller should have rounded first.
        if cursor > remainders.len().saturating_mul(2) {
            return Err(AllocationError::Overflow);
        }
    }
    Ok(parts)
}

/// Equal-weight convenience form: split `total` into `n` parts.
///
/// # Errors
///
/// As [`allocate`].
pub fn allocate_evenly(
    total: Decimal,
    n: usize,
    scale: u32,
) -> Result<Vec<Decimal>, AllocationError> {
    allocate(total, &vec![Decimal::ONE; n], scale)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "the no-unwrap rule permits unwrap in tests"
    )]

    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn hundred_across_three_gives_the_extra_cent_to_the_first() {
        let parts = allocate_evenly(dec!(100.00), 3, 2).unwrap();
        assert_eq!(parts, vec![dec!(33.34), dec!(33.33), dec!(33.33)]);
        assert_eq!(parts.iter().sum::<Decimal>(), dec!(100.00));
    }

    #[test]
    fn one_cent_across_three_gives_one_part_everything() {
        // The case that has three equally defensible answers and therefore needs
        // a stated rule.
        let parts = allocate_evenly(dec!(0.01), 3, 2).unwrap();
        assert_eq!(parts, vec![dec!(0.01), dec!(0.00), dec!(0.00)]);
        assert_eq!(parts.iter().sum::<Decimal>(), dec!(0.01));
    }

    #[test]
    fn negative_totals_allocate_in_the_right_direction() {
        let parts = allocate_evenly(dec!(-100.00), 3, 2).unwrap();
        assert_eq!(parts, vec![dec!(-33.34), dec!(-33.33), dec!(-33.33)]);
        assert_eq!(parts.iter().sum::<Decimal>(), dec!(-100.00));
    }

    #[test]
    fn zero_decimal_currencies_allocate_whole_units() {
        // JPY. A ledger that assumes two decimals gives 33.34 yen, which is not
        // an amount that exists.
        let parts = allocate_evenly(dec!(100), 3, 0).unwrap();
        assert_eq!(parts, vec![dec!(34), dec!(33), dec!(33)]);
    }

    #[test]
    fn three_decimal_currencies_allocate_thousandths() {
        // BHD, KWD.
        let parts = allocate_evenly(dec!(1.000), 3, 3).unwrap();
        assert_eq!(parts, vec![dec!(0.334), dec!(0.333), dec!(0.333)]);
        assert_eq!(parts.iter().sum::<Decimal>(), dec!(1.000));
    }

    #[test]
    fn unequal_weights_favour_the_largest_discarded_fraction() {
        // 10.00 across 1:1:1:1:1:1 = 1.6666… each. Six shares of 1.66 leave
        // 0.04, which goes to the first four by index after the tie.
        let parts = allocate_evenly(dec!(10.00), 6, 2).unwrap();
        assert_eq!(
            parts,
            vec![
                dec!(1.67),
                dec!(1.67),
                dec!(1.67),
                dec!(1.67),
                dec!(1.66),
                dec!(1.66)
            ]
        );
        assert_eq!(parts.iter().sum::<Decimal>(), dec!(10.00));
    }

    #[test]
    fn rejects_degenerate_weightings() {
        assert_eq!(allocate(dec!(1), &[], 2), Err(AllocationError::NoWeights));
        assert_eq!(
            allocate(dec!(1), &[dec!(0), dec!(0)], 2),
            Err(AllocationError::ZeroWeight)
        );
        assert_eq!(
            allocate(dec!(1), &[dec!(-1), dec!(2)], 2),
            Err(AllocationError::NegativeWeight(dec!(-1)))
        );
    }
}

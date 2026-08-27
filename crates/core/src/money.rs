//! Exact monetary amounts.
//!
//! Money is never `f64`/`f32`, anywhere, ever. Binary floating
//! point cannot represent `0.10` exactly, and in a ledger that error compounds
//! silently across millions of postings until a number is wrong in a way
//! nobody can explain to an auditor. Every crate in this workspace denies
//! `clippy::float_arithmetic` to keep it that way.

use core::cmp::Ordering;

use rust_decimal::Decimal;
pub use rust_decimal::RoundingStrategy;

/// Attempted arithmetic between two different currencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("currency mismatch: {lhs} and {rhs}")]
pub struct CurrencyMismatch {
    pub lhs: Currency,
    pub rhs: Currency,
}

/// An ISO 4217 currency.
///
/// `#[non_exhaustive]` because adding a currency is expected and must not be a
/// breaking change for downstream matches.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[non_exhaustive]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Chf,
    Jpy,
    Cad,
    Aud,
    Sek,
    Nok,
    Dkk,
    Sgd,
    Inr,
}

impl Currency {
    /// The ISO 4217 alphabetic code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Gbp => "GBP",
            Self::Chf => "CHF",
            Self::Jpy => "JPY",
            Self::Cad => "CAD",
            Self::Aud => "AUD",
            Self::Sek => "SEK",
            Self::Nok => "NOK",
            Self::Dkk => "DKK",
            Self::Sgd => "SGD",
            Self::Inr => "INR",
        }
    }

    /// Parses an ISO 4217 alphabetic code.
    ///
    /// Returns `None` rather than defaulting to USD: a row whose currency we
    /// cannot identify must not be silently reinterpreted as dollars.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_uppercase().as_str() {
            "USD" => Some(Self::Usd),
            "EUR" => Some(Self::Eur),
            "GBP" => Some(Self::Gbp),
            "CHF" => Some(Self::Chf),
            "JPY" => Some(Self::Jpy),
            "CAD" => Some(Self::Cad),
            "AUD" => Some(Self::Aud),
            "SEK" => Some(Self::Sek),
            "NOK" => Some(Self::Nok),
            "DKK" => Some(Self::Dkk),
            "SGD" => Some(Self::Sgd),
            "INR" => Some(Self::Inr),
            _ => None,
        }
    }

    /// Number of digits after the decimal separator for presentation and
    /// settlement rounding. JPY has none.
    ///
    /// Note that this is *not* the precision at which amounts are stored:
    /// usage-based AI pricing routinely quotes sub-cent rates (fractions of a
    /// cent per thousand tokens), so [`Money`] keeps full `Decimal` precision
    /// internally and rounds only where a real-world payment is produced.
    #[must_use]
    pub const fn minor_units(self) -> u32 {
        match self {
            Self::Jpy => 0,
            _ => 2,
        }
    }
}

impl core::fmt::Display for Currency {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.code())
    }
}

/// An exact monetary amount in a specific currency.
///
/// There is deliberately no `Add`/`Sub`/`Sum` implementation: adding two
/// `Money` values of different currencies is a domain error, and making it a
/// `Result` at every call site is what forces an explicit conversion decision
/// instead of a silent one. Use [`Money::checked_add`] and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Money {
    amount: Decimal,
    currency: Currency,
}

impl Money {
    #[must_use]
    pub const fn new(amount: Decimal, currency: Currency) -> Self {
        Self { amount, currency }
    }

    /// A zero amount in `currency`. The additive identity for sums that have a
    /// known currency but no rows yet.
    #[must_use]
    pub const fn zero(currency: Currency) -> Self {
        Self {
            amount: Decimal::ZERO,
            currency,
        }
    }

    /// Takes `self` by value — `Money` is `Copy` — so that `Money::amount`
    /// composes directly as a function reference, e.g. `opt.map(Money::amount)`.
    #[must_use]
    pub const fn amount(self) -> Decimal {
        self.amount
    }

    #[must_use]
    pub const fn currency(self) -> Currency {
        self.currency
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
    }

    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.amount.is_sign_negative() && !self.amount.is_zero()
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, CurrencyMismatch> {
        self.same_currency(other)?;
        Ok(Self::new(self.amount + other.amount, self.currency))
    }

    pub fn checked_sub(&self, other: &Self) -> Result<Self, CurrencyMismatch> {
        self.same_currency(other)?;
        Ok(Self::new(self.amount - other.amount, self.currency))
    }

    /// Compares two amounts, refusing to order across currencies.
    ///
    /// [`PartialOrd`] is also implemented and returns `None` on a mismatch, but
    /// `None` is easy to discard accidentally with `<`; prefer this where the
    /// comparison drives a financial decision (is this commitment exhausted, is
    /// this entry over a threshold) and a mismatch should surface as an error.
    pub fn checked_cmp(&self, other: &Self) -> Result<Ordering, CurrencyMismatch> {
        self.same_currency(other)?;
        Ok(self.amount.cmp(&other.amount))
    }

    /// The smaller of two amounts in the same currency.
    pub fn checked_min(&self, other: &Self) -> Result<Self, CurrencyMismatch> {
        Ok(if self.checked_cmp(other)? == Ordering::Greater {
            *other
        } else {
            *self
        })
    }

    /// The larger of two amounts in the same currency.
    pub fn checked_max(&self, other: &Self) -> Result<Self, CurrencyMismatch> {
        Ok(if self.checked_cmp(other)? == Ordering::Less {
            *other
        } else {
            *self
        })
    }

    /// Multiplies by a dimensionless factor — a discount rate, a proration
    /// fraction, a usage quantity. The currency is unchanged, so unlike
    /// addition this cannot fail.
    #[must_use]
    pub fn scale(&self, factor: Decimal) -> Self {
        Self::new(self.amount * factor, self.currency)
    }

    #[must_use]
    pub fn negate(&self) -> Self {
        Self::new(-self.amount, self.currency)
    }

    /// Rounds to `dp` decimal places using banker's rounding (half-to-even).
    ///
    /// Half-to-even rather than half-up, deliberately: this system rounds
    /// millions of sub-cent usage charges, and half-up biases every one of
    /// those midpoints in the same direction, so the error accumulates into a
    /// real number instead of cancelling out. Note the consequence — `1.005`
    /// rounds to `1.00`, not `1.01`. Where a contract or jurisdiction mandates
    /// a different convention, state it explicitly with [`Money::round_dp_with`]
    /// rather than changing this default.
    ///
    /// Rounding is always an explicit act: apportionment and rating hold full
    /// precision and round once, at the boundary where a number becomes an
    /// invoice line.
    #[must_use]
    pub fn round_dp(&self, dp: u32) -> Self {
        Self::new(self.amount.round_dp(dp), self.currency)
    }

    /// Rounds to `dp` decimal places under an explicitly chosen strategy.
    #[must_use]
    pub fn round_dp_with(&self, dp: u32, strategy: RoundingStrategy) -> Self {
        Self::new(
            self.amount.round_dp_with_strategy(dp, strategy),
            self.currency,
        )
    }

    /// Rounds to the currency's settlement precision, per [`Money::round_dp`].
    #[must_use]
    pub fn round_to_minor_units(&self) -> Self {
        self.round_dp(self.currency.minor_units())
    }

    /// Sums an iterator of amounts that must all share `currency`.
    ///
    /// Takes the currency explicitly so that an empty iterator still yields a
    /// well-defined zero rather than `None`.
    pub fn try_sum<'a, I>(iter: I, currency: Currency) -> Result<Self, CurrencyMismatch>
    where
        I: IntoIterator<Item = &'a Self>,
    {
        iter.into_iter()
            .try_fold(Self::zero(currency), |acc, item| acc.checked_add(item))
    }

    fn same_currency(&self, other: &Self) -> Result<(), CurrencyMismatch> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(CurrencyMismatch {
                lhs: self.currency,
                rhs: other.currency,
            })
        }
    }
}

/// Ordering within a currency only.
///
/// Note the absence of a matching [`Ord`] impl. Deriving both is tempting, but a
/// derived `Ord` compares `amount` first and `currency` second, which makes
/// `USD 5 > EUR 3` quietly return `true` — a total order over values that have
/// no total order. Returning `None` across currencies is the honest answer, and
/// [`Money::checked_cmp`] is available where an error is wanted instead.
impl PartialOrd for Money {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.currency == other.currency {
            Some(self.amount.cmp(&other.amount))
        } else {
            None
        }
    }
}

impl core::fmt::Display for Money {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} {}", self.amount, self.currency.code())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unwrap is permitted in tests")]

    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn adds_within_a_currency() {
        let a = Money::new(dec!(10.25), Currency::Usd);
        let b = Money::new(dec!(0.75), Currency::Usd);
        assert_eq!(
            a.checked_add(&b).unwrap(),
            Money::new(dec!(11.00), Currency::Usd)
        );
    }

    #[test]
    fn refuses_to_add_across_currencies() {
        let usd = Money::new(dec!(10), Currency::Usd);
        let eur = Money::new(dec!(10), Currency::Eur);
        assert_eq!(
            usd.checked_add(&eur),
            Err(CurrencyMismatch {
                lhs: Currency::Usd,
                rhs: Currency::Eur
            })
        );
    }

    #[test]
    #[expect(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "showing that neither operator holds across currencies is the point of the test"
    )]
    fn refuses_to_order_across_currencies() {
        let usd = Money::new(dec!(5), Currency::Usd);
        let eur = Money::new(dec!(3), Currency::Eur);
        assert_eq!(usd.partial_cmp(&eur), None);
        assert!(usd.checked_cmp(&eur).is_err());
        // The bug this prevents: a derived Ord would report `true` here.
        assert!(!(usd > eur));
        assert!(!(usd < eur));
    }

    #[test]
    fn orders_within_a_currency() {
        let big = Money::new(dec!(5), Currency::Usd);
        let small = Money::new(dec!(3), Currency::Usd);
        assert!(big > small);
        assert_eq!(big.checked_cmp(&small).unwrap(), Ordering::Greater);
    }

    #[test]
    fn decimal_arithmetic_is_exact_where_float_would_not_be() {
        let sum = Money::new(dec!(0.1), Currency::Usd)
            .checked_add(&Money::new(dec!(0.2), Currency::Usd))
            .unwrap();
        assert_eq!(sum, Money::new(dec!(0.3), Currency::Usd));

        // For contrast: `0.1` is not representable in binary floating point at
        // all, before any arithmetic happens. `from_f64_retain` keeps the bits
        // the f64 actually holds rather than presenting a cleaned-up value.
        // This is the entire reason for the no-float-money rule.
        let as_f64_really_is = Decimal::from_f64_retain(0.1_f64).unwrap();
        assert_ne!(as_f64_really_is, dec!(0.1));
        assert!(
            as_f64_really_is
                .to_string()
                .starts_with("0.1000000000000000055")
        );
    }

    #[test]
    fn accumulates_a_third_of_a_cent_without_drift() {
        // Sub-cent token pricing: a third of a cent, three hundred times, is
        // exactly one dollar. Full precision is held until an explicit round.
        let rate = Money::new(dec!(0.001), Currency::Usd);
        let total = (0..1000).try_fold(Money::zero(Currency::Usd), |acc, _| acc.checked_add(&rate));
        assert_eq!(total.unwrap(), Money::new(dec!(1.000), Currency::Usd));
    }

    #[test]
    fn sums_an_empty_iterator_to_zero() {
        let none: Vec<Money> = vec![];
        assert_eq!(
            Money::try_sum(none.iter(), Currency::Usd).unwrap(),
            Money::zero(Currency::Usd)
        );
    }

    #[test]
    fn sum_rejects_a_mixed_currency_batch() {
        let batch = [
            Money::new(dec!(1), Currency::Usd),
            Money::new(dec!(1), Currency::Gbp),
        ];
        assert!(Money::try_sum(batch.iter(), Currency::Usd).is_err());
    }

    #[test]
    fn rounds_to_settlement_precision_per_currency() {
        // JPY has no minor unit.
        assert_eq!(
            Money::new(dec!(1234.56), Currency::Jpy).round_to_minor_units(),
            Money::new(dec!(1235), Currency::Jpy)
        );
        assert_eq!(
            Money::new(dec!(10.994), Currency::Usd).round_to_minor_units(),
            Money::new(dec!(10.99), Currency::Usd)
        );
    }

    #[test]
    fn default_rounding_is_bankers_not_half_up() {
        // The behaviour most likely to surprise someone reading an invoice:
        // exact midpoints go to the even digit, so 1.005 -> 1.00 and
        // 1.015 -> 1.02. Pinned here so a future change to the default is a
        // deliberate, visible one.
        let midpoint_down = Money::new(dec!(1.005), Currency::Usd);
        let midpoint_up = Money::new(dec!(1.015), Currency::Usd);
        assert_eq!(
            midpoint_down.round_dp(2),
            Money::new(dec!(1.00), Currency::Usd)
        );
        assert_eq!(
            midpoint_up.round_dp(2),
            Money::new(dec!(1.02), Currency::Usd)
        );

        // And the explicit half-up alternative, for contracts that mandate it.
        assert_eq!(
            midpoint_down.round_dp_with(2, RoundingStrategy::MidpointAwayFromZero),
            Money::new(dec!(1.01), Currency::Usd)
        );
    }

    #[test]
    fn bankers_rounding_does_not_bias_a_batch_of_midpoints() {
        // Ten midpoint values that half-up would each push upward by 0.005,
        // inflating the batch by 0.05. Half-to-even splits them evenly.
        let midpoints: Vec<Money> = (0..10)
            .map(|i| Money::new(Decimal::new(1005 + i * 10, 3), Currency::Usd))
            .collect();

        let exact = Money::try_sum(midpoints.iter(), Currency::Usd).unwrap();
        let bankers = midpoints
            .iter()
            .map(|m| m.round_dp(2))
            .try_fold(Money::zero(Currency::Usd), |acc, m| acc.checked_add(&m))
            .unwrap();
        let half_up = midpoints
            .iter()
            .map(|m| m.round_dp_with(2, RoundingStrategy::MidpointAwayFromZero))
            .try_fold(Money::zero(Currency::Usd), |acc, m| acc.checked_add(&m))
            .unwrap();

        assert_eq!(bankers.amount(), exact.amount());
        assert_eq!(
            half_up.checked_sub(&exact).unwrap().amount(),
            dec!(0.05),
            "half-up drifts upward across a batch of midpoints"
        );
    }

    #[test]
    fn picks_min_and_max_within_a_currency_and_refuses_across() {
        let small = Money::new(dec!(3), Currency::Usd);
        let big = Money::new(dec!(5), Currency::Usd);
        assert_eq!(small.checked_min(&big).unwrap(), small);
        assert_eq!(small.checked_max(&big).unwrap(), big);
        assert!(
            small
                .checked_min(&Money::new(dec!(1), Currency::Eur))
                .is_err()
        );
    }

    #[test]
    fn scales_by_a_discount_rate() {
        let list = Money::new(dec!(1000), Currency::Usd);
        assert_eq!(
            list.scale(dec!(0.85)),
            Money::new(dec!(850.00), Currency::Usd)
        );
    }
}

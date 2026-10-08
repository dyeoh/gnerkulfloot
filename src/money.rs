//! Currency-agnostic money. Amounts are integers in the currency's minor units
//! (cents, sen, or whole yen), so no floating point ever touches a price. Every
//! operation checks that both sides share a currency and that nothing overflows.

use std::fmt;

use iso_currency::Currency;
use serde::{Deserialize, Serialize};

/// An amount of money in one currency.
///
/// # Examples
/// ```
/// use gnerkulfloot::money::{Money, Rounding};
/// use iso_currency::Currency;
///
/// let price = Money::new(1990, Currency::MYR); // RM19.90
/// let total = price.times(3).unwrap();
/// assert_eq!(total.amount, 5970);
/// // 6% tax, rounded half-up to the nearest sen.
/// assert_eq!(total.apply_rate(6, 100, Rounding::HalfUp).unwrap().amount, 358);
/// assert_eq!(total.to_string(), "59.70 MYR");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    /// Minor units of `currency`.
    pub amount: i64,
    pub currency: Currency,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MoneyError {
    #[error("cannot combine {0} with {1}")]
    CurrencyMismatch(Currency, Currency),
    #[error("money arithmetic overflowed")]
    Overflow,
    #[error("rate denominator must be positive")]
    InvalidRate,
    #[error("{0:?} isn't a valid amount for this currency")]
    InvalidDecimal(String),
}

/// How to round when a calculation lands between two minor units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    /// 0.5 rounds away from zero. What most people expect on a receipt.
    #[default]
    HalfUp,
    /// 0.5 rounds to the nearest even unit ("banker's rounding"). Some tax rules require it.
    HalfEven,
}

impl Money {
    pub const fn new(amount: i64, currency: Currency) -> Self {
        Self { amount, currency }
    }

    pub const fn zero(currency: Currency) -> Self {
        Self::new(0, currency)
    }

    /// Number of decimal places this currency uses (2 for MYR, 0 for JPY, 3 for BHD).
    /// Currencies without one, such as gold (XAU), are treated as having none.
    pub fn exponent(&self) -> u32 {
        self.currency.exponent().map_or(0, u32::from)
    }

    /// # Errors
    /// `CurrencyMismatch` if the currencies differ, `Overflow` if the sum doesn't fit.
    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(other)?;
        let amount = self.amount.checked_add(other.amount).ok_or(MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }

    /// # Errors
    /// `CurrencyMismatch` if the currencies differ, `Overflow` if the result doesn't fit.
    pub fn checked_sub(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(other)?;
        let amount = self.amount.checked_sub(other.amount).ok_or(MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }

    /// Multiplies by a whole quantity, e.g. unit price × items in an order line.
    ///
    /// # Errors
    /// `Overflow` if the result doesn't fit.
    pub fn times(self, qty: i64) -> Result<Money, MoneyError> {
        let amount = self.amount.checked_mul(qty).ok_or(MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }

    /// Returns `self × numerator / denominator`, rounded to a whole minor unit.
    ///
    /// Rates are passed as a fraction so they stay exact: 6% is `(6, 100)` and
    /// 8.25% is `(825, 10_000)`. Use this for tax and percentage discounts.
    ///
    /// # Errors
    /// `InvalidRate` if `denominator <= 0`, `Overflow` if the result doesn't fit.
    pub fn apply_rate(self, numerator: i64, denominator: i64, rounding: Rounding) -> Result<Money, MoneyError> {
        if denominator <= 0 {
            return Err(MoneyError::InvalidRate);
        }
        // i128 keeps the intermediate product exact for any i64 inputs.
        let product = i128::from(self.amount) * i128::from(numerator);
        let rounded = div_round(product, i128::from(denominator), rounding);
        let amount = i64::try_from(rounded).map_err(|_| MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }

    /// Adds up amounts that must all be in `currency`. An empty list sums to zero.
    ///
    /// # Errors
    /// `CurrencyMismatch` on the first amount in another currency, `Overflow` if the total doesn't fit.
    pub fn sum<I: IntoIterator<Item = Money>>(currency: Currency, items: I) -> Result<Money, MoneyError> {
        items.into_iter().try_fold(Money::zero(currency), Money::checked_add)
    }

    /// The amount as a plain decimal string in major units, e.g. `"19.90"` for
    /// MYR or `"500"` for JPY. This is the format payment providers usually want.
    pub fn to_decimal_string(&self) -> String {
        let shown = self.to_string();
        shown[..shown.len() - self.currency.code().len() - 1].to_owned()
    }

    /// Parses a decimal amount in major units (`"19.9"`, `"19.90"`, `"500"`)
    /// exactly, without going through floating point.
    ///
    /// # Errors
    /// `InvalidDecimal` for anything that isn't a plain decimal, or that has
    /// more decimal places than the currency allows (`"1.234"` in MYR);
    /// `Overflow` if it doesn't fit.
    pub fn parse_decimal(text: &str, currency: Currency) -> Result<Money, MoneyError> {
        let invalid = || MoneyError::InvalidDecimal(text.to_owned());
        let exp = Money::zero(currency).exponent() as usize;
        let (negative, digits) = match text.trim().strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.trim()),
        };
        let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
        let all_digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
        if whole.is_empty() || !all_digits(whole) || !all_digits(frac) || frac.len() > exp {
            return Err(invalid());
        }
        let padded = format!("{whole}{frac:0<exp$}");
        let minor: i64 = padded.parse().map_err(|_| MoneyError::Overflow)?;
        Ok(Money::new(if negative { -minor } else { minor }, currency))
    }

    fn same_currency(self, other: Money) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch(self.currency, other.currency))
        }
    }
}

/// Divides with the requested rounding. `d` must be positive.
fn div_round(n: i128, d: i128, rounding: Rounding) -> i128 {
    let q = n.div_euclid(d); // floor, because d > 0
    let r = n.rem_euclid(d); // 0 <= r < d
    let twice = r * 2;
    let round_up = match twice.cmp(&d) {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => match rounding {
            // Exactly halfway: away from zero means up for positives, toward the floor for negatives.
            Rounding::HalfUp => n >= 0,
            Rounding::HalfEven => q % 2 != 0,
        },
    };
    if round_up { q + 1 } else { q }
}

impl fmt::Display for Money {
    /// Formats as `<major>.<minor> <CODE>`, e.g. `19.90 MYR`, `500 JPY`, `-1.250 BHD`.
    /// Presentation (symbols, locales) is the storefront's job.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exp = self.exponent();
        let code = self.currency.code();
        if exp == 0 {
            return write!(f, "{} {code}", self.amount);
        }
        let scale = 10u64.pow(exp);
        let abs = self.amount.unsigned_abs();
        let sign = if self.amount < 0 { "-" } else { "" };
        write!(
            f,
            "{sign}{}.{:0width$} {code}",
            abs / scale,
            abs % scale,
            width = exp as usize
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Currency::{BHD, JPY, MYR, USD};

    #[test]
    fn refuses_to_mix_currencies() {
        let err = Money::new(100, MYR).checked_add(Money::new(100, USD)).unwrap_err();
        assert_eq!(err, MoneyError::CurrencyMismatch(MYR, USD));
        assert!(Money::sum(MYR, [Money::new(1, MYR), Money::new(1, JPY)]).is_err());
    }

    #[test]
    fn detects_overflow() {
        assert_eq!(Money::new(i64::MAX, MYR).times(2), Err(MoneyError::Overflow));
        assert_eq!(
            Money::new(i64::MAX, MYR).checked_add(Money::new(1, MYR)),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn rounding_half_up_and_half_even() {
        // 250 × 1/100 = 2.5 minor units: the tie case.
        let m = Money::new(250, MYR);
        assert_eq!(m.apply_rate(1, 100, Rounding::HalfUp).unwrap().amount, 3);
        assert_eq!(m.apply_rate(1, 100, Rounding::HalfEven).unwrap().amount, 2);
        assert_eq!(
            Money::new(350, MYR)
                .apply_rate(1, 100, Rounding::HalfEven)
                .unwrap()
                .amount,
            4
        );
        // Negative ties (refunds) round away from zero under HalfUp.
        assert_eq!(
            Money::new(-250, MYR)
                .apply_rate(1, 100, Rounding::HalfUp)
                .unwrap()
                .amount,
            -3
        );
        // Non-ties round to nearest regardless of mode.
        assert_eq!(
            Money::new(1999, MYR)
                .apply_rate(6, 100, Rounding::HalfEven)
                .unwrap()
                .amount,
            120
        );
    }

    #[test]
    fn rates_are_exact_for_fractional_percentages() {
        // 8.25% of 10.00 USD = 0.825 → 0.83 half-up.
        assert_eq!(
            Money::new(1000, USD)
                .apply_rate(825, 10_000, Rounding::HalfUp)
                .unwrap()
                .amount,
            83
        );
        assert_eq!(
            Money::new(1000, USD).apply_rate(1, 0, Rounding::HalfUp),
            Err(MoneyError::InvalidRate)
        );
    }

    #[test]
    fn rounds_to_each_currencys_minor_unit() {
        // The same 10% on "1000 minor units" means different things per currency;
        // rounding happens in minor units, so it's correct for every exponent.
        assert_eq!(
            Money::new(1005, JPY)
                .apply_rate(1, 10, Rounding::HalfUp)
                .unwrap()
                .amount,
            101
        );
        assert_eq!(
            Money::new(1005, BHD)
                .apply_rate(1, 10, Rounding::HalfUp)
                .unwrap()
                .amount,
            101
        );
    }

    #[test]
    fn displays_with_currency_exponent() {
        assert_eq!(Money::new(1990, MYR).to_string(), "19.90 MYR");
        assert_eq!(Money::new(5, MYR).to_string(), "0.05 MYR");
        assert_eq!(Money::new(500, JPY).to_string(), "500 JPY");
        assert_eq!(Money::new(-1250, BHD).to_string(), "-1.250 BHD");
    }

    #[test]
    fn decimal_strings_round_trip_exactly() {
        assert_eq!(Money::new(2920, MYR).to_decimal_string(), "29.20");
        assert_eq!(Money::new(5, MYR).to_decimal_string(), "0.05");
        assert_eq!(Money::new(500, JPY).to_decimal_string(), "500");
        assert_eq!(Money::new(-1250, BHD).to_decimal_string(), "-1.250");

        assert_eq!(Money::parse_decimal("29.20", MYR).unwrap().amount, 2920);
        assert_eq!(Money::parse_decimal("29.2", MYR).unwrap().amount, 2920);
        assert_eq!(Money::parse_decimal("29", MYR).unwrap().amount, 2900);
        assert_eq!(Money::parse_decimal("0.1", MYR).unwrap().amount, 10); // no 0.1 float error
        assert_eq!(Money::parse_decimal("500", JPY).unwrap().amount, 500);
        assert_eq!(Money::parse_decimal("-1.25", BHD).unwrap().amount, -1250);
        assert_eq!(Money::parse_decimal("5.", MYR).unwrap().amount, 500); // trailing dot is harmless
        for bad in ["", "1.234", "1,00", "abc", ".5", "1e3", "-"] {
            assert!(Money::parse_decimal(bad, MYR).is_err(), "{bad:?} should fail");
        }
        assert!(Money::parse_decimal("500.5", JPY).is_err(), "JPY has no decimals");
    }

    #[test]
    fn serializes_currency_as_code() {
        let json = serde_json::to_string(&Money::new(1990, MYR)).unwrap();
        assert_eq!(json, r#"{"amount":1990,"currency":"MYR"}"#);
    }
}

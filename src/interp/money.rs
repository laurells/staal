// Exact money. Amounts are integers in minor units (e.g. cents); there is no
// floating point. All arithmetic is checked: an overflow is an error, never a wrap.

use std::fmt;

/// Supported currencies and their minor-unit exponents (RUNTIME.md section 2).
const CURRENCIES: &[(&str, u32)] = &[
    ("USD", 2),
    ("EUR", 2),
    ("GBP", 2),
    ("NGN", 2),
    ("GHS", 2),
    ("KES", 2),
    ("ZAR", 2),
    ("CAD", 2),
    ("AUD", 2),
    ("JPY", 0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Currency {
    pub code: &'static str,
    pub exponent: u32,
}

impl Currency {
    /// Every supported currency: its code and how many decimal places it has.
    pub fn all() -> &'static [(&'static str, u32)] {
        CURRENCIES
    }

    pub fn parse(code: &str) -> Result<Currency, String> {
        CURRENCIES
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(c, e)| Currency { code: c, exponent: *e })
            .ok_or_else(|| {
                let known: Vec<&str> = CURRENCIES.iter().map(|(c, _)| *c).collect();
                format!("unsupported currency '{code}'; supported: {}", known.join(", "))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Money {
    /// Amount in minor units. Never negative.
    pub minor: i64,
    pub currency: Currency,
}

impl Money {
    /// Parses a decimal string such as "12.50" in the given currency. Rejects signs,
    /// exponents, spaces, empty parts and more decimal places than the currency allows.
    pub fn parse(amount: &str, currency: Currency) -> Result<Money, String> {
        let bad = || format!("invalid {} amount '{amount}': expected digits like \"12.50\"", currency.code);
        let (whole, frac) = match amount.split_once('.') {
            Some((w, f)) => (w, Some(f)),
            None => (amount, None),
        };
        if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let frac = frac.unwrap_or("");
        if amount.contains('.') && frac.is_empty() {
            return Err(bad());
        }
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        if frac.len() as u32 > currency.exponent {
            return Err(format!(
                "{} allows at most {} decimal place(s), got '{amount}'",
                currency.code, currency.exponent
            ));
        }
        let scale = 10i64.checked_pow(currency.exponent).ok_or_else(bad)?;
        let whole: i64 = whole.parse().map_err(|_| format!("amount '{amount}' is too large"))?;
        let mut frac_minor: i64 = if frac.is_empty() { 0 } else { frac.parse().map_err(|_| bad())? };
        // "12.5" in a 2-decimal currency means 50 minor units, not 5.
        for _ in frac.len() as u32..currency.exponent {
            frac_minor = frac_minor.checked_mul(10).ok_or_else(bad)?;
        }
        let minor = whole
            .checked_mul(scale)
            .and_then(|m| m.checked_add(frac_minor))
            .ok_or_else(|| format!("amount '{amount}' is too large"))?;
        Ok(Money { minor, currency })
    }

    /// A whole number of major units, as written in a capability limit (`max: 500 USD`).
    pub fn from_major(major: u64, currency: Currency) -> Result<Money, String> {
        let scale = 10i64.checked_pow(currency.exponent).ok_or("currency exponent too large")?;
        let major = i64::try_from(major).map_err(|_| format!("limit {major} is too large"))?;
        let minor = major.checked_mul(scale).ok_or_else(|| format!("limit {major} {} is too large", currency.code))?;
        Ok(Money { minor, currency })
    }

    /// The canonical decimal string, e.g. "12.50" or "1000" (JPY).
    pub fn amount_string(&self) -> String {
        if self.currency.exponent == 0 {
            return self.minor.to_string();
        }
        let scale = 10i64.pow(self.currency.exponent); // exponent <= 2, cannot overflow
        format!("{}.{:0width$}", self.minor / scale, self.minor % scale, width = self.currency.exponent as usize)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount_string(), self.currency.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd() -> Currency {
        Currency::parse("USD").unwrap()
    }

    #[test]
    fn parses_exact_amounts() {
        assert_eq!(Money::parse("12.50", usd()).unwrap().minor, 1250);
        assert_eq!(Money::parse("12.5", usd()).unwrap().minor, 1250);
        assert_eq!(Money::parse("12", usd()).unwrap().minor, 1200);
        assert_eq!(Money::parse("0.01", usd()).unwrap().minor, 1);
        assert_eq!(Money::parse("0", usd()).unwrap().minor, 0);
        let jpy = Currency::parse("JPY").unwrap();
        assert_eq!(Money::parse("1500", jpy).unwrap().minor, 1500);
    }

    #[test]
    fn rejects_malformed_amounts() {
        for bad in ["", ".", "1.", ".5", "-1", "+1", "1.234", "1e3", " 1", "1 ", "1,000", "12.5.0", "0x10", "١٢"] {
            assert!(Money::parse(bad, usd()).is_err(), "{bad:?} should be rejected");
        }
        let jpy = Currency::parse("JPY").unwrap();
        assert!(Money::parse("1.5", jpy).is_err());
    }

    #[test]
    fn rejects_overflow() {
        assert!(Money::parse("92233720368547758.08", usd()).is_err());
        assert!(Money::parse("99999999999999999999", usd()).is_err());
        assert!(Money::from_major(u64::MAX, usd()).is_err());
        assert!(Money::from_major(92233720368547759, usd()).is_err());
    }

    #[test]
    fn rejects_unknown_currency() {
        assert!(Currency::parse("XYZ").is_err());
        assert!(Currency::parse("usd").is_err());
    }

    #[test]
    fn formats_canonically() {
        assert_eq!(Money::parse("12.5", usd()).unwrap().amount_string(), "12.50");
        assert_eq!(Money::parse("0.07", usd()).unwrap().amount_string(), "0.07");
        assert_eq!(Money::from_major(500, usd()).unwrap().to_string(), "500.00 USD");
    }
}

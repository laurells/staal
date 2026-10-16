// The capability broker (RUNTIME.md section 6). It holds every capability; a
// program only ever holds a handle. Limits are cumulative and are checked
// against every ancestor, so narrowing can never create new authority.

use super::money::Money;

#[derive(Debug)]
struct CapState {
    path: String,
    limit: Option<Money>,
    spent: i64,
    parent: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Broker {
    caps: Vec<CapState>,
}

impl Broker {
    /// A root capability, created only from a grant.
    pub fn grant(&mut self, path: &str, limit: Option<Money>) -> usize {
        self.caps.push(CapState { path: path.to_string(), limit, spent: 0, parent: None });
        self.caps.len() - 1
    }

    /// A child with a (possibly) smaller limit. Its spending also counts against
    /// every ancestor, so the child can never spend more than they allow.
    pub fn narrow(&mut self, parent: usize, limit: Money) -> Result<usize, String> {
        let p = self.caps.get(parent).ok_or("unknown capability handle")?;
        let path = p.path.clone();
        self.caps.push(CapState { path, limit: Some(limit), spent: 0, parent: Some(parent) });
        Ok(self.caps.len() - 1)
    }

    pub fn path(&self, h: usize) -> Result<&str, String> {
        self.caps.get(h).map(|c| c.path.as_str()).ok_or_else(|| "unknown capability handle".into())
    }

    fn chain(&self, h: usize) -> Result<Vec<usize>, String> {
        let mut out = Vec::new();
        let mut cur = Some(h);
        while let Some(i) = cur {
            let c = self.caps.get(i).ok_or("unknown capability handle")?;
            out.push(i);
            cur = c.parent;
        }
        Ok(out)
    }

    /// Checks that an action spending `amount` is within every limit in the chain.
    /// Does not spend; call `charge` after the action succeeds.
    pub fn authorize(&self, h: usize, amount: Option<Money>) -> Result<(), String> {
        for i in self.chain(h)? {
            let c = &self.caps[i];
            let Some(limit) = c.limit else { continue };
            let Some(amount) = amount else {
                return Err(format!(
                    "'{}' is limited to {limit}, and the amount of this action could not be determined",
                    c.path
                ));
            };
            if amount.currency != limit.currency {
                return Err(format!(
                    "'{}' is limited in {}, but this action spends {}",
                    c.path, limit.currency.code, amount.currency.code
                ));
            }
            let after = c.spent.checked_add(amount.minor).ok_or("amount overflow")?;
            if after > limit.minor {
                let left = Money { minor: limit.minor - c.spent, currency: limit.currency };
                return Err(format!("'{}' has {left} left of its {limit} limit; this action needs {amount}", c.path));
            }
        }
        Ok(())
    }

    /// Records spending against the whole chain. Must follow a successful `authorize`.
    pub fn charge(&mut self, h: usize, amount: Option<Money>) -> Result<(), String> {
        let Some(amount) = amount else { return Ok(()) };
        for i in self.chain(h)? {
            let c = &mut self.caps[i];
            if c.limit.is_some() {
                c.spent = c.spent.checked_add(amount.minor).ok_or("amount overflow")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::money::Currency;

    fn usd(minor: i64) -> Money {
        Money { minor, currency: Currency::parse("USD").unwrap() }
    }

    #[test]
    fn limits_are_cumulative() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", Some(usd(500)));
        b.authorize(root, Some(usd(300))).unwrap();
        b.charge(root, Some(usd(300))).unwrap();
        assert!(b.authorize(root, Some(usd(300))).is_err());
        b.authorize(root, Some(usd(200))).unwrap();
    }

    #[test]
    fn children_draw_from_ancestors() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", Some(usd(500)));
        let a = b.narrow(root, usd(400)).unwrap();
        let c = b.narrow(root, usd(400)).unwrap();
        b.authorize(a, Some(usd(400))).unwrap();
        b.charge(a, Some(usd(400))).unwrap();
        // Sibling c has 400 of its own, but the root has only 100 left.
        assert!(b.authorize(c, Some(usd(200))).is_err());
        b.authorize(c, Some(usd(100))).unwrap();
    }

    #[test]
    fn unknown_amount_or_other_currency_is_refused() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", Some(usd(500)));
        assert!(b.authorize(root, None).is_err());
        let ngn = Money { minor: 1, currency: Currency::parse("NGN").unwrap() };
        assert!(b.authorize(root, Some(ngn)).is_err());
    }

    #[test]
    fn unlimited_capabilities_need_no_amount() {
        let mut b = Broker::default();
        let root = b.grant("email.send", None);
        b.authorize(root, None).unwrap();
        b.charge(root, None).unwrap();
    }

    #[test]
    fn bad_handles_are_errors_not_panics() {
        let mut b = Broker::default();
        assert!(b.authorize(7, None).is_err());
        assert!(b.narrow(7, usd(1)).is_err());
        assert!(b.path(7).is_err());
    }
}

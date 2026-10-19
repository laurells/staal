// The capability broker (RUNTIME.md section 6). It holds every capability; a
// program only ever holds a handle. Every limit is checked against every ancestor,
// so narrowing can never create new authority.
//
// Limits (all optional, all enforced together):
//   max   the most money spent through the capability in total (charged on success)
//   each  the most money one action may spend
//   uses  how many actions may be attempted in total (counted when approved, success
//         or not: a failed attempt can still cost money or reach someone)
//   only  the targets it may act on (patterns, see crate::pattern)
//
// A limit that needs to know something about an action (its amount, its target)
// refuses the action when the adapter cannot say: limits fail closed.

use super::money::Money;
use crate::pattern;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub max: Option<Money>,
    pub each: Option<Money>,
    pub uses: Option<u64>,
    pub only: Option<Vec<String>>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        *self == Limits::default()
    }
}

#[derive(Debug)]
struct CapState {
    path: String,
    limits: Limits,
    /// Money spent through this capability and its children (minor units).
    spent: i64,
    /// Actions attempted through this capability and its children.
    used: u64,
    parent: Option<usize>,
    /// For `net.http`: the operator has stated that the service deduplicates requests
    /// by an `Idempotency-Key` header, so one left in doubt may be retried. Set only by
    /// a grant, and inherited by every capability narrowed from it.
    idempotency_key: bool,
}

#[derive(Debug, Default)]
pub struct Broker {
    caps: Vec<CapState>,
}

impl Broker {
    /// A root capability, created only from a grant.
    pub fn grant(&mut self, path: &str, limits: Limits) -> usize {
        self.caps.push(CapState { path: path.to_string(), limits, spent: 0, used: 0, parent: None, idempotency_key: false });
        self.caps.len() - 1
    }

    /// Marks a granted capability as deduplicated by `Idempotency-Key` (see CapState).
    pub fn set_idempotency_key(&mut self, h: usize) -> Result<(), String> {
        let c = self.caps.get_mut(h).ok_or("unknown capability handle")?;
        c.idempotency_key = true;
        Ok(())
    }

    pub fn idempotency_key(&self, h: usize) -> Result<bool, String> {
        self.caps.get(h).map(|c| c.idempotency_key).ok_or_else(|| "unknown capability handle".into())
    }

    /// A child with (possibly) tighter limits. Everything it does also counts against
    /// every ancestor, so the child can never do more than they allow.
    pub fn narrow(&mut self, parent: usize, limits: Limits) -> Result<usize, String> {
        let p = self.caps.get(parent).ok_or("unknown capability handle")?;
        let (path, idempotency_key) = (p.path.clone(), p.idempotency_key);
        self.caps.push(CapState { path, limits, spent: 0, used: 0, parent: Some(parent), idempotency_key });
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

    /// Checks that an action spending `amount` on `target` is within every limit in
    /// the chain. Changes nothing: call `count_use` before performing it, and `charge`
    /// after it succeeds.
    pub fn authorize(&self, h: usize, amount: Option<Money>, target: Option<&str>) -> Result<(), String> {
        for i in self.chain(h)? {
            let c = &self.caps[i];
            let l = &c.limits;
            if let Some(limit) = l.max {
                let amount = known_amount(amount, &c.path, "max", limit)?;
                let after = c.spent.checked_add(amount.minor).ok_or("amount overflow")?;
                if after > limit.minor {
                    let left = Money { minor: limit.minor - c.spent, currency: limit.currency };
                    return Err(format!("'{}' has {left} left of its {limit} limit; this action needs {amount}", c.path));
                }
            }
            if let Some(limit) = l.each {
                let amount = known_amount(amount, &c.path, "each", limit)?;
                if amount.minor > limit.minor {
                    return Err(format!("'{}' allows at most {limit} per action; this action needs {amount}", c.path));
                }
            }
            if let Some(uses) = l.uses
                && c.used >= uses
            {
                return Err(format!("'{}' has used all {uses} of its allowed uses", c.path));
            }
            if let Some(only) = &l.only {
                let Some(t) = target else {
                    return Err(format!(
                        "'{}' may only act on {only:?}, and the target of this action could not be determined",
                        c.path
                    ));
                };
                if !only.iter().any(|p| pattern::matches(p, t)) {
                    return Err(format!("'{}' may only act on {only:?}; this action targets \"{t}\"", c.path));
                }
            }
        }
        Ok(())
    }

    /// Records one attempted action against the whole chain. Must follow a successful
    /// `authorize`, before the action is performed.
    pub fn count_use(&mut self, h: usize) -> Result<(), String> {
        for i in self.chain(h)? {
            let c = &mut self.caps[i];
            c.used = c.used.checked_add(1).ok_or("use count overflow")?;
        }
        Ok(())
    }

    /// Records money spent against the whole chain, after the action succeeded.
    pub fn charge(&mut self, h: usize, amount: Option<Money>) -> Result<(), String> {
        let Some(amount) = amount else { return Ok(()) };
        for i in self.chain(h)? {
            let c = &mut self.caps[i];
            if c.limits.max.is_some() {
                c.spent = c.spent.checked_add(amount.minor).ok_or("amount overflow")?;
            }
        }
        Ok(())
    }
}

/// The action's amount, which a money limit needs, in the limit's currency.
fn known_amount(amount: Option<Money>, path: &str, name: &str, limit: Money) -> Result<Money, String> {
    let Some(amount) = amount else {
        return Err(format!("'{path}' has `{name}: {limit}`, and the amount of this action could not be determined"));
    };
    if amount.currency != limit.currency {
        return Err(format!(
            "'{path}' is limited in {}, but this action spends {}",
            limit.currency.code, amount.currency.code
        ));
    }
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::money::Currency;

    fn usd(minor: i64) -> Money {
        Money { minor, currency: Currency::parse("USD").unwrap() }
    }

    fn max(minor: i64) -> Limits {
        Limits { max: Some(usd(minor)), ..Limits::default() }
    }

    #[test]
    fn limits_are_cumulative() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", max(500));
        b.authorize(root, Some(usd(300)), None).unwrap();
        b.charge(root, Some(usd(300))).unwrap();
        assert!(b.authorize(root, Some(usd(300)), None).is_err());
        b.authorize(root, Some(usd(200)), None).unwrap();
    }

    #[test]
    fn children_draw_from_ancestors() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", max(500));
        let a = b.narrow(root, max(400)).unwrap();
        let c = b.narrow(root, max(400)).unwrap();
        b.authorize(a, Some(usd(400)), None).unwrap();
        b.charge(a, Some(usd(400))).unwrap();
        // Sibling c has 400 of its own, but the root has only 100 left.
        assert!(b.authorize(c, Some(usd(200)), None).is_err());
        b.authorize(c, Some(usd(100)), None).unwrap();
    }

    #[test]
    fn unknown_amount_or_other_currency_is_refused() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", max(500));
        assert!(b.authorize(root, None, None).is_err());
        let ngn = Money { minor: 1, currency: Currency::parse("NGN").unwrap() };
        assert!(b.authorize(root, Some(ngn), None).is_err());
        let each = b.grant("payments.refund", Limits { each: Some(usd(50)), ..Limits::default() });
        assert!(b.authorize(each, None, None).unwrap_err().contains("could not be determined"));
    }

    #[test]
    fn unlimited_capabilities_need_no_amount_or_target() {
        let mut b = Broker::default();
        let root = b.grant("email.send", Limits::default());
        b.authorize(root, None, None).unwrap();
        b.count_use(root).unwrap();
        b.charge(root, None).unwrap();
    }

    #[test]
    fn each_limits_a_single_action() {
        let mut b = Broker::default();
        let root = b.grant("payments.refund", Limits { each: Some(usd(50)), ..Limits::default() });
        b.authorize(root, Some(usd(50)), None).unwrap();
        assert!(b.authorize(root, Some(usd(51)), None).unwrap_err().contains("per action"));
        // `each` does not accumulate.
        b.charge(root, Some(usd(50))).unwrap();
        b.authorize(root, Some(usd(50)), None).unwrap();
    }

    #[test]
    fn uses_count_attempts_through_the_chain() {
        let mut b = Broker::default();
        let root = b.grant("building.lock.open", Limits { uses: Some(3), ..Limits::default() });
        let child = b.narrow(root, Limits { uses: Some(2), ..Limits::default() }).unwrap();
        for _ in 0..2 {
            b.authorize(child, None, None).unwrap();
            b.count_use(child).unwrap();
        }
        assert!(b.authorize(child, None, None).unwrap_err().contains("all 2"));
        // The root has one use left; a fresh child cannot exceed it.
        let other = b.narrow(root, Limits { uses: Some(5), ..Limits::default() }).unwrap();
        b.authorize(other, None, None).unwrap();
        b.count_use(other).unwrap();
        assert!(b.authorize(other, None, None).unwrap_err().contains("all 3"));
    }

    #[test]
    fn only_restricts_targets_and_fails_closed() {
        let mut b = Broker::default();
        let only = |p: &[&str]| Limits { only: Some(p.iter().map(|s| s.to_string()).collect()), ..Limits::default() };
        let root = b.grant("email.send", only(&["*@teamozy.org"]));
        b.authorize(root, None, Some("ada@teamozy.org")).unwrap();
        assert!(b.authorize(root, None, Some("ada@evil.com")).unwrap_err().contains("targets \"ada@evil.com\""));
        assert!(b.authorize(root, None, None).unwrap_err().contains("could not be determined"));
        // A child cannot reach beyond its ancestor, even with a wider pattern of its own.
        let child = b.narrow(root, only(&["*"])).unwrap();
        assert!(b.authorize(child, None, Some("ada@evil.com")).is_err());
    }

    #[test]
    fn bad_handles_are_errors_not_panics() {
        let mut b = Broker::default();
        assert!(b.authorize(7, None, None).is_err());
        assert!(b.narrow(7, max(1)).is_err());
        assert!(b.path(7).is_err());
        assert!(b.count_use(7).is_err());
    }
}

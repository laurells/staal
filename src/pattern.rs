// Allow-list patterns for capability limits (`only: [...]`), shared by the compiler
// (which checks narrowing) and the runtime broker (which checks each action).
//
// A pattern is either exact text, or text with exactly one `*`, which matches any run
// of characters, including none: "*@teamozy.org", "/reports/*", "+234*", "*".

/// Checks that `p` is a valid pattern: not empty, and at most one `*`.
pub fn validate(p: &str) -> Result<(), String> {
    if p.is_empty() {
        return Err("an empty pattern matches nothing; remove it".into());
    }
    if p.matches('*').count() > 1 {
        return Err(format!("pattern \"{p}\" has more than one `*`; only one is allowed"));
    }
    Ok(())
}

/// Splits a pattern into the parts before and after its `*`, or None if it is exact.
fn split(p: &str) -> Option<(&str, &str)> {
    p.split_once('*')
}

/// Whether `s` matches pattern `p`.
pub fn matches(p: &str, s: &str) -> bool {
    match split(p) {
        None => p == s,
        // Both parts must fit without overlapping: "ab*ba" must not match "aba".
        Some((pre, suf)) => s.len() >= pre.len() + suf.len() && s.starts_with(pre) && s.ends_with(suf),
    }
}

/// Whether every string matching `narrow` also matches `wide`.
pub fn contained(narrow: &str, wide: &str) -> bool {
    match (split(narrow), split(wide)) {
        // An exact pattern is contained exactly when `wide` matches it.
        (None, _) => matches(wide, narrow),
        // A pattern with `*` matches infinitely many strings; an exact one matches one.
        (Some(_), None) => false,
        // a*b is inside c*d exactly when c is a prefix of a and d is a suffix of b:
        // then every a·x·b starts with c, ends with d, and is long enough; otherwise
        // some long a·x·b fails to start with c or to end with d.
        (Some((np, ns)), Some((wp, ws))) => np.starts_with(wp) && ns.ends_with(ws),
    }
}

/// Whether every pattern in `narrow` is contained in some pattern of `wide`. This is
/// conservative: a pattern covered only by several wide patterns together is refused.
pub fn all_contained(narrow: &[String], wide: &[String]) -> bool {
    narrow.iter().all(|n| wide.iter().any(|w| contained(n, w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_wildcard_matching() {
        assert!(matches("north-door", "north-door"));
        assert!(!matches("north-door", "north-door-2"));
        assert!(matches("*@teamozy.org", "ada@teamozy.org"));
        assert!(!matches("*@teamozy.org", "ada@teamozy.org.evil.com"));
        assert!(matches("/reports/*", "/reports/q3.pdf"));
        assert!(!matches("/reports/*", "/reports"));
        assert!(matches("+234*", "+2348012345678"));
        assert!(matches("*", ""));
        assert!(matches("ab*ba", "abba"));
        assert!(!matches("ab*ba", "aba"), "the two parts may not overlap");
        assert!(matches("é*ü", "éxü"));
    }

    #[test]
    fn invalid_patterns_are_refused() {
        assert!(validate("").is_err());
        assert!(validate("a*b*c").is_err());
        assert!(validate("a*b").is_ok());
        assert!(validate("exact").is_ok());
    }

    #[test]
    fn containment_examples() {
        assert!(contained("ada@teamozy.org", "*@teamozy.org"));
        assert!(contained("*@sales.teamozy.org", "*teamozy.org"));
        assert!(contained("/reports/2026/*", "/reports/*"));
        assert!(!contained("/reports/*", "/reports/2026/*"), "narrowing may not widen");
        assert!(!contained("*", "anything"), "a wildcard is never inside an exact pattern");
        assert!(contained("x", "x"));
        assert!(!contained("x", "y"));
        assert!(all_contained(&["a1".into(), "b*".into()], &["a*".into(), "b*".into()]));
        assert!(!all_contained(&["c".into()], &["a*".into(), "b*".into()]));
        assert!(all_contained(&[], &["a".into()]), "an empty list allows nothing, which is narrower");
    }

    /// Checks `contained` against its definition by brute force: for every pair of small
    /// patterns, `contained` must hold exactly when every string matching the narrow
    /// pattern (up to a length bound, which suffices for patterns this short) also
    /// matches the wide one.
    #[test]
    fn containment_agrees_with_brute_force() {
        let alphabet = ['a', 'b'];
        let mut strings = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..7 {
            let mut next = Vec::new();
            for s in &frontier {
                for c in alphabet {
                    next.push(format!("{s}{c}"));
                }
            }
            strings.extend(next.iter().cloned());
            frontier = next;
        }
        // Every pattern over {a, b} with at most one `*`, up to 3 characters plus the star.
        let mut patterns = Vec::new();
        for s in strings.iter().filter(|s| s.len() <= 3) {
            if !s.is_empty() {
                patterns.push(s.clone());
            }
            for i in 0..=s.len() {
                patterns.push(format!("{}*{}", &s[..i], &s[i..]));
            }
        }
        let mut checked = 0;
        for n in &patterns {
            for w in &patterns {
                let truth = strings.iter().all(|s| !matches(n, s) || matches(w, s));
                assert_eq!(contained(n, w), truth, "contained({n:?}, {w:?})");
                checked += 1;
            }
        }
        assert!(checked > 1000, "checked {checked} pairs");
    }
}

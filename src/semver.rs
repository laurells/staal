// Versions and version ranges for package dependencies (RUNTIME.md section 14).
//
// A version is Semantic Versioning 2.0.0 without build metadata: MAJOR.MINOR.PATCH,
// optionally `-` and dot-separated pre-release identifiers. Build metadata (`+...`) is
// refused: two versions that differ only in it would be different code under one
// precedence.
//
// A range is one or more comparators separated by commas, all of which must hold:
//
//   1.2.3  ^1.2.3   at least 1.2.3, below 2.0.0 (the default, as in Cargo)
//   ~1.2.3          at least 1.2.3, below 1.3.0
//   =1.2.3          exactly 1.2.3
//   >=1.2  >1.2  <2  <=1.4   comparisons; a missing part is a wildcard
//   1.*  1.2.*  *   wildcards
//
// For 0.x versions `^` is narrower (^0.2.3 is below 0.3.0; ^0.0.3 is exactly 0.0.3),
// since before 1.0 a minor version may break compatibility. A pre-release version
// (1.3.0-beta.1) matches only a range with a comparator that names a pre-release of
// the same MAJOR.MINOR.PATCH, so a range never moves a project onto a pre-release it
// did not ask for.

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pre {
    Num(u64),
    Alpha(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<Pre>,
}

/// One numeric part: digits, no leading zero, within u64.
fn number(s: &str, what: &str) -> Result<u64, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{what} '{s}' is not a number"));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(format!("{what} '{s}' has a leading zero"));
    }
    s.parse::<u64>().map_err(|_| format!("{what} '{s}' is too large"))
}

fn parse_pre(s: &str) -> Result<Vec<Pre>, String> {
    let mut out = Vec::new();
    for id in s.split('.') {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("pre-release identifier '{id}' must be letters, digits and -"));
        }
        if id.bytes().all(|b| b.is_ascii_digit()) {
            out.push(Pre::Num(number(id, "pre-release number")?));
        } else {
            out.push(Pre::Alpha(id.to_string()));
        }
    }
    Ok(out)
}

impl Version {
    pub fn parse(s: &str) -> Result<Version, String> {
        if s.contains('+') {
            return Err(format!("'{s}': build metadata (+...) is not allowed in a version"));
        }
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, parse_pre(p).map_err(|e| format!("'{s}': {e}"))?),
            None => (s, Vec::new()),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [ma, mi, pa] = parts[..] else {
            return Err(format!("'{s}' is not a version (MAJOR.MINOR.PATCH, such as 1.4.0)"));
        };
        let n = |p: &str| number(p, "version part").map_err(|e| format!("'{s}': {e}"));
        Ok(Version { major: n(ma)?, minor: n(mi)?, patch: n(pa)?, pre })
    }

    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }

    fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.triple().cmp(&o.triple()).then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // A pre-release comes before its release.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&o.pre) {
                    let c = match (a, b) {
                        (Pre::Num(x), Pre::Num(y)) => x.cmp(y),
                        (Pre::Num(_), Pre::Alpha(_)) => Ordering::Less,
                        (Pre::Alpha(_), Pre::Num(_)) => Ordering::Greater,
                        (Pre::Alpha(x), Pre::Alpha(y)) => x.as_bytes().cmp(y.as_bytes()),
                    };
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                self.pre.len().cmp(&o.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, p) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match p {
                Pre::Num(n) => write!(f, "{n}")?,
                Pre::Alpha(a) => f.write_str(a)?,
            }
        }
        Ok(())
    }
}

/// One comparator as bounds: at least `lower` (inclusive or not), below `upper` (or
/// at most, when inclusive). `pre` is the MAJOR.MINOR.PATCH of a pre-release it names.
#[derive(Debug, Clone, PartialEq)]
struct Bound {
    lower: Option<(Version, bool)>,
    upper: Option<(Version, bool)>,
    pre: Option<(u64, u64, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    text: String,
    bounds: Vec<Bound>,
}

fn release(major: u64, minor: u64, patch: u64) -> Version {
    Version { major, minor, patch, pre: Vec::new() }
}

/// The smallest version above every MAJOR.MINOR.x (or MAJOR.x.x): the start of the
/// next one, as a pre-release-free bound.
fn bump(major: u64, minor: Option<u64>) -> Result<Version, String> {
    let over = || "a version part is too large".to_string();
    Ok(match minor {
        Some(mi) => release(major, mi.checked_add(1).ok_or_else(over)?, 0),
        None => release(major.checked_add(1).ok_or_else(over)?, 0, 0),
    })
}

/// The smallest pre-release of MAJOR.MINOR.PATCH (`-0`): an exclusive upper bound below
/// it excludes the pre-releases of that release too.
fn floor(v: Version) -> Version {
    Version { pre: vec![Pre::Num(0)], ..v }
}

fn comparator(c: &str) -> Result<Bound, String> {
    let (op, rest) = ["^", "~", ">=", "<=", ">", "<", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim_start())))
        .unwrap_or(("", c));
    if rest.is_empty() {
        return Err(format!("'{c}' has no version"));
    }
    if rest.contains('+') {
        return Err(format!("'{c}': build metadata (+...) is not allowed"));
    }
    let (core, pre) = match rest.split_once('-') {
        Some((core, p)) => (core, parse_pre(p)?),
        None => (rest, Vec::new()),
    };
    // Parts: a number, or a wildcard (`*`, `x`, `X`), or missing; nothing after a wildcard.
    let mut parts: Vec<Option<u64>> = Vec::new();
    let mut wild = false;
    for p in core.split('.') {
        if ["*", "x", "X"].contains(&p) {
            wild = true;
            parts.push(None);
        } else if wild {
            return Err(format!("'{c}': a number cannot follow a wildcard"));
        } else {
            parts.push(Some(number(p, "version part")?));
        }
    }
    if parts.len() > 3 {
        return Err(format!("'{c}' has more than three parts"));
    }
    parts.resize(3, None);
    let [ma, mi, pa] = [parts[0], parts[1], parts[2]];
    if !pre.is_empty() && pa.is_none() {
        return Err(format!("'{c}': a pre-release needs all three parts"));
    }
    if wild && !op.is_empty() && op != "=" {
        return Err(format!("'{c}': use a wildcard alone (1.*), not with {op}"));
    }
    let named_pre = (!pre.is_empty()).then(|| (ma.unwrap_or(0), mi.unwrap_or(0), pa.unwrap_or(0)));
    let full = |ma, mi, pa| Version { major: ma, minor: mi, patch: pa, pre: pre.clone() };
    let lower_of = || release(ma.unwrap_or(0), mi.unwrap_or(0), pa.unwrap_or(0));
    let b = |lower: Option<(Version, bool)>, upper: Option<(Version, bool)>| Ok(Bound { lower, upper, pre: named_pre });
    let Some(ma) = ma else {
        // `*`: anything.
        return b(None, None);
    };
    match (op, mi, pa) {
        // =1.2.3 and 1.2.3-pre exactly; =1.2 and 1.2.* any 1.2.x; =1 and 1.* any 1.x.x.
        ("=", Some(mi), Some(pa)) => b(Some((full(ma, mi, pa), true)), Some((full(ma, mi, pa), true))),
        ("=" | "", mi, None) if op == "=" || wild => b(Some((lower_of(), true)), Some((floor(bump(ma, mi)?), false))),
        ("^" | "", mi, pa) => {
            let low = match (mi, pa) {
                (Some(mi), Some(pa)) => full(ma, mi, pa),
                _ => lower_of(),
            };
            let up = match (ma, mi, pa) {
                (0, Some(0), Some(pa)) => release(0, 0, pa.checked_add(1).ok_or("a version part is too large")?),
                (0, Some(mi), _) => bump(0, Some(mi))?,
                (ma, _, _) => bump(ma, None)?,
            };
            b(Some((low, true)), Some((floor(up), false)))
        }
        ("~", mi, pa) => {
            let low = match (mi, pa) {
                (Some(mi), Some(pa)) => full(ma, mi, pa),
                _ => lower_of(),
            };
            let up = match mi {
                Some(mi) => bump(ma, Some(mi))?,
                None => bump(ma, None)?,
            };
            b(Some((low, true)), Some((floor(up), false)))
        }
        (">=", mi, pa) => b(Some((pa.map(|pa| full(ma, mi.unwrap_or(0), pa)).unwrap_or_else(lower_of), true)), None),
        (">", Some(mi), Some(pa)) => b(Some((full(ma, mi, pa), false)), None),
        (">", mi, None) => b(Some((bump(ma, mi)?, true)), None),
        ("<", mi, pa) => {
            let v = pa.map(|pa| full(ma, mi.unwrap_or(0), pa)).unwrap_or_else(lower_of);
            // Below 2.0.0 means below its pre-releases too, unless one is named.
            let v = if pre.is_empty() { floor(v) } else { v };
            b(None, Some((v, false)))
        }
        ("<=", Some(mi), Some(pa)) => b(None, Some((full(ma, mi, pa), true))),
        ("<=", mi, None) => b(None, Some((floor(bump(ma, mi)?), false))),
        _ => Err(format!("'{c}' is not a version range")),
    }
}

impl Range {
    pub fn parse(s: &str) -> Result<Range, String> {
        let t = s.trim();
        if t.is_empty() {
            return Err("an empty version range; use \"*\" for any version".into());
        }
        let mut bounds = Vec::new();
        for c in t.split(',') {
            let c = c.trim();
            if c.is_empty() {
                return Err(format!("'{s}': an empty comparator between commas"));
            }
            if c.contains(char::is_whitespace) && !c.starts_with(['<', '>', '=', '^', '~']) {
                return Err(format!("'{s}': separate comparators with commas"));
            }
            bounds.push(comparator(c).map_err(|e| format!("version range '{s}': {e}"))?);
        }
        Ok(Range { text: t.to_string(), bounds })
    }

    pub fn matches(&self, v: &Version) -> bool {
        let within = self.bounds.iter().all(|b| {
            let lower_ok = match &b.lower {
                None => true,
                Some((l, true)) => v >= l,
                Some((l, false)) => v > l,
            };
            let upper_ok = match &b.upper {
                None => true,
                Some((u, true)) => v <= u,
                Some((u, false)) => v < u,
            };
            lower_ok && upper_ok
        });
        within && (!v.is_pre() || self.bounds.iter().any(|b| b.pre == Some(v.triple())))
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_are_exact() {
        for ok in ["0.0.0", "1.2.3", "10.20.30", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-0.3.7", "1.0.0-x-y.7"] {
            assert_eq!(v(ok).to_string(), ok);
        }
        for bad in ["1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.03", "v1.2.3", "1.2.3+build", "1.2.3-", "1.2.3-a..b", "1.2.3-01", "a.b.c", "", " 1.2.3", "18446744073709551616.0.0"] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn precedence_follows_semver() {
        let order = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0", "2.0.0"];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
    }

    fn m(range: &str, yes: &[&str], no: &[&str]) {
        let r = Range::parse(range).unwrap();
        for y in yes {
            assert!(r.matches(&v(y)), "{range} should match {y}");
        }
        for n in no {
            assert!(!r.matches(&v(n)), "{range} should not match {n}");
        }
    }

    #[test]
    fn caret_is_the_default() {
        m("1.2.3", &["1.2.3", "1.9.0", "1.2.10"], &["1.2.2", "2.0.0", "2.0.0-alpha", "1.3.0-beta"]);
        m("^1.2.3", &["1.2.3", "1.99.99"], &["2.0.0", "1.2.2"]);
        m("^1.2", &["1.2.0", "1.5.1"], &["1.1.9", "2.0.0"]);
        m("^1", &["1.0.0", "1.9.9"], &["0.9.9", "2.0.0"]);
        m("^0.2.3", &["0.2.3", "0.2.9"], &["0.3.0", "0.2.2"]);
        m("^0.0.3", &["0.0.3"], &["0.0.4", "0.0.2"]);
        m("^0.0", &["0.0.0", "0.0.9"], &["0.1.0"]);
        m("^0", &["0.0.0", "0.9.9"], &["1.0.0"]);
    }

    #[test]
    fn tilde_wildcards_and_comparisons() {
        m("~1.2.3", &["1.2.3", "1.2.9"], &["1.3.0", "1.2.2"]);
        m("~1.2", &["1.2.0", "1.2.9"], &["1.3.0"]);
        m("~1", &["1.0.0", "1.9.0"], &["2.0.0"]);
        m("=1.2.3", &["1.2.3"], &["1.2.4", "1.2.3-rc.1"]);
        m("=1.2", &["1.2.0", "1.2.7"], &["1.3.0"]);
        m("1.*", &["1.0.0", "1.8.2"], &["2.0.0", "0.9.0"]);
        m("1.2.*", &["1.2.0", "1.2.8"], &["1.3.0"]);
        m("*", &["0.0.0", "9.9.9"], &["1.0.0-alpha"]);
        m(">=1.2, <1.5", &["1.2.0", "1.4.9"], &["1.1.9", "1.5.0", "1.5.0-alpha"]);
        m(">1.2.3", &["1.2.4"], &["1.2.3"]);
        m(">1.2", &["1.3.0"], &["1.2.9"]);
        m("<=1.4", &["1.4.9", "0.1.0"], &["1.5.0"]);
        m("<=1.4.0", &["1.4.0"], &["1.4.1"]);
        m("<2", &["1.9.9"], &["2.0.0", "2.0.0-alpha"]);
    }

    #[test]
    fn a_pre_release_matches_only_when_named() {
        m("^1.3.0-beta.1", &["1.3.0-beta.1", "1.3.0-beta.2", "1.3.0", "1.4.0"], &["1.3.0-alpha", "1.4.0-beta.1", "2.0.0"]);
        m(">=1.0.0-rc.1, <2", &["1.0.0-rc.2", "1.5.0"], &["1.1.0-rc.1"]);
        m("=1.0.0-rc.1", &["1.0.0-rc.1"], &["1.0.0", "1.0.0-rc.2"]);
    }

    #[test]
    fn bad_ranges_are_refused() {
        for bad in ["", " ", "1.2.3 1.4", ",", "1.2,", "^", "~1.x.3", ">=1.*", "1.2.3+b", "1.2-beta", "v1", "01.2", "^1.2.3.4", "=>1.2", "18446744073709551615.*"] {
            assert!(Range::parse(bad).is_err(), "{bad}");
        }
    }
}

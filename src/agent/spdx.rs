// SPDX license expressions (SPDX Specification 2.3, Annex D), as `agent.json`'s
// `license` takes them (AGENT_MANIFEST.md section 4). The strictest reading of the
// grammar, so that one license is written one way:
//
//   expression := or
//   or         := and ( " OR " and )*
//   and        := with ( " AND " with )*
//   with       := primary ( " WITH " exception-id )?
//   primary    := license-id "+"? | "(" or ")"
//
// - Identifiers are those of the SPDX License List 3.27.0, deprecated ones left out
//   (src/agent/spdx_licenses.txt, src/agent/spdx_exceptions.txt, written by
//   scripts/spdx_lists.py), in the list's own letter case. SPDX matches identifiers
//   without regard to case; here a different case is refused, so the text shown is the
//   identifier itself.
// - `LicenseRef-` and `DocumentRef-` identifiers are refused: they name a license
//   text only the publisher has, which no one checking the agent can read.
// - The operators are upper case, separated from what is around them by exactly one
//   space; a parenthesis has no space inside it. No other white space is allowed.
// - WITH binds tighter than AND, and AND tighter than OR, as SPDX defines.

const LICENSES: &str = include_str!("spdx_licenses.txt");
const EXCEPTIONS: &str = include_str!("spdx_exceptions.txt");

fn listed(list: &'static str, id: &str) -> bool {
    list.lines().filter(|l| !l.starts_with('#')).any(|l| l == id)
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Open,
    Close,
    Word(&'a str),
}

/// The tokens of `s`, with the spacing rules checked.
fn tokens(s: &str) -> Result<Vec<Token<'_>>, String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => {
                out.push(Token::Open);
                i += 1;
            }
            b')' => {
                out.push(Token::Close);
                i += 1;
            }
            b' ' => {
                // One space, between two words, or between a word and an operator on
                // the far side of a parenthesis: never two, never inside a parenthesis,
                // never at either end.
                let before = out.last();
                let after = b.get(i + 1);
                let ok = matches!(before, Some(Token::Word(_)) | Some(Token::Close))
                    && after.is_some_and(|c| *c != b' ' && *c != b')');
                if !ok {
                    return Err("spaces must be single, between words, with none at either end or inside a parenthesis".into());
                }
                i += 1;
            }
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'.' || c == b'+' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'.' || b[i] == b'+') {
                    i += 1;
                }
                out.push(Token::Word(&s[start..i]));
            }
            _ => return Err(format!("'{}' cannot appear in a license expression", s[i..].chars().next().unwrap_or(' '))),
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<Token<'a>>,
    at: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token<'a>> {
        self.toks.get(self.at)
    }

    fn or(&mut self) -> Result<(), String> {
        self.and()?;
        while self.peek() == Some(&Token::Word("OR")) {
            self.at += 1;
            self.and()?;
        }
        Ok(())
    }

    fn and(&mut self) -> Result<(), String> {
        self.with()?;
        while self.peek() == Some(&Token::Word("AND")) {
            self.at += 1;
            self.with()?;
        }
        Ok(())
    }

    fn with(&mut self) -> Result<(), String> {
        self.primary()?;
        if self.peek() == Some(&Token::Word("WITH")) {
            self.at += 1;
            match self.peek() {
                Some(Token::Word(e)) if listed(EXCEPTIONS, e) => self.at += 1,
                Some(Token::Word(e)) => return Err(format!("'{e}' is not an SPDX license exception identifier (SPDX License List 3.27.0)")),
                _ => return Err("WITH must be followed by a license exception identifier".into()),
            }
        }
        Ok(())
    }

    fn primary(&mut self) -> Result<(), String> {
        match self.peek() {
            Some(Token::Open) => {
                self.at += 1;
                self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err("a parenthesis is not closed".into());
                }
                self.at += 1;
                Ok(())
            }
            Some(Token::Word(w)) => {
                let w = *w;
                if ["AND", "OR", "WITH"].contains(&w) {
                    return Err(format!("{w} must come between two licenses"));
                }
                if w.starts_with("LicenseRef-") || w.starts_with("DocumentRef-") {
                    return Err(format!("'{w}' names a license only the publisher has; use an SPDX License List identifier"));
                }
                let id = w.strip_suffix('+').unwrap_or(w);
                if id.contains('+') || !listed(LICENSES, id) {
                    return Err(format!("'{w}' is not an SPDX license identifier (SPDX License List 3.27.0, in its own letter case)"));
                }
                self.at += 1;
                Ok(())
            }
            Some(Token::Close) => Err("a parenthesis closes nothing".into()),
            None => Err("a license is missing".into()),
        }
    }
}

/// Checks that `s` is an SPDX license expression under the rules above.
pub fn check(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    let toks = tokens(s)?;
    // Two words with no operator between them, or an operator in the wrong place, are
    // caught by the parser: a word runs until a character that is not part of one, so
    // two words are always separated by something.
    let mut p = Parser { toks, at: 0 };
    p.or()?;
    if p.at != p.toks.len() {
        return Err("something follows the end of the expression".into());
    }
    // Spacing around operators next to parentheses: "(MIT)AND Apache-2.0" has no space
    // before AND. Every operator must have a space on both sides in the text.
    for op in ["AND", "OR", "WITH"] {
        let mut from = 0;
        while let Some(k) = s[from..].find(op) {
            let i = from + k;
            let left = s[..i].chars().next_back();
            let right = s[i + op.len()..].chars().next();
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '+');
            // Only a whole word is an operator.
            if !is_word(left) && !is_word(right) && (left != Some(' ') || right != Some(' ')) {
                return Err(format!("{op} must have one space on each side"));
            }
            from = i + op.len();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions() {
        for ok in [
            "MIT",
            "Apache-2.0",
            "GPL-2.0-or-later",
            "Apache-2.0 OR MIT",
            "MIT AND (Apache-2.0 OR BSD-3-Clause)",
            "(MIT OR Apache-2.0) AND BSD-2-Clause",
            "GPL-2.0-only WITH Classpath-exception-2.0",
            "Apache-2.0+",
            "LGPL-2.1-only OR (GPL-2.0-only WITH Classpath-exception-2.0)",
        ] {
            assert_eq!(check(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "mit",
            "MIT or Apache-2.0",
            "MIT  OR Apache-2.0",
            " MIT",
            "MIT ",
            "( MIT)",
            "(MIT )",
            "(MIT)AND Apache-2.0",
            "MIT AND(Apache-2.0)",
            "MIT Apache-2.0",
            "MIT OR",
            "OR MIT",
            "(MIT",
            "MIT)",
            "LicenseRef-mine",
            "DocumentRef-x:LicenseRef-y",
            "GPL-2.0",
            "MIT WITH Classpath-exception-2.0x",
            "MIT WITH",
            "Apache-2.0++",
            "MIT\tOR Apache-2.0",
            "MIT/Apache-2.0",
        ] {
            assert!(check(bad).is_err(), "{bad:?} was accepted");
        }
    }
}

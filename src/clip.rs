// Text from outside (a caller's arguments, a model's proposal, a target an adapter
// reports) shown inside one of the runtime's own messages. Those messages reach
// models, logs and terminals, so such text is shown with every character that could
// change how the message reads escaped, and shortened when long:
//
//   - control characters (newlines and tabs included), the Unicode line and paragraph
//     separators, and the bidirectional formatting characters, which can make text
//     display in an order other than its own (CVE-2021-42574), are written `\u{1b}`;
//   - a backslash is written `\\`, so an escape in the message always stands for one
//     character of the text and can never be forged by the text itself;
//   - text longer than SHOWN characters is cut there, followed by how long it was.
//
// Only what is shown changes: the value itself, as a program, journal or service
// receives it, is untouched.

/// The most characters of one value a message shows.
pub const SHOWN: usize = 100;

fn escaped(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{061C}' | '\u{200E}' | '\u{200F}')
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// `text` as a runtime message shows it.
pub fn clip(text: &str) -> String {
    let mut out = String::new();
    for (count, c) in text.chars().enumerate() {
        if count == SHOWN {
            let all = text.chars().count();
            out.push_str(&format!("… ({all} characters)"));
            return out;
        }
        if c == '\\' {
            out.push_str("\\\\");
        } else if escaped(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_shown_as_it_is() {
        assert_eq!(clip("4417"), "4417");
        assert_eq!(clip("Adaeze Okonkwo-Bello, +234 801 234 5678"), "Adaeze Okonkwo-Bello, +234 801 234 5678");
        assert_eq!(clip("naïve 日本 ₦"), "naïve 日本 ₦");
        assert_eq!(clip(""), "");
    }

    #[test]
    fn characters_that_change_how_a_message_reads_are_escaped() {
        assert_eq!(clip("4417\u{1b}[2J\u{0}"), "4417\\u{1b}[2J\\u{0}");
        assert_eq!(clip("a\nb\tc\r"), "a\\u{a}b\\u{9}c\\u{d}");
        assert_eq!(clip("\u{7f}\u{85}"), "\\u{7f}\\u{85}");
        assert_eq!(clip("pay\u{202e}dsu"), "pay\\u{202e}dsu");
        assert_eq!(clip("\u{2066}x\u{2069}\u{200f}\u{2028}"), "\\u{2066}x\\u{2069}\\u{200f}\\u{2028}");
        // A backslash cannot forge an escape: the text `\u{1b}` is not ESC.
        assert_eq!(clip("\\u{1b}"), "\\\\u{1b}");
    }

    #[test]
    fn long_text_is_cut_and_says_how_long_it_was() {
        let shown = clip(&"4".repeat(1_000_000));
        assert_eq!(shown, format!("{}… (1000000 characters)", "4".repeat(SHOWN)));
        assert_eq!(clip(&"x".repeat(SHOWN)), "x".repeat(SHOWN));
        assert_eq!(clip(&"x".repeat(SHOWN + 1)), format!("{}… ({} characters)", "x".repeat(SHOWN), SHOWN + 1));
        // Counted in characters, so a cut never splits one.
        assert_eq!(clip(&"日".repeat(SHOWN + 5)), format!("{}… ({} characters)", "日".repeat(SHOWN), SHOWN + 5));
    }
}

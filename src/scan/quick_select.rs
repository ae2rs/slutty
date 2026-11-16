//! Quick select: the things on screen worth copying, each with a short
//! label to pick it by.

use std::sync::OnceLock;

use regex::Regex;

use super::row::{RowText, Span};

/// WezTerm's default patterns. A pattern with a group selects the group.
const PATTERNS: [&str; 14] = [
    r"\[[^]]*\]\(([^)]+)\)",
    r"(?:https?://|git@|git://|ssh://|ftp://|file://)\S+",
    r"--- a/(\S+)",
    r"\+\+\+ b/(\S+)",
    r"sha256:([0-9a-f]{64})",
    r"(?:[.\w\-@~]+)?(?:/+[.\w\-@]+)+",
    r"#[0-9a-fA-F]{6}",
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    r"Qm[0-9a-zA-Z]{44}",
    r"[0-9a-f]{7,40}",
    r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}",
    r"[A-f0-9:]+:+[A-f0-9:]+[%\w\d]+",
    r"0x[0-9a-fA-F]+",
    r"[0-9]{4,}",
];

/// WezTerm's default label alphabet.
pub const ALPHABET: &str = "asdfqwerzxcvjklmiuopghtybn";

/// URLs only, for opening one.
pub const URL_PATTERN: &str = r"https?://\S+";

/// The patterns joined into one, so the leftmost match wins and matches do
/// not overlap.
pub fn default_patterns() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| join(&PATTERNS))
}

pub fn url_pattern() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| join(&[URL_PATTERN]))
}

fn join(patterns: &[&str]) -> Regex {
    let joined: Vec<String> = patterns.iter().map(|p| format!("(?:{p})")).collect();
    Regex::new(&joined.join("|")).expect("valid patterns")
}

/// A match and the label that picks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labelled {
    pub span: Span,
    pub label: String,
}

/// Finds matches in `rows` (top to bottom) and labels them from the bottom
/// up. The same text gets the same label.
pub fn find(rows: &[RowText], regex: &Regex) -> Vec<Labelled> {
    let mut spans = Vec::new();
    for (y, text) in rows.iter().enumerate().rev() {
        let y = u16::try_from(y).unwrap_or(u16::MAX);
        let mut row = Vec::new();
        for captures in regex.captures_iter(&text.text) {
            let found = captures.iter().skip(1).flatten().next().or_else(|| captures.get(0));
            if let Some(found) = found.filter(|m| !m.as_str().trim().is_empty()) {
                row.push(Span::new(y, text, found.range()));
            }
        }
        // Right to left, so labels read from the bottom-right corner.
        spans.extend(row.into_iter().rev());
    }
    let mut texts: Vec<&str> = Vec::new();
    for span in &spans {
        if !texts.contains(&span.text.as_str()) {
            texts.push(&span.text);
        }
    }
    let labels = labels(ALPHABET, texts.len());
    spans
        .iter()
        .map(|span| {
            let i = texts.iter().position(|t| *t == span.text).unwrap_or(0);
            Labelled { span: span.clone(), label: labels[i].clone() }
        })
        .collect()
}

/// `count` labels of equal length, so none is the start of another.
fn labels(alphabet: &str, count: usize) -> Vec<String> {
    let chars: Vec<char> = alphabet.chars().collect();
    if count <= chars.len() {
        return chars.iter().take(count).map(char::to_string).collect();
    }
    chars.iter().flat_map(|a| chars.iter().map(move |b| format!("{a}{b}"))).take(count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<RowText> {
        lines.iter().map(|l| RowText::from_str(l)).collect()
    }

    #[test]
    fn matches_are_labelled_from_the_bottom() {
        let found = find(&rows(&["commit 1a2b3c4d", "see https://a.io/x and 0xff"]), default_patterns());
        let texts: Vec<(&str, &str)> = found.iter().map(|m| (m.span.text.as_str(), m.label.as_str())).collect();
        assert_eq!(texts, vec![("0xff", "a"), ("https://a.io/x", "s"), ("1a2b3c4d", "d")]);
        assert_eq!((found[1].span.row, found[1].span.start), (1, 4));
    }

    #[test]
    fn groups_select_part_of_a_match() {
        let found = find(&rows(&["[docs](https://a.io/d)", "+++ b/src/main.rs"]), default_patterns());
        let texts: Vec<&str> = found.iter().map(|m| m.span.text.as_str()).collect();
        assert_eq!(texts, vec!["src/main.rs", "https://a.io/d"]);
    }

    #[test]
    fn the_same_text_shares_a_label() {
        let found = find(&rows(&["1234 1234 5678"]), default_patterns());
        let labels: Vec<&str> = found.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["a", "s", "s"]);
    }

    #[test]
    fn many_matches_get_two_letters() {
        let labels = labels("ab", 3);
        assert_eq!(labels, vec!["aa", "ab", "ba"]);
    }

    #[test]
    fn the_url_pattern_finds_only_urls() {
        let found = find(&rows(&["/tmp/x https://a.io 1234"]), url_pattern());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].span.text, "https://a.io");
    }
}

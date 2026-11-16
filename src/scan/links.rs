//! Links in plain text, found with WezTerm's default hyperlink rules.

use std::sync::OnceLock;

use regex::Regex;

use super::row::{RowText, Span};

/// A pattern, and how its match becomes a URL.
struct Rule {
    regex: Regex,
    /// Prepended to the match, as `mailto:`.
    prefix: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            // A URL inside (), [], {} or <> leaves the brackets out.
            (r"\((\w+://\S+)\)", ""),
            (r"\[(\w+://\S+)\]", ""),
            (r"\{(\w+://\S+)\}", ""),
            (r"<(\w+://\S+)>", ""),
            (r"\b\w+://\S+[)/a-zA-Z0-9-]+", ""),
            (r"\b\w+@[\w-]+(\.[\w-]+)+\b", "mailto:"),
        ]
        .into_iter()
        .map(|(pattern, prefix)| Rule { regex: Regex::new(pattern).expect("a valid pattern"), prefix })
        .collect()
    })
}

/// The link covering cell `col`, from the first rule with a match there.
/// The span covers what the link shows; the text is the URL to open.
pub fn link_at(row: u16, text: &RowText, col: u16) -> Option<Span> {
    rules().iter().find_map(|rule| {
        rule.regex.captures_iter(&text.text).find_map(|captures| {
            // The first group when the rule has one, else the whole match.
            let found = captures.get(1).filter(|_| rule.prefix.is_empty()).or_else(|| captures.get(0))?;
            let span = Span::new(row, text, found.range());
            span.contains(col).then(|| Span { text: format!("{}{}", rule.prefix, span.text), ..span })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(line: &str, col: u16) -> Option<String> {
        link_at(0, &RowText::from_str(line), col).map(|span| span.text)
    }

    #[test]
    fn urls_are_found_under_the_cell() {
        let line = "see https://example.com/a?b=1, then";
        assert_eq!(url(line, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url(line, 20).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url(line, 2), None);
        assert_eq!(url(line, 29), None, "the trailing comma is not part of it");
    }

    #[test]
    fn brackets_around_a_url_are_left_out() {
        assert_eq!(url("(https://a.io/x)", 3).as_deref(), Some("https://a.io/x"));
        assert_eq!(url("<https://a.io>", 1).as_deref(), Some("https://a.io"));
        let span = link_at(0, &RowText::from_str("[https://a.io/]"), 5).expect("a link");
        assert_eq!((span.start, span.end), (1, 14));
    }

    #[test]
    fn addresses_become_mailto() {
        assert_eq!(url("mail me@host.example now", 6).as_deref(), Some("mailto:me@host.example"));
    }
}

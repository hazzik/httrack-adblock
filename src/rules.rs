//! uBlock Origin HTML filters (`##^`), which the `adblock` crate rejects.

use regex::{Regex, RegexBuilder};
use scraper::Selector;

pub(crate) enum TextMatch {
    Plain(String),
    Pattern(Regex),
}

impl TextMatch {
    pub(crate) fn matches(&self, text: &str) -> bool {
        match self {
            Self::Plain(s) => text.contains(s.as_str()),
            Self::Pattern(re) => re.is_match(text),
        }
    }
}

/// A uBlock Origin HTML filter: `domains##^css:has-text(...)`.
pub(crate) struct HtmlRule {
    domains: Domains,
    /// The domains of the rule's `#@#^` exceptions.
    pub(crate) exceptions: Vec<Domains>,
    pub(crate) css: Selector,
    pub(crate) texts: Vec<TextMatch>,
}

impl HtmlRule {
    pub(crate) fn applies(&self, host: &str) -> bool {
        self.domains.matches(host) && !self.exceptions.iter().any(|e| e.matches(host))
    }
}

/// The domain list of a rule, such as `a.com,~b.a.com`.
#[derive(Clone)]
pub(crate) struct Domains {
    included: Vec<String>,
    excluded: Vec<String>,
}

impl Domains {
    pub(crate) fn parse(domains: &str) -> Self {
        let mut parsed = Self {
            included: Vec::new(),
            excluded: Vec::new(),
        };
        for domain in domains.split(',').filter(|d| !d.is_empty()) {
            match domain.strip_prefix('~') {
                Some(excluded) => parsed.excluded.push(excluded.to_owned()),
                None => parsed.included.push(domain.to_owned()),
            }
        }
        parsed
    }

    fn matches(&self, host: &str) -> bool {
        (self.included.is_empty() || self.included.iter().any(|d| host_matches(host, d)))
            && !self.excluded.iter().any(|d| host_matches(host, d))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.included.is_empty() && self.excluded.is_empty()
    }
}

/// `site.*` matches `site` with any public suffix, such as `www.site.co.uk`.
fn host_matches(host: &str, domain: &str) -> bool {
    let is_or_under = |host: &str, domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    match domain.strip_suffix(".*") {
        Some(entity) => without_public_suffix(host).is_some_and(|host| is_or_under(host, entity)),
        None => is_or_under(host, domain),
    }
}

/// `www.site.co.uk` becomes `www.site`, as in `adblock`'s `get_hostname_without_public_suffix`.
fn without_public_suffix(host: &str) -> Option<&str> {
    let url = adblock::url_parser::parse_url(&format!("https://{host}/"))?;
    let (_, suffix) = url.domain().split_once('.')?;
    host.strip_suffix(suffix)?.strip_suffix('.')
}

/// Returns the argument of `:has-text(` and the rest after its `)`.
fn has_text_argument(s: &str) -> Option<(&str, &str)> {
    let mut depth = 1;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn text_match(argument: &str) -> Option<TextMatch> {
    let Some(body) = argument.strip_prefix('/') else {
        return Some(TextMatch::Plain(argument.to_owned()));
    };
    let (pattern, flags) = body.rsplit_once('/')?;
    let pattern = RegexBuilder::new(pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .build()
        .ok()?;
    Some(TextMatch::Pattern(pattern))
}

/// Returns `None` for operators other than `:has-text` and for CSS that `scraper` rejects.
pub(crate) fn parse_html_rule(domains: &str, selector: &str) -> Option<HtmlRule> {
    let (css, mut rest) = match selector.find(":has-text(") {
        Some(i) => selector.split_at(i),
        None => (selector, ""),
    };
    let mut texts = Vec::new();
    while let Some(after) = rest.strip_prefix(":has-text(") {
        let (argument, after) = has_text_argument(after)?;
        texts.push(text_match(argument)?);
        rest = after;
    }
    if css.is_empty() || !rest.is_empty() {
        return None;
    }
    let css = Selector::parse(css).ok()?;
    Some(HtmlRule {
        domains: Domains::parse(domains),
        exceptions: Vec::new(),
        css,
        texts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_match_subdomains_and_skip_exclusions() {
        let domains = Domains::parse("example.com,~ads.example.com");
        assert!(domains.matches("example.com"));
        assert!(domains.matches("www.example.com"));
        assert!(!domains.matches("ads.example.com"));
        assert!(!domains.matches("notexample.com"));
        assert!(Domains::parse("~ads.example.com").matches("other.org"));
        assert!(Domains::parse("").is_empty());
    }

    #[test]
    fn a_wildcard_domain_matches_any_public_suffix() {
        assert!(host_matches("www.site.co.uk", "site.*"));
        assert!(host_matches("site.com", "site.*"));
        assert!(!host_matches("othersite.com", "site.*"));
    }

    #[test]
    fn has_text_argument_skips_nested_and_escaped_parentheses() {
        assert_eq!(has_text_argument("a(b)c):x"), Some(("a(b)c", ":x")));
        assert_eq!(has_text_argument(r"a\)b)"), Some((r"a\)b", "")));
        assert_eq!(has_text_argument("open"), None);
    }

    #[test]
    fn parse_html_rule_reads_plain_and_regex_texts() {
        let rule = parse_html_rule("example.com", "div.ad:has-text(Sponsored):has-text(/^buy$/i)").unwrap();
        assert!(rule.applies("example.com"));
        assert!(!rule.applies("other.org"));
        assert!(rule.texts[0].matches("Sponsored by us"));
        assert!(rule.texts[1].matches("BUY"));
        assert!(!rule.texts[1].matches("buy now"));
    }

    #[test]
    fn parse_html_rule_rejects_what_the_plugin_cannot_apply() {
        assert!(parse_html_rule("", "div:upward(2)").is_none());
        assert!(parse_html_rule("", ":has-text(x)").is_none());
        assert!(parse_html_rule("", "div:has-text(x):upward(1)").is_none());
        assert!(parse_html_rule("", "div:has-text(/[/)").is_none());
    }
}

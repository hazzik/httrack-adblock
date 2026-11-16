//! Finds the encoding of a page as the HTML standard does, and decodes the page.

use std::borrow::Cow;
use std::sync::LazyLock;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use regex::bytes::Regex;
use scraper::{Html, Selector};
use url::Url;

pub(crate) const UTF_8_BOM: &[u8] = b"\xEF\xBB\xBF";

static CHARSET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)charset\s*=\s*["']?([a-z0-9_-]+)"#).expect("the regex is valid"));
static CHARSET_META: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("meta[charset], meta[http-equiv]").expect("the selector is valid"));

/// The encoding that the `charset` parameter of a `Content-Type` value names.
pub(crate) fn from_content_type(value: &[u8]) -> Option<&'static Encoding> {
    let label = CHARSET.captures(value)?.get(1)?.as_bytes();
    Encoding::for_label(label).map(Encoding::output_encoding)
}

/// Parses `body` in the `declared` encoding. Without one, the encoding comes from `<meta>`,
/// then from a guess.
pub(crate) fn parse(body: &[u8], page: &Url, declared: Option<&'static Encoding>) -> (&'static Encoding, Html) {
    if let Some(encoding) = declared {
        let (encoding, text) = decode(encoding, body);
        return (encoding, Html::parse_document(&text));
    }
    let (guess, text) = match std::str::from_utf8(body) {
        Ok(text) => (UTF_8, Cow::Borrowed(text)),
        Err(_) => decode(guess(body, page), body),
    };
    let doc = Html::parse_document(&text);
    match meta_encoding(&doc) {
        Some(meta) if meta != guess => {
            let (meta, meta_text) = decode(meta, body);
            let doc = if meta_text == text {
                doc
            } else {
                Html::parse_document(&meta_text)
            };
            (meta, doc)
        }
        _ => (guess, doc),
    }
}

fn meta_encoding(doc: &Html) -> Option<&'static Encoding> {
    doc.select(&CHARSET_META).find_map(|el| {
        let el = el.value();
        match el.attr("charset") {
            Some(charset) => Encoding::for_label(charset.trim().as_bytes()).map(Encoding::output_encoding),
            None if el.attr("http-equiv")?.trim().eq_ignore_ascii_case("content-type") => {
                from_content_type(el.attr("content")?.as_bytes())
            }
            None => None,
        }
    })
}

/// Guesses as Firefox does for a page that declares no encoding.
fn guess(body: &[u8], page: &Url) -> &'static Encoding {
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(body, true);
    let tld = page.domain().and_then(|domain| domain.rsplit('.').next());
    detector.guess(tld.map(str::as_bytes), Utf8Detection::Deny)
}

/// Falls back to windows-1252, which keeps every byte.
fn decode<'a>(encoding: &'static Encoding, body: &'a [u8]) -> (&'static Encoding, Cow<'a, str>) {
    match encoding.decode_without_bom_handling_and_without_replacement(body) {
        Some(text) => (encoding, text),
        None => (WINDOWS_1252, WINDOWS_1252.decode_without_bom_handling(body).0),
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::{SHIFT_JIS, WINDOWS_1251};

    use super::*;

    #[test]
    fn from_content_type_reads_the_charset_parameter() {
        assert_eq!(from_content_type(b"text/html; charset=\"Shift_JIS\""), Some(SHIFT_JIS));
        assert_eq!(from_content_type(b"text/html"), None);
        assert_eq!(from_content_type(b"text/html; charset=nonsense"), None);
    }

    #[test]
    fn parse_follows_a_meta_charset() {
        let page = Url::parse("http://example.com/").unwrap();
        let mut body = b"<meta charset=windows-1251><p>".to_vec();
        body.extend_from_slice(&WINDOWS_1251.encode("\u{0442}\u{0435}\u{043a}\u{0441}\u{0442}").0);
        let (encoding, doc) = parse(&body, &page, None);
        assert_eq!(encoding, WINDOWS_1251);
        assert!(
            doc.root_element()
                .text()
                .any(|text| text == "\u{0442}\u{0435}\u{043a}\u{0441}\u{0442}")
        );
    }

    #[test]
    fn decode_falls_back_to_windows_1252() {
        let (encoding, text) = decode(UTF_8, b"caf\xE9");
        assert_eq!(encoding, WINDOWS_1252);
        assert_eq!(text, "caf\u{e9}");
    }
}

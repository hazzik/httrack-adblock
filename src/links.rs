//! The URLs of links, and the request types that the plugin checks a link as.

use percent_encoding::{AsciiSet, percent_encode};
use url::Url;

/// The request types that the `adblock` crate knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestType {
    Document,
    Font,
    Image,
    Media,
    Object,
    Other,
    Script,
    Stylesheet,
    SubFrame,
}

impl RequestType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Font => "font",
            Self::Image => "image",
            Self::Media => "media",
            Self::Object => "object",
            Self::Other => "other",
            Self::Script => "script",
            Self::Stylesheet => "stylesheet",
            Self::SubFrame => "sub_frame",
        }
    }
}

/// HTTrack follows every link, so the plugin checks a page link as a page and as any file.
pub(crate) const PAGE_LINK_TYPES: &[RequestType] = &[RequestType::Document, RequestType::Other];

/// `adr` and `fil` can be in the page's charset.
pub(crate) fn page_url(adr: &[u8], fil: &[u8]) -> Option<Url> {
    let scheme = if adr.windows(3).any(|w| w == b"://") {
        ""
    } else {
        "http://"
    };
    let (adr, fil) = (
        percent_encode(adr, &AsciiSet::EMPTY),
        percent_encode(fil, &AsciiSet::EMPTY),
    );
    Url::parse(&format!("{scheme}{adr}{fil}")).ok()
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle))
}

pub(crate) fn tag_name(tag: &[u8]) -> Vec<u8> {
    let tag = tag.strip_prefix(b"<").unwrap_or(tag);
    tag.iter()
        .take_while(|b| b.is_ascii_alphanumeric())
        .map(u8::to_ascii_lowercase)
        .collect()
}

/// HTTrack's `link_has_authority`.
pub(crate) fn has_authority(link: &str) -> bool {
    let rest = link.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let rest = if rest.len() == link.len() {
        rest
    } else {
        let Some(rest) = rest.strip_prefix(':') else {
            return false;
        };
        rest
    };
    rest.starts_with("//")
}

/// HTTrack's `link_base_is_hostname`.
pub(crate) fn base_is_hostname(link: &str) -> bool {
    !link.starts_with('.')
        && link
            .chars()
            .take_while(|c| !matches!(c, '/' | '?' | '#'))
            .any(|c| matches!(c, '.' | ':'))
}

/// The plugin blocks the link when a rule blocks any of these types.
pub(crate) fn request_types_for_tag(tag: &[u8]) -> &'static [RequestType] {
    match tag_name(tag).as_slice() {
        b"script" => &[RequestType::Script],
        b"img" | b"image" | b"input" => &[RequestType::Image],
        b"link" if find_ci(tag, b"stylesheet").is_some() => &[RequestType::Stylesheet],
        b"iframe" | b"frame" => &[RequestType::SubFrame],
        b"embed" | b"object" => &[RequestType::Object],
        b"audio" | b"video" | b"source" => &[RequestType::Media],
        b"a" | b"area" => PAGE_LINK_TYPES,
        _ => &[RequestType::Other],
    }
}

/// A tag that allows `Other` does not decide the type, so the file extension adds one.
pub(crate) fn with_path_type(types: &[RequestType], link: &str) -> Vec<RequestType> {
    let mut types = types.to_vec();
    let from_path = request_type_for_path(link);
    if types.contains(&RequestType::Other) && !types.contains(&from_path) {
        types.push(from_path);
    }
    types
}

fn request_type_for_path(path: &str) -> RequestType {
    let path = path.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "js" | "mjs" => RequestType::Script,
        "css" => RequestType::Stylesheet,
        "png" | "gif" | "jpg" | "jpeg" | "webp" | "svg" | "ico" | "bmp" => RequestType::Image,
        "woff" | "woff2" | "ttf" | "otf" | "eot" => RequestType::Font,
        "mp4" | "webm" | "ogv" | "mov" | "mp3" | "m4a" | "ogg" | "oga" | "opus" | "wav" | "flac" => RequestType::Media,
        _ => RequestType::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_url_adds_http_to_a_host_without_scheme() {
        assert_eq!(
            page_url(b"example.com", b"/a b").unwrap().as_str(),
            "http://example.com/a%20b"
        );
        assert_eq!(
            page_url(b"https://example.com", b"/").unwrap().as_str(),
            "https://example.com/"
        );
    }

    #[test]
    fn tag_name_is_lowercase_and_stops_at_the_first_other_byte() {
        assert_eq!(tag_name(b"<IMG src=x"), b"img");
        assert_eq!(tag_name(b"a href"), b"a");
    }

    #[test]
    fn has_authority_follows_httrack() {
        assert!(has_authority("//cdn.example.com/x.js"));
        assert!(has_authority("https://example.com"));
        assert!(!has_authority("www.example.com"));
        assert!(!has_authority("mailto:me@example.com"));
    }

    #[test]
    fn base_is_hostname_follows_httrack() {
        assert!(base_is_hostname("www.example.com"));
        assert!(base_is_hostname("localhost:8080/"));
        assert!(!base_is_hostname("./dir/"));
        assert!(!base_is_hostname("dir/page.html"));
    }

    #[test]
    fn request_types_for_tag_reads_the_tag_name() {
        assert_eq!(request_types_for_tag(b"<script src="), [RequestType::Script]);
        assert_eq!(
            request_types_for_tag(b"<link rel=\"StyleSheet\" href="),
            [RequestType::Stylesheet]
        );
        assert_eq!(request_types_for_tag(b"<link rel=\"icon\" href="), [RequestType::Other]);
        assert_eq!(request_types_for_tag(b"<a href="), PAGE_LINK_TYPES);
    }

    #[test]
    fn with_path_type_adds_the_type_of_the_extension_to_other() {
        assert_eq!(
            with_path_type(&[RequestType::Other], "/x.JS?v=1"),
            [RequestType::Other, RequestType::Script]
        );
        assert_eq!(with_path_type(&[RequestType::Other], "/page"), [RequestType::Other]);
        assert_eq!(with_path_type(&[RequestType::Image], "/x.js"), [RequestType::Image]);
    }
}

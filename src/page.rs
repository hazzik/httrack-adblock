//! Removes blocked elements from an HTML page and writes it back out.

use std::collections::HashSet;
use std::io::{self, Write as _};
use std::sync::LazyLock;

use encoding_rs::{Encoding, UTF_8};
use html5ever::serialize::{SerializeOpts, TraversalScope, serialize};
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector, StrTendril};
use url::Url;

use crate::encoding::{self, UTF_8_BOM};
use crate::links::{RequestType, request_types_for_element};
use crate::state::State;

static SCRIPT_BLOCKS: LazyLock<Selector> = LazyLock::new(|| selector("script, noscript"));
static BASE_HREF: LazyLock<Selector> = LazyLock::new(|| selector("base[href]"));
static LEADING_NEWLINE: LazyLock<Selector> = LazyLock::new(|| selector("pre, textarea, listing"));
/// Elements whose `src` the plugin checks.
static SOURCES: LazyLock<Selector> = LazyLock::new(|| selector("img[src], iframe[src], frame[src], embed[src]"));
static URL_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?:)?//[a-z0-9][a-z0-9.-]*\.[a-z]{2,}(?::\d+)?(?:/[^\s"'<>\\()]*)?"#)
        .expect("the regex is valid")
});

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("the selector is valid")
}

/// Returns `None` when the plugin removed nothing. The encoding comes from the byte order
/// mark, then the header, then `<meta>`, as in the HTML standard, then a guess.
pub(crate) fn process(
    state: &State,
    html: &[u8],
    url: &Url,
    header_encoding: Option<&'static Encoding>,
) -> Option<Vec<u8>> {
    let (bom, body) = match html.strip_prefix(UTF_8_BOM) {
        Some(body) => (UTF_8_BOM, body),
        None => (&[][..], html),
    };
    let declared = if bom.is_empty() { header_encoding } else { Some(UTF_8) };
    let (encoding, mut doc) = encoding::parse(body, url, declared);
    let base = doc
        .select(&BASE_HREF)
        .next()
        .and_then(|el| url.join(el.value().attr("href")?).ok());
    let page = Page { state, url, base };
    if page.remove_blocked(&mut doc) == 0 {
        return None;
    }
    let mut out = bom.to_vec();
    out.extend_from_slice(&encoding.encode(&to_html(doc)).0);
    Some(out)
}

/// HTTrack also passes scripts, style sheets and SVG images to `preprocess-html`.
pub(crate) fn starts_like_markup(buffer: &[u8]) -> bool {
    let rest = buffer.strip_prefix(UTF_8_BOM).unwrap_or(buffer);
    let start = rest.trim_ascii_start();
    start.starts_with(b"<") && !starts_with_ci(start, b"<?xml") && !starts_with_ci(start, b"<svg")
}

fn starts_with_ci(hay: &[u8], prefix: &[u8]) -> bool {
    hay.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

struct Page<'a> {
    state: &'a State,
    url: &'a Url,
    /// The first `<base href>`, as in a browser.
    base: Option<Url>,
}

impl Page<'_> {
    fn blocks(&self, link: &str, request_types: &[RequestType]) -> bool {
        self.state
            .filters
            .blocks_link(link, self.base.as_ref(), Some(self.url), request_types)
    }

    fn mentions_blocked_url(&self, text: &str, request_types: &[RequestType]) -> bool {
        URL_IN_TEXT
            .find_iter(text)
            .any(|m| self.blocks(m.as_str(), request_types))
    }

    fn html_rule_matches<'d>(&self, doc: &'d Html) -> Vec<ElementRef<'d>> {
        let host = self.url.host_str().unwrap_or("");
        self.state
            .filters
            .html_rules
            .iter()
            .filter(|rule| rule.applies(host))
            .flat_map(|rule| {
                doc.select(&rule.css).filter(|el| {
                    rule.texts.is_empty() || {
                        let text: String = el.text().collect();
                        rule.texts.iter().all(|t| t.matches(&text))
                    }
                })
            })
            .collect()
    }

    /// `<script>` and `<noscript>` elements that load or mention a blocked URL.
    fn blocked_scripts<'d>(&self, doc: &'d Html) -> Vec<ElementRef<'d>> {
        doc.select(&SCRIPT_BLOCKS)
            .filter(|el| {
                let blocked_src = el.value().name() == "script"
                    && el
                        .value()
                        .attr("src")
                        .is_some_and(|src| self.blocks(src, &[RequestType::Script]));
                blocked_src || {
                    let text: String = el.text().collect();
                    self.mentions_blocked_url(&text, &[RequestType::Script, RequestType::Image])
                }
            })
            .collect()
    }

    fn hiding_selectors(&self, doc: &Html) -> Vec<String> {
        let engine = &self.state.filters.engine;
        let resources = engine.url_cosmetic_resources(self.url.as_str());
        let mut selectors: Vec<String> = resources.hide_selectors.into_iter().collect();
        if !resources.generichide {
            let mut classes = HashSet::new();
            let mut ids = HashSet::new();
            for el in doc.root_element().descendants().filter_map(ElementRef::wrap) {
                classes.extend(el.value().classes());
                ids.extend(el.value().id());
            }
            selectors.extend(engine.hidden_class_id_selectors(&classes, &ids, &resources.exceptions));
        }
        selectors
    }

    /// Elements in `SOURCES` whose `src` is blocked.
    fn blocked_sources<'d>(&self, doc: &'d Html) -> Vec<ElementRef<'d>> {
        doc.select(&SOURCES)
            .filter(|el| {
                let request_types = request_types_for_element(el.value().name().as_bytes());
                el.value()
                    .attr("src")
                    .is_some_and(|src| self.blocks(src, request_types))
            })
            .collect()
    }

    fn remove_blocked(&self, doc: &mut Html) -> usize {
        let stats = &self.state.stats;
        let selectors = self.hiding_selectors(doc);
        let parsed: Vec<Selector> = selectors.iter().filter_map(|s| Selector::parse(s).ok()).collect();
        let found: Vec<_> = [
            (self.html_rule_matches(doc), &stats.removed_html),
            (self.blocked_scripts(doc), &stats.removed_blocks),
            (
                parsed.iter().flat_map(|s| doc.select(s)).collect(),
                &stats.removed_elements,
            ),
            (self.blocked_sources(doc), &stats.removed_elements),
        ]
        .into_iter()
        .flat_map(|(elements, counter)| elements.into_iter().map(move |el| (el.id(), counter)))
        .collect();

        // Skip elements inside a removed element.
        let marked: HashSet<_> = found.iter().map(|&(id, _)| id).collect();
        let mut removed = HashSet::new();
        for (id, counter) in found {
            let Some(node) = doc.tree.get(id) else {
                continue;
            };
            if node.ancestors().any(|a| marked.contains(&a.id())) || !removed.insert(id) {
                continue;
            }
            if let Some(mut node) = doc.tree.get_mut(id) {
                node.detach();
            }
            counter.update(|n| n + 1);
        }
        if self.state.debug {
            let mut shown = self.url.clone();
            let _ = shown.set_username("");
            let _ = shown.set_password(None);
            eprintln!(
                "httrack-adblock: {shown}: {} of {} element-hiding selectors parsed, {} elements removed",
                parsed.len(),
                selectors.len(),
                removed.len()
            );
        }
        removed.len()
    }
}

/// The parser drops the first newline after `<pre>`, `<textarea>` and `<listing>`,
/// and html5ever's serializer does not write it back.
fn keep_leading_newlines(doc: &mut Html) {
    let texts: Vec<_> = doc
        .select(&LEADING_NEWLINE)
        .filter_map(|el| el.first_child())
        .filter(|node| node.value().as_text().is_some_and(|text| text.starts_with('\n')))
        .map(|node| node.id())
        .collect();
    for id in texts {
        if let Some(mut node) = doc.tree.get_mut(id)
            && let Node::Text(text) = node.value()
        {
            text.text = StrTendril::from(format!("\n{}", &*text.text));
        }
    }
}

/// `Html::html()` escapes `<noscript>` content and drops the doctype's public and system IDs.
fn to_html(mut doc: Html) -> String {
    keep_leading_newlines(&mut doc);
    let mut out = Vec::new();
    write_html(&doc, &mut out).expect("writing to a `Vec` does not fail");
    String::from_utf8(out).expect("html5ever writes UTF-8")
}

fn write_html(doc: &Html, out: &mut Vec<u8>) -> io::Result<()> {
    let opts = SerializeOpts {
        scripting_enabled: true,
        traversal_scope: TraversalScope::IncludeNode,
        create_missing_parent: false,
    };
    for node in doc.tree.root().children() {
        match node.value() {
            Node::Doctype(doctype) => {
                write!(out, "<!DOCTYPE {}", doctype.name())?;
                let (public, system) = (doctype.public_id(), doctype.system_id());
                if !public.is_empty() {
                    write!(out, " PUBLIC {}", quote(public))?;
                } else if !system.is_empty() {
                    out.extend_from_slice(b" SYSTEM");
                }
                if !system.is_empty() {
                    write!(out, " {}", quote(system))?;
                }
                out.push(b'>');
            }
            Node::Comment(comment) => write!(out, "<!--{}-->", &**comment)?,
            _ => {
                if let Some(element) = ElementRef::wrap(node) {
                    serialize(&mut *out, &element, opts.clone())?;
                }
            }
        }
    }
    Ok(())
}

/// A doctype ID can contain one kind of quote but not both.
fn quote(id: &str) -> String {
    if id.contains('"') {
        format!("'{id}'")
    } else {
        format!("\"{id}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_like_markup_skips_xml_and_svg() {
        assert!(starts_like_markup(b"\xEF\xBB\xBF  <!DOCTYPE html>"));
        assert!(!starts_like_markup(b"<?xml version=\"1.0\"?><html>"));
        assert!(!starts_like_markup(b"<SVG xmlns="));
        assert!(!starts_like_markup(b"var x = 1;"));
        assert!(!starts_like_markup(b""));
    }

    #[test]
    fn to_html_keeps_the_doctype_ids_and_top_comments() {
        let html = "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">\
                    <!--top--><html><head></head><body><noscript><b>x</b></noscript></body></html>";
        assert_eq!(to_html(Html::parse_document(html)), html);
    }

    #[test]
    fn to_html_writes_back_the_newline_after_pre() {
        let doc = Html::parse_document("<pre>\n\nx</pre>");
        assert!(to_html(doc).contains("<pre>\n\nx</pre>"));
    }
}

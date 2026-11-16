//! What the plugin does when HTTrack calls it. `httrack` converts HTTrack's raw arguments and calls
//! the function here with the same name.

use std::borrow::Cow;
use std::ffi::CStr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};

use percent_encoding::{AsciiSet, percent_encode};
use url::Url;

use crate::encoding;
use crate::filters::LoadError;
use crate::headers::Response;
use crate::httrack::{Options, forget_previous_callbacks, register_callbacks};
use crate::links::{PAGE_LINK_TYPES, page_url, request_types_for_tag, tag_name};
use crate::page;
use crate::state::{self, STATE, State};

const HTML_MIME: &str = "text/html";
/// HTTrack's type for a response without `Content-Type`.
const HTS_UNKNOWN_MIME: &str = "unknown/unknown";
/// Copy of `is_hypertext_mime__` in HTTrack's `htslib.h`.
const PARSED_MIME_TYPES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "text/javascript",
    "application/javascript",
    "application/x-javascript",
    "application/ecmascript",
    "text/css",
    "image/svg+xml",
    "image/svg-xml",
    "application/x-authorware-map",
];
/// HTTrack's internal page that lists the start URLs.
const PRIMARY_ADR: &[u8] = b"primary";
const PRIMARY_FIL: &[u8] = b"/primary";

/// `hts_plug` calls that `hts_unplug` has not matched yet. HTTrack calls
/// `hts_unplug` after a failed `hts_plug` too.
static PLUGS: AtomicUsize = AtomicUsize::new(0);

/// Returns `None` when the plugin cannot run.
pub(crate) fn hts_plug(opts: Options<'_>, argv: Option<&CStr>) -> Option<()> {
    let first = PLUGS.fetch_add(1, Ordering::Relaxed) == 0;
    let loaded = catch_unwind(|| load(argv)).ok()?;
    let loaded = loaded.inspect_err(|error| eprintln!("httrack-adblock: {error}")).ok()?;
    if let Some(state) = loaded {
        STATE.set(Some(state));
    }
    // A plugin that HTTrack loaded since the first `-%W` for this plugin calls this
    // plugin's callbacks. Registering them again would make the two call each other.
    if first && let Err(name) = register_callbacks(opts) {
        eprintln!(
            "httrack-adblock: HTTrack has no callback named {}",
            name.to_string_lossy()
        );
        return None;
    }
    Some(())
}

/// Returns `None` when `argv` adds no list to the loaded ones.
fn load(argv: Option<&CStr>) -> Result<Option<State>, LoadError> {
    let argv = argv.and_then(|argv| argv.to_str().ok()).unwrap_or("");
    let (_, lists) = argv.split_once(',').ok_or(LoadError::NoLists)?;
    state::load(STATE.get().as_deref(), lists)
}

pub(crate) fn hts_unplug() {
    if PLUGS.fetch_sub(1, Ordering::Relaxed) == 1 {
        STATE.set(None);
        forget_previous_callbacks();
    }
}

/// Makes the page at `adr` and `fil` the current page, and returns it without the blocked elements.
/// Returns `None` for HTTrack's list of start URLs, for a file that is not HTML, and for a page that
/// the plugin did not change.
pub(crate) fn on_preprocess(
    opts: Options<'_>,
    adr: Option<&CStr>,
    fil: Option<&CStr>,
    input: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let state = STATE.get()?;
    let (adr, fil) = (adr?.to_bytes(), fil?.to_bytes());
    let start_urls = adr == PRIMARY_ADR && fil == PRIMARY_FIL;
    let page = if start_urls { None } else { page_url(adr, fil) };
    *state.page.borrow_mut() = page.clone();
    *state.base.borrow_mut() = None;
    let page = page?;
    let header_encoding = state.header_encodings.borrow_mut().remove(page.as_str());
    let input = input?;
    if !is_html(&state, opts, &page, fil, input) {
        return None;
    }
    page::process(&state, input, &page, header_encoding)
}

/// A browser parses a `text/html` response as HTML however it starts.
fn is_html(state: &State, opts: Options<'_>, page: &Url, fil: &[u8], buffer: &[u8]) -> bool {
    let reported = state.mime_types.borrow_mut().remove(page.as_str());
    match reported {
        Some(is_html) => is_html,
        None => opts.name_is_html(fil).unwrap_or(true) && page::starts_like_markup(buffer),
    }
}

/// Returns `true` to drop the link. HTTrack also calls `link-detected2` for the start URLs.
pub(crate) fn on_linkdetected2(link: Option<&CStr>, tag: Option<&[u8]>) -> bool {
    count_blocked(|state| {
        // HTTrack passes the link in the page's charset.
        let link: Cow<'_, str> = percent_encode(link?.to_bytes(), &AsciiSet::EMPTY).into();
        let Some(tag) = tag else {
            // `htsAddLink` adds links from sitemaps and Java classes, and no callback
            // tells which file they came from. HTTrack treats a sitemap link as a start URL.
            return Some(state.filters.blocks_link(&link, None, None, PAGE_LINK_TYPES));
        };
        let name = tag_name(tag);
        // HTTrack does not download a `<base href>`, and it ignores one that a callback
        // refused, which would resolve the page's links against the wrong URL.
        if name == b"base" {
            state.set_base(&link);
            return Some(false);
        }
        let tag_types = if state.page.borrow().is_none() {
            PAGE_LINK_TYPES
        } else {
            request_types_for_tag(&name, tag)
        };
        Some(state.blocks_link(&link, tag_types))
    })
}

/// Returns `true` to refuse the link. Checks only redirect targets: `on_linkdetected2` already
/// checked page links.
pub(crate) fn on_check_link(adr: Option<&CStr>, fil: Option<&CStr>, refused: bool) -> bool {
    count_blocked(|state| {
        if state.redirects.borrow().is_empty() {
            return None;
        }
        let url = page_url(adr?.to_bytes(), fil?.to_bytes())?;
        let source = state.redirects.borrow_mut().remove(url.as_str())?;
        if refused {
            return None;
        }
        Some(state.filters.blocks(&url, &source, PAGE_LINK_TYPES))
    })
}

fn count_blocked(check: impl FnOnce(&State) -> Option<bool>) -> bool {
    let Some(state) = STATE.get() else {
        return false;
    };
    let blocked = matches!(catch_unwind(AssertUnwindSafe(|| check(&state))), Ok(Some(true)));
    if blocked {
        state.stats.blocked_links.update(|n| n + 1);
    }
    blocked
}

/// Saves redirect targets and header charsets. `referer_adr` and `referer_fil` are the host and
/// path of the page that linked to the response.
pub(crate) fn on_receive_header(
    headers: Option<&CStr>,
    adr: Option<&CStr>,
    fil: Option<&CStr>,
    referer_adr: Option<&CStr>,
    referer_fil: Option<&CStr>,
) -> Option<()> {
    let state = STATE.get()?;
    let response = Response::parse(headers?.to_bytes())?;
    let from = page_url(adr?.to_bytes(), fil?.to_bytes())?;
    if response.is_redirect() {
        // HTTrack sends no referer for a start URL, so the start URL is the source.
        let source = match (referer_adr, referer_fil) {
            (Some(adr), Some(fil)) if !adr.is_empty() => page_url(adr.to_bytes(), fil.to_bytes())?,
            _ => from.clone(),
        };
        record_redirect(&state, &from, response.header(b"location")?, source)
    } else if response.is_success() {
        record_encoding(&state, from, response.header(b"content-type")?)
    } else {
        None
    }
}

fn record_redirect(state: &State, from: &Url, location: &[u8], source: Url) -> Option<()> {
    let mut target = from.join(std::str::from_utf8(location).ok()?).ok()?;
    target.set_fragment(None);
    state.redirects.borrow_mut().insert(target.into(), source);
    Some(())
}

fn record_encoding(state: &State, page: Url, content_type: &[u8]) -> Option<()> {
    let (mime, parameters) = std::str::from_utf8(content_type).ok()?.split_once(';')?;
    if !is_parsed_mime(mime.trim()) {
        return None;
    }
    let encoding = encoding::from_content_type(parameters.as_bytes())?;
    state.header_encodings.borrow_mut().insert(page.into(), encoding);
    Some(())
}

/// Saves the MIME type of each parsed file for `is_html`.
pub(crate) fn on_check_mime(
    opts: Options<'_>,
    adr: Option<&CStr>,
    fil: Option<&CStr>,
    mime: Option<&CStr>,
) -> Option<()> {
    let state = STATE.get()?;
    let mime = mime?;
    let name = mime.to_str().ok()?.split(';').next()?.trim();
    if !is_parsed_mime(name) {
        return None;
    }
    // HTTrack guesses these from the file name, and so does `is_html`.
    if name.eq_ignore_ascii_case(HTS_UNKNOWN_MIME) || opts.may_unknown(mime) {
        return None;
    }
    let url = page_url(adr?.to_bytes(), fil?.to_bytes())?;
    let is_html = name.eq_ignore_ascii_case(HTML_MIME);
    state.mime_types.borrow_mut().insert(url.into(), is_html);
    Some(())
}

fn is_parsed_mime(mime: &str) -> bool {
    PARSED_MIME_TYPES.iter().any(|parsed| mime.eq_ignore_ascii_case(parsed))
}

pub(crate) fn on_end() {
    if let Some(state) = STATE.get() {
        eprintln!("httrack-adblock: {}", state.stats);
    }
}

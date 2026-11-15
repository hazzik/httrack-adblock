//! What the plugin keeps between HTTrack's callbacks.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

use encoding_rs::Encoding;
use url::Url;

use crate::filters::{self, Filters, LoadError};
use crate::httrack::Global;
use crate::links::{RequestType, base_is_hostname, has_authority};

pub(crate) struct State {
    pub(crate) filters: Filters,
    pub(crate) debug: bool,
    /// `None` before the first page and on HTTrack's list of start URLs.
    pub(crate) page: RefCell<Option<Url>>,
    /// The last `<base href>` that HTTrack found on the page, as in HTTrack.
    pub(crate) base: RefCell<Option<Url>>,
    /// Redirect target -> the page that linked to the redirecting URL.
    pub(crate) redirects: RefCell<HashMap<String, Url>>,
    /// Page URL -> charset from the `Content-Type` header.
    pub(crate) header_encodings: RefCell<HashMap<String, &'static Encoding>>,
    /// Page URL -> MIME type from `check-mime`.
    pub(crate) mime_types: RefCell<HashMap<String, String>>,
    pub(crate) stats: Stats,
}

impl State {
    fn new(filters: Filters) -> Self {
        Self {
            filters,
            debug: std::env::var_os("HAB_DEBUG").is_some_and(|v| !v.is_empty() && v != "0"),
            page: RefCell::default(),
            base: RefCell::default(),
            redirects: RefCell::default(),
            header_encodings: RefCell::default(),
            mime_types: RefCell::default(),
            stats: Stats::default(),
        }
    }

    /// `link` is relative to the current page's base URL.
    pub(crate) fn blocks_link(&self, link: &str, request_type: RequestType) -> bool {
        self.filters.blocks_link(
            link,
            self.base.borrow().as_ref(),
            self.page.borrow().as_ref(),
            request_type,
        )
    }

    /// Follows HTTrack's `htsparse`, which reads `<base href="www.example.com">` as a host name.
    pub(crate) fn set_base(&self, href: &str) {
        let page = self.page.borrow();
        let Some(page) = page.as_ref() else { return };
        let base = if !has_authority(href) && base_is_hostname(href) {
            Url::parse(&format!("http://{href}"))
        } else {
            page.join(href)
        };
        if let Ok(base) = base {
            *self.base.borrow_mut() = Some(base);
        }
    }
}

#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) blocked_links: Cell<u64>,
    pub(crate) removed_html: Cell<u64>,
    pub(crate) removed_blocks: Cell<u64>,
    pub(crate) removed_elements: Cell<u64>,
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blocked {} links, removed {} elements with HTML filters, {} script blocks and {} elements with element-hiding rules",
            self.blocked_links.get(),
            self.removed_html.get(),
            self.removed_blocks.get(),
            self.removed_elements.get()
        )
    }
}

pub(crate) static STATE: Global<State> = Global::new();

/// Adds the lists in `new_paths` to the lists of earlier `-%W` options, which
/// `httrack --update` repeats from `hts-cache/doit.log`. Returns `None` when `new_paths` adds no list.
pub(crate) fn load(previous: Option<&State>, new_paths: &str) -> Result<Option<State>, LoadError> {
    let mut paths = previous.map_or_else(BTreeSet::new, |state| state.filters.paths().clone());
    let known = paths.len();
    for path in new_paths.split(',').filter(|p| !p.is_empty()) {
        filters::add_list_files(Path::new(path), &mut paths)?;
    }
    if previous.is_some() && paths.len() == known {
        return Ok(None);
    }
    Ok(Some(State::new(Filters::load(paths)?)))
}

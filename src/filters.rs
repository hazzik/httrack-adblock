//! Loads the filter lists and checks URLs against them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use adblock::Engine;
use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use url::Url;

use crate::links::{RequestType, with_path_type};
use crate::rules::{Domains, HtmlRule, parse_html_rule};

pub(crate) struct Filters {
    /// Canonical paths, from every `-%W`.
    pub(crate) paths: BTreeSet<PathBuf>,
    pub(crate) engine: Engine,
    pub(crate) html_rules: Vec<HtmlRule>,
}

impl Filters {
    pub(crate) fn load(paths: BTreeSet<PathBuf>) -> Result<Self, LoadError> {
        let mut set = FilterSet::new(false);
        let mut html_rules = Vec::new();
        let mut html_exceptions = Vec::new();
        let mut skipped_html_rules = 0;
        for path in &paths {
            let text = fs::read_to_string(path).map_err(LoadError::io(path))?;
            for line in text.lines().filter(|line| !line.starts_with('!')) {
                if let Some((domains, selector)) = line.split_once("#@#^") {
                    html_exceptions.push((Domains::parse(domains), selector.to_owned()));
                } else if let Some((domains, selector)) = line.split_once("##^") {
                    match parse_html_rule(domains, selector) {
                        Some(rule) => html_rules.push((selector.to_owned(), rule)),
                        None => skipped_html_rules += 1,
                    }
                }
            }
            set.add_filter_list(text, ParseOptions::default());
        }
        eprintln!(
            "httrack-adblock: loaded {} HTML filters, skipped {skipped_html_rules} that the plugin cannot apply",
            html_rules.len()
        );
        // An exception without domains cancels its rule everywhere.
        let html_rules = html_rules
            .into_iter()
            .filter_map(|(selector, mut rule)| {
                for (domains, _) in html_exceptions.iter().filter(|(_, s)| *s == selector) {
                    if domains.is_empty() {
                        return None;
                    }
                    rule.exceptions.push(domains.clone());
                }
                Some(rule)
            })
            .collect();
        Ok(Self {
            paths,
            engine: Engine::new_with_filter_set(set),
            html_rules,
        })
    }

    /// Whether a rule blocks `url` as any of `request_types`, or as the type of its extension when
    /// `request_types` allows `Other`.
    pub(crate) fn blocks(&self, url: &Url, source: &Url, request_types: &[RequestType]) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        // For an `http` or `https` URL, `Request::new` uses the type only to set `request_type`.
        let Ok(mut request) = Request::new(url.as_str(), source.as_str(), "other", "GET") else {
            return false;
        };
        with_path_type(request_types, url.path()).any(|request_type| {
            request.request_type = request_type;
            self.engine.check_network_request(&request).should_block()
        })
    }

    /// `link` is relative to `base`, or to `page` when there is no base.
    pub(crate) fn blocks_link(
        &self,
        link: &str,
        base: Option<&Url>,
        page: Option<&Url>,
        request_types: &[RequestType],
    ) -> bool {
        let absolute = match base.or(page) {
            Some(base) => base.join(link),
            None => Url::parse(link),
        };
        let Ok(absolute) = absolute else { return false };
        self.blocks(&absolute, page.unwrap_or(&absolute), request_types)
    }
}

/// Adds `path`, or every `.txt` file in the folder `path` and its subfolders. The walk
/// skips symbolic links to folders, which can point back up the tree.
pub(crate) fn add_list_files(path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), LoadError> {
    if !path.is_dir() {
        files.insert(fs::canonicalize(path).map_err(LoadError::io(path))?);
        return Ok(());
    }
    for entry in fs::read_dir(path).map_err(LoadError::io(path))? {
        let entry = entry.map_err(LoadError::io(path))?;
        let child = entry.path();
        if entry.file_type().map_err(LoadError::io(&child))?.is_dir() {
            add_list_files(&child, files)?;
        } else if child.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("txt")) && child.is_file() {
            files.insert(fs::canonicalize(&child).map_err(LoadError::io(&child))?);
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum LoadError {
    NoLists,
    Io { path: PathBuf, source: io::Error },
}

impl LoadError {
    fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLists => f.write_str("no filter lists given after the plugin path"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::PAGE_LINK_TYPES;

    fn filters(list: &str) -> Filters {
        Filters {
            paths: BTreeSet::new(),
            engine: Engine::new_with_list_text(list),
            html_rules: Vec::new(),
        }
    }

    #[test]
    fn blocks_reads_the_extension_from_the_path() {
        let filters = filters("||example.mov^$media");
        let url = |s| Url::parse(s).unwrap();
        let video = url("https://example.mov/v.MP4?x=1");
        assert!(filters.blocks(&video, &video, PAGE_LINK_TYPES));
        let host = url("https://example.mov");
        assert!(!filters.blocks(&host, &host, PAGE_LINK_TYPES));
    }

    #[test]
    fn blocks_link_reads_the_extension_from_the_resolved_link() {
        let filters = filters("||site.test/x.js$script");
        let page = Url::parse("http://site.test/dir/page.html").unwrap();
        let blocks = |link, types| filters.blocks_link(link, None, Some(&page), types);
        assert!(blocks(" /x.js ", &[RequestType::Other]));
        assert!(blocks("/x.j\ns", &[RequestType::Other]));
        assert!(blocks("/x.js\t", &[RequestType::Other]));
        assert!(!blocks("/x.js", &[RequestType::Image]));
    }
}

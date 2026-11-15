//! The raw header block that HTTrack passes to `receive-header`.

/// Copy of `HTTP_IS_REDIRECT` in HTTrack's `htslib.h`.
const REDIRECT_STATUS_CODES: &[&[u8]] = &[b"301", b"302", b"303", b"307", b"308"];

pub(crate) struct Response<'a> {
    status: &'a [u8],
    headers: Vec<&'a [u8]>,
}

impl<'a> Response<'a> {
    pub(crate) fn parse(block: &'a [u8]) -> Option<Self> {
        let mut lines = block.split(|&b| b == b'\n').map(<[u8]>::trim_ascii);
        // HTTrack's `htsback.c` skips one empty line before the status line.
        let mut status_line = lines.next()?;
        if status_line.is_empty() {
            status_line = lines.next()?;
        }
        let status = status_line.split(|&b| b == b' ').nth(1)?;
        Some(Self {
            status,
            headers: lines.collect(),
        })
    }

    pub(crate) fn is_redirect(&self) -> bool {
        REDIRECT_STATUS_CODES.contains(&self.status)
    }

    pub(crate) fn is_success(&self) -> bool {
        self.status.starts_with(b"2")
    }

    /// The value of the first header named `name`, in any case.
    pub(crate) fn header(&self, name: &[u8]) -> Option<&'a [u8]> {
        self.headers.iter().find_map(|line| {
            let (header, value) = line.split_at(line.iter().position(|&b| b == b':')?);
            header
                .trim_ascii()
                .eq_ignore_ascii_case(name)
                .then(|| value[1..].trim_ascii())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_one_empty_line_before_the_status_line() {
        let response = Response::parse(b"\r\nHTTP/1.1 302 Found\r\nLocation: /next\r\n\r\n").unwrap();
        assert!(response.is_redirect());
        assert_eq!(response.header(b"location"), Some(&b"/next"[..]));
    }

    #[test]
    fn header_ignores_case_and_keeps_colons_in_the_value() {
        let response = Response::parse(b"HTTP/1.1 200 OK\nCONTENT-TYPE : text/html; a=b:c\n").unwrap();
        assert!(response.is_success());
        assert!(!response.is_redirect());
        assert_eq!(response.header(b"content-type"), Some(&b"text/html; a=b:c"[..]));
        assert_eq!(response.header(b"location"), None);
    }

    #[test]
    fn parse_needs_a_status_code() {
        assert!(Response::parse(b"").is_none());
        assert!(Response::parse(b"\n").is_none());
        assert!(Response::parse(b"HTTP/1.1").is_none());
    }
}

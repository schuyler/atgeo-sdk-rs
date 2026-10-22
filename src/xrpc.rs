//! XRPC URLs (contract §1) and the URL checks every request passes before a transport sees it.

/// Percent-encodes everything but the RFC 3986 unreserved characters, so `+` goes as `%2B` and a space
/// as `%20`.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `{server}/xrpc/{nsid}?{params}`, keeping the reference's trailing `?` when there are no parameters.
pub(crate) fn url(server: &str, nsid: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect();
    format!("{server}/xrpc/{nsid}?{}", query.join("&"))
}

/// The server with trailing slashes removed.
pub(crate) fn strip_server(server: &str) -> String {
    server.trim_end_matches('/').to_owned()
}

/// Prepares a URL the way WHATWG URL parsing treats these characters: leading and trailing C0 controls
/// and spaces trimmed, tab, LF and CR dropped, and space, other controls, non-ASCII and `" < > ` { }`
/// percent-encoded, with `%XX` left alone. A clean URL is unchanged. (In a query WHATWG leaves `` ` ``, `{`
/// and `}` raw, which this encodes, and encodes `'`, which this leaves raw; either way the server decodes
/// the same value.)
pub(crate) fn prepare_url(url: &str) -> String {
    let url = url.trim_matches(|c: char| c <= ' ');
    let mut out = String::with_capacity(url.len());
    for &b in url.as_bytes() {
        match b {
            b'\t' | b'\n' | b'\r' => {}
            b'"' | b'<' | b'>' | b'`' | b'{' | b'}' => out.push_str(&format!("%{b:02X}")),
            0x21..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// An absolute `http` or `https` URL (scheme in any case) with a non-empty host and no credentials.
///
/// Credentials are refused, as the Fetch standard's `Request` does in the reference (contract §4.4): reqwest
/// would send them as `Authorization: Basic` to whatever host the URL names.
pub(crate) fn is_http_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    // Any `@` before the first `/`, `?` or `#` is refused, scanning past a backslash: an empty userinfo
    // (`https://@h/x`), which `fetch` drops, and `https://h\\@x/y`, which WHATWG reads as host `h` but a
    // parser that does not end the authority at a backslash (an app's transport, say) reads as userinfo
    // `h\\` at host `x`. Both are margins of contract §4.4; CONFORMANCE.md.
    if rest.split(['/', '?', '#']).next().unwrap_or("").contains('@') {
        return false;
    }
    // WHATWG ends a special URL's authority at a backslash too: `https://h\\x/y` is host `h`, path `/x/y`.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    !host.is_empty()
}

/// The URL with any userinfo removed from its authority, for messages.
#[cfg(feature = "ureq")]
pub(crate) fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// Resolves a redirect's `Location` against the URL that answered with it, removing dot segments
/// (RFC 3986 §5.2), as reqwest and the reference resolve it.
#[cfg(feature = "ureq")]
pub(crate) fn resolve(base: &str, location: &str) -> String {
    let location = location.trim();
    let has_scheme = location.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && location
            .split_once(':')
            .is_some_and(|(s, _)| s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b)));
    let (scheme, rest) = base.split_once("://").unwrap_or(("http", base));
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = &base[..scheme.len() + 3 + authority_end];
    let path_and_query = &rest[authority_end..];
    let path = &path_and_query[..path_and_query.find(['?', '#']).unwrap_or(path_and_query.len())];
    let resolved = if has_scheme {
        location.to_owned()
    } else if let Some(net) = location.strip_prefix("//") {
        format!("{scheme}://{net}")
    } else if location.starts_with('/') {
        format!("{origin}{location}")
    } else if location.starts_with('?') {
        format!("{origin}{path}{location}")
    } else if location.starts_with('#') || location.is_empty() {
        let without_fragment = &base[..base.find('#').unwrap_or(base.len())];
        format!("{without_fragment}{location}")
    } else {
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        let dir = if dir.is_empty() { "/" } else { dir };
        format!("{origin}{dir}{location}")
    };
    without_dot_segments(&resolved)
}

/// The URL with `.` and `..` segments removed from its path.
#[cfg(feature = "ureq")]
fn without_dot_segments(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let Some(path_start) = rest.find('/') else { return url.to_owned() };
    let after_path = rest.find(['?', '#']).unwrap_or(rest.len());
    if after_path < path_start {
        return url.to_owned();
    }
    let path = &rest[path_start..after_path];
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut out: Vec<&str> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let last = i == segments.len() - 1;
        match *segment {
            "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
        // A path ending in a dot segment ends in a slash.
        if last && matches!(*segment, "." | "..") {
            out.push("");
        }
    }
    format!("{scheme}://{}/{}{}", &rest[..path_start], out.join("/"), &rest[after_path..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b+c&d=e#f%g,h:i"), "a%20b%2Bc%26d%3De%23f%25g%2Ch%3Ai");
        assert_eq!(encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(
            url(
                "https://s",
                "org.atgeo.getTiles",
                &[("dataset", "overturemaps.org"), ("bbox", "-1.00,2.00,3.00,4.00")]
            ),
            "https://s/xrpc/org.atgeo.getTiles?dataset=overturemaps.org&bbox=-1.00%2C2.00%2C3.00%2C4.00"
        );
        assert_eq!(url("https://s", "org.atgeo.describeService", &[]), "https://s/xrpc/org.atgeo.describeService?");
    }

    #[test]
    fn strips_trailing_slashes() {
        assert_eq!(strip_server("http://localhost:8000///"), "http://localhost:8000");
    }

    #[test]
    fn prepares_urls() {
        assert_eq!(prepare_url("https://h/a b.json"), "https://h/a%20b.json");
        assert_eq!(prepare_url("https://h/café"), "https://h/caf%C3%A9");
        assert_eq!(prepare_url("https://h/a\tb\nc\rd"), "https://h/abcd");
        assert_eq!(prepare_url("  https://h/x \u{1}"), "https://h/x");
        assert_eq!(prepare_url("https://h/a\u{7F}%20"), "https://h/a%7F%20");
        assert_eq!(prepare_url("https://h/a<b>{c}\"d`e"), "https://h/a%3Cb%3E%7Bc%7D%22d%60e");
        let clean = "https://places.atgeo.org/tiles/a.json?x=1%2C2#f";
        assert_eq!(prepare_url(clean), clean);
    }

    #[test]
    fn recognizes_http_urls() {
        for ok in
            ["https://h", "HTTP://h/x", "http://h:80/x@y", "http://[::1]:8000/x", "https://h?q", "https://h\\x/y@z"]
        {
            assert!(is_http_url(ok), "{ok}");
        }
        for bad in
            ["", "h/x", "/x", "ftp://h/x", "file:///etc/passwd", "https:///x", "https://", "http://u@/x", "https:h"]
                .into_iter()
                .chain(["http://u:p@h:80/x", "https://u@h/x", "https://:p@h", "https://@h/x", "https://u:p@[::1]/x"])
                .chain([
                    "https://h\\@x/y",
                    "https://h\\%FF@x/y",
                    "http://h\\a\\@x",
                    "https://h\\@x?q",
                    "https://h\\u:p@x#f",
                ])
        {
            assert!(!is_http_url(bad), "{bad}");
        }
    }

    #[cfg(feature = "ureq")]
    #[test]
    fn removes_credentials_for_messages() {
        assert_eq!(without_credentials("https://u:pw@h:8/a@b?c"), "https://h:8/a@b?c");
        assert_eq!(without_credentials("file:///etc/passwd"), "file:///etc/passwd");
        assert_eq!(without_credentials("https://h/x"), "https://h/x");
    }

    #[cfg(feature = "ureq")]
    #[test]
    fn resolves_redirect_locations() {
        let base = "https://h:8/a/b/c.json?x=1#f";
        assert_eq!(resolve(base, "http://o/x"), "http://o/x");
        assert_eq!(resolve(base, "file:///etc"), "file:///etc");
        assert_eq!(resolve(base, "//o/x"), "https://o/x");
        assert_eq!(resolve(base, "/x?y"), "https://h:8/x?y");
        assert_eq!(resolve(base, "d.json"), "https://h:8/a/b/d.json");
        assert_eq!(resolve(base, "?z"), "https://h:8/a/b/c.json?z");
        assert_eq!(resolve("https://h", "d"), "https://h/d");
        assert_eq!(resolve("https://h?q", "/p"), "https://h/p");
        assert_eq!(resolve(base, "sub/../ok?x=1"), "https://h:8/a/b/ok?x=1");
        assert_eq!(resolve(base, "../../../../x"), "https://h:8/x");
        assert_eq!(resolve(base, "./"), "https://h:8/a/b/");
        assert_eq!(resolve(base, ".."), "https://h:8/a/");
        assert_eq!(resolve(base, "/p/./q/../r#f"), "https://h:8/p/r#f");
        assert_eq!(resolve(base, "http://o/a/../b?c=../d"), "http://o/b?c=../d");
        assert_eq!(resolve(base, "http://o"), "http://o");
    }
}

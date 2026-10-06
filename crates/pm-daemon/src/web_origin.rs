//! Whether a request carrying the session cookie came from the dashboard
//! itself.
//!
//! `SameSite` is scheme plus registrable domain and excludes the port, so
//! another listener on the same host is same-site with the dashboard, as is
//! every subdomain under a share domain. The browser hands such a page the
//! session cookie, and the page can spend it two ways.
//!
//! A WebSocket upgrade has neither a preflight nor a response gate to hide
//! behind: the handshake completes and the cookie rides it before any
//! application code runs. For ordinary requests CORS hides the response, but
//! it does not stop the effect of one that needs no preflight, and a mutating
//! handler that does not require `application/json` accepts a cross-origin
//! POST. Either way the handshake's or request's own `Origin` is what
//! separates the dashboard's client from the neighbour.
//!
//! The two entry points differ in one respect, deliberately: what a stated
//! origin's absence means. See [`mutation_is_own_origin`].

use axum::http::{header, HeaderMap};

/// Set by browsers that implement fetch metadata. Any value other than this
/// one names a document that is not the dashboard.
const SEC_FETCH_SITE: &str = "sec-fetch-site";
const SAME_ORIGIN_SITE: &str = "same-origin";

/// Ports a URL leaves implicit, so an `Origin` that omits one compares equal
/// to a `Host` that spells it out.
const IMPLICIT_PORTS: [&str; 2] = ["80", "443"];

/// Whether a WebSocket upgrade may proceed to authentication.
///
/// A client that states no origin is allowed here. Browsers always send one
/// on an upgrade, so absence names a native client, and the phone's socket
/// is one: it reaches the controller by a configured address and may still
/// authenticate with the session cookie rather than a device token.
pub fn handshake_is_own_origin(headers: &HeaderMap, public_url: Option<&str>) -> bool {
    match stated_origin(headers) {
        Some(origin) => origin_is_own(origin, headers, public_url),
        None => fetch_metadata_agrees(headers),
    }
}

/// Whether a state-changing request carrying the session cookie may proceed.
///
/// Absence is refused here, which is the one place this differs from an
/// upgrade. A browser sends `Origin` on every request whose method is not GET
/// or HEAD, so absence still names a client that is not a page — but nothing
/// legitimate reaches this surface that way while relying on the cookie. The
/// CLI and the TUI use the unix socket or a bearer token, workers use the
/// worker plane, and the phone and the MCP surface both authenticate with a
/// bearer token. So
/// allowing absence buys no client anything, and refusing it keeps the
/// control from resting on a browser always getting that header right.
pub fn mutation_is_own_origin(headers: &HeaderMap, public_url: Option<&str>) -> bool {
    match stated_origin(headers) {
        Some(origin) => origin_is_own(origin, headers, public_url),
        None => false,
    }
}

fn stated_origin(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, header::ORIGIN.as_str())
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
}

/// Fetch metadata is a second, independent statement of the same fact, and a
/// browser that sends it cannot be talked out of it. Absent, the `Origin`
/// comparison answers alone.
fn fetch_metadata_agrees(headers: &HeaderMap) -> bool {
    header_str(headers, SEC_FETCH_SITE)
        .is_none_or(|site| site.trim().eq_ignore_ascii_case(SAME_ORIGIN_SITE))
}

fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name)?.to_str().ok()
}

/// The allowed origins are the one the request was addressed to and the
/// canonical base URL the operator configured. A page can set neither: the
/// browser fills `Origin` from the document and `Host` from the URL being
/// opened, so they agree only when the page is already the dashboard.
///
/// Scheme is deliberately not compared. A reverse proxy that terminates TLS
/// forwards a plaintext request whose `Host` matches an `https` `Origin`, and
/// that is the deployment the documentation recommends. Host and port are
/// what separate the dashboard from a same-site neighbour.
fn origin_is_own(origin: &str, headers: &HeaderMap, public_url: Option<&str>) -> bool {
    if !fetch_metadata_agrees(headers) {
        return false;
    }
    let Some(origin) = origin_authority(origin) else {
        return false;
    };
    [header_str(headers, header::HOST.as_str()), public_url]
        .into_iter()
        .flatten()
        .filter_map(url_authority)
        .any(|allowed| allowed == origin)
}

/// The `host[:port]` an `Origin` names, or `None` when the value is not an
/// http(s) origin. `Origin: null`, which a sandboxed document sends, has no
/// authority and so matches nothing.
fn origin_authority(origin: &str) -> Option<String> {
    let (scheme, rest) = origin.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    if rest.contains(['/', '?', '#']) {
        return None;
    }
    normalize_authority(rest)
}

/// The same for a `Host` header or a configured base URL, either of which
/// may arrive with or without a scheme and a path.
fn url_authority(value: &str) -> Option<String> {
    let rest = value.split_once("://").map_or(value, |(_, rest)| rest);
    normalize_authority(rest.split(['/', '?', '#']).next()?)
}

fn normalize_authority(authority: &str) -> Option<String> {
    let authority = authority.trim();
    let (host, port) = match authority.strip_prefix('[') {
        // A bracketed IPv6 literal holds colons that are not a port
        // separator.
        Some(rest) => {
            let end = rest.find(']')?;
            (&authority[..end + 2], rest[end + 1..].strip_prefix(':'))
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return None;
    }
    let host = host.to_ascii_lowercase();
    match port.filter(|p| !p.is_empty() && !IMPLICIT_PORTS.contains(p)) {
        Some(port) if !port.bytes().all(|b| b.is_ascii_digit()) => None,
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host),
    }
}

#[cfg(test)]
mod tests {
    use super::{handshake_is_own_origin, mutation_is_own_origin};
    use axum::http::{HeaderMap, HeaderValue};

    /// The headers a browser sends, spelled out so each test names only what
    /// it varies. `None` means the header is absent.
    fn headers(origin: Option<&str>, site: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in [("origin", origin), ("sec-fetch-site", site), ("host", host)] {
            if let Some(value) = value {
                map.insert(name, HeaderValue::from_str(value).unwrap());
            }
        }
        map
    }

    /// The shape the exploit takes: a page on another port of the same host,
    /// or a share subdomain, both of which `SameSite=Lax` hands the cookie.
    #[test]
    fn a_same_site_neighbour_is_refused_on_both_surfaces() {
        for origin in ["http://127.0.0.1:3999", "https://f42.share.example"] {
            let h = headers(Some(origin), None, Some("127.0.0.1:7676"));
            assert!(!handshake_is_own_origin(&h, None), "{origin} upgraded");
            assert!(!mutation_is_own_origin(&h, None), "{origin} mutated");
        }
    }

    #[test]
    fn the_dashboards_own_origin_is_allowed_on_both_surfaces() {
        let h = headers(
            Some("http://127.0.0.1:7676"),
            Some("same-origin"),
            Some("127.0.0.1:7676"),
        );
        assert!(handshake_is_own_origin(&h, None));
        assert!(mutation_is_own_origin(&h, None));
    }

    /// The one deliberate asymmetry. The phone's socket may state no origin
    /// and still authenticate with the cookie, so an upgrade allows it. No
    /// client mutates over HTTP on the cookie without being a page, so a
    /// mutation refuses it rather than resting on browsers always sending it.
    #[test]
    fn a_stated_origins_absence_upgrades_but_does_not_mutate() {
        for h in [
            headers(None, None, Some("phone.tailnet.ts.net")),
            headers(Some("   "), None, Some("127.0.0.1:7676")),
        ] {
            assert!(handshake_is_own_origin(&h, None));
            assert!(!mutation_is_own_origin(&h, None));
        }
    }

    /// A sandboxed document's origin is opaque and serializes to `null`.
    #[test]
    fn an_opaque_origin_is_refused_on_both_surfaces() {
        let h = headers(Some("null"), None, Some("127.0.0.1:7676"));
        assert!(!handshake_is_own_origin(&h, None));
        assert!(!mutation_is_own_origin(&h, None));
    }

    #[test]
    fn fetch_metadata_refuses_a_cross_site_request_on_its_own() {
        for site in ["cross-site", "same-site", "none"] {
            let h = headers(
                Some("http://127.0.0.1:7676"),
                Some(site),
                Some("127.0.0.1:7676"),
            );
            assert!(!handshake_is_own_origin(&h, None), "{site} upgraded");
            assert!(!mutation_is_own_origin(&h, None), "{site} mutated");
            // And with no Origin to compare, so the two signals are
            // independent rather than one gating the other.
            let bare = headers(None, Some(site), Some("127.0.0.1:7676"));
            assert!(
                !handshake_is_own_origin(&bare, None),
                "{site} upgraded bare"
            );
        }
    }

    #[test]
    fn the_configured_public_url_is_allowed_beside_the_request_host() {
        let public = Some("https://pm.example.com/");
        // A proxy that rewrites Host still has to name the public URL, and
        // reaching the daemon directly on loopback still has to work.
        assert!(mutation_is_own_origin(
            &headers(Some("https://pm.example.com"), None, Some("127.0.0.1:7676")),
            public,
        ));
        assert!(mutation_is_own_origin(
            &headers(Some("http://127.0.0.1:7676"), None, Some("127.0.0.1:7676")),
            public,
        ));
        assert!(!mutation_is_own_origin(
            &headers(
                Some("https://other.example.com"),
                None,
                Some("127.0.0.1:7676")
            ),
            public,
        ));
    }

    /// TLS terminated in front leaves the daemon serving plaintext under an
    /// `https` origin, so the schemes legitimately disagree.
    #[test]
    fn a_tls_terminating_proxy_still_matches() {
        assert!(mutation_is_own_origin(
            &headers(
                Some("https://pm.example.com"),
                Some("same-origin"),
                Some("pm.example.com"),
            ),
            None,
        ));
    }

    #[test]
    fn an_implicit_port_matches_the_one_spelled_out() {
        assert!(mutation_is_own_origin(
            &headers(
                Some("https://pm.example.com"),
                None,
                Some("pm.example.com:443")
            ),
            None,
        ));
        assert!(!mutation_is_own_origin(
            &headers(
                Some("https://pm.example.com:8443"),
                None,
                Some("pm.example.com")
            ),
            None,
        ));
    }

    #[test]
    fn a_bracketed_v6_literal_keeps_its_port_separate() {
        assert!(mutation_is_own_origin(
            &headers(Some("http://[::1]:7676"), None, Some("[::1]:7676")),
            None,
        ));
        assert!(!mutation_is_own_origin(
            &headers(Some("http://[::1]:3999"), None, Some("[::1]:7676")),
            None,
        ));
    }

    #[test]
    fn host_comparison_ignores_case() {
        assert!(mutation_is_own_origin(
            &headers(Some("https://PM.Example.COM"), None, Some("pm.example.com")),
            None,
        ));
    }

    /// A value that does not parse as an http(s) origin is never a match, so
    /// no unexpected spelling becomes an accidental allowance.
    #[test]
    fn a_malformed_or_non_http_origin_is_refused() {
        for origin in [
            "pm.example.com",
            "file://",
            "chrome-extension://abcdef",
            "http://",
            "http://pm.example.com/evil",
            "http://pm.example.com:notaport",
        ] {
            let h = headers(Some(origin), None, Some("pm.example.com"));
            assert!(!handshake_is_own_origin(&h, None), "{origin} upgraded");
            assert!(!mutation_is_own_origin(&h, None), "{origin} mutated");
        }
    }

    /// Without a `Host` and without a configured URL there is nothing to
    /// compare against, so a stated origin cannot be accepted.
    #[test]
    fn a_stated_origin_with_nothing_to_match_is_refused() {
        let h = headers(Some("http://127.0.0.1:7676"), None, None);
        assert!(!handshake_is_own_origin(&h, None));
        assert!(!mutation_is_own_origin(&h, None));
    }
}

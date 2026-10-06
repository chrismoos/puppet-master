//! HTTP-aware auth proxy for published forwards. Each accepted
//! connection reads the first HTTP request's headers, authenticates
//! the caller against the dashboard session cookie, a bearer token,
//! or a scoped forward token, and only then proceeds with the
//! existing TCP splice. Non-HTTP forwards (scheme != http/https)
//! bypass auth with a logged warning.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tracing::debug;

use crate::auth::SESSION_COOKIE;
use crate::daemon::Daemon;
use crate::forward::FORWARD_TOKEN_TTL;

/// Query parameter carrying a scoped forward token.
const FORWARD_TOKEN_PARAM: &str = "fwd_token";

/// Cookie the proxy sets from a valid query token, so the rest of the
/// page's requests carry the credential the address bar cannot.
const FORWARD_COOKIE: &str = "pm_fwd";

/// Upper bound on the bytes we buffer looking for the end of the
/// first HTTP request's headers. Anything larger is not a plausible
/// browser request and gets rejected.
const MAX_HEADER_BYTES: usize = 16_384;

/// A forward transport with an optional prefix replayed before reading the stream.
pub(crate) trait ForwardIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ForwardIo for T {}

pub struct ForwardStream {
    prefix: Vec<u8>,
    offset: usize,
    pub(crate) tcp: Box<dyn ForwardIo>,
}

impl ForwardStream {
    pub(crate) fn new(prefix: Vec<u8>, tcp: TcpStream) -> Self {
        Self {
            prefix,
            offset: 0,
            tcp: Box::new(tcp),
        }
    }

    pub(crate) fn duplex(stream: tokio::io::DuplexStream) -> Self {
        Self {
            prefix: Vec::new(),
            offset: 0,
            tcp: Box::new(stream),
        }
    }

    pub(crate) fn plain(tcp: TcpStream) -> Self {
        Self::new(Vec::new(), tcp)
    }
}

impl AsyncRead for ForwardStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.offset < this.prefix.len() {
            let remaining = &this.prefix[this.offset..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            this.offset += n;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut this.tcp).poll_read(cx, buf)
        }
    }
}

impl AsyncWrite for ForwardStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().tcp).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tcp).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tcp).poll_shutdown(cx)
    }
}

/// Outcome of authenticating a forward connection.
#[allow(dead_code)]
pub(crate) enum ForwardAuthResult {
    /// The caller is authenticated; carry the buffered header bytes
    /// so they can be replayed to the backend.
    Authenticated {
        header_bytes: Vec<u8>,
        username: String,
    },
    /// The caller is not authenticated; a rejection response has
    /// already been written to the TCP stream.
    Rejected,
    /// The connection closed or produced garbage before we could
    /// read enough to decide.
    Error(io::Error),
    /// A complete response has been written and the connection is
    /// finished. Used for the redirect that moves a query token into a
    /// cookie.
    Handled,
}

/// Reads the HTTP request line and headers from `tcp`, checks the
/// caller's identity against the daemon's auth stores, and either
/// returns the buffered bytes (for replay into the splice) or writes
/// a rejection response and returns `Rejected`.
pub(crate) async fn authenticate_forward(
    tcp: &mut TcpStream,
    daemon: &Arc<Daemon>,
    forward_id: u64,
) -> ForwardAuthResult {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    let header_end;

    // Read until we see the end-of-headers marker.
    loop {
        let n = match tcp.read(&mut tmp).await {
            Ok(0) => {
                return ForwardAuthResult::Error(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "closed before headers",
                ))
            }
            Ok(n) => n,
            Err(e) => return ForwardAuthResult::Error(e),
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > MAX_HEADER_BYTES {
            let _ = write_response(tcp, 431, "Request Header Fields Too Large").await;
            return ForwardAuthResult::Rejected;
        }
    }

    let headers = &buf[..header_end];
    let header_str = String::from_utf8_lossy(headers);

    // Try cookie auth first.
    if let Some(token) = extract_cookie(&header_str, SESSION_COOKIE) {
        if let Some((_, username)) = daemon.auth_verify_user(&token) {
            debug!(forward = forward_id, user = %username, "forward authed via cookie");
            return ForwardAuthResult::Authenticated {
                header_bytes: buf,
                username,
            };
        }
    }

    // Bearer token (mobile access token).
    if let Some(token) = extract_bearer(&header_str) {
        if let Some((_, username, _)) = daemon.access_token_verify(&token) {
            debug!(forward = forward_id, user = %username, "forward authed via bearer");
            return ForwardAuthResult::Authenticated {
                header_bytes: buf,
                username,
            };
        }
    }

    // A forward token this proxy previously moved into a cookie, which
    // is what every request after the first one carries.
    let cookie_token = extract_cookie(&header_str, FORWARD_COOKIE);
    if let Some(token) = &cookie_token {
        if let Some(username) = daemon.verify_forward_token(token, forward_id) {
            debug!(forward = forward_id, user = %username, "forward authed via forward cookie");
            return ForwardAuthResult::Authenticated {
                header_bytes: buf,
                username,
            };
        }
    }

    // Scoped forward token in the query string.
    let query_token = extract_query_token(&header_str);
    if let Some(token) = &query_token {
        if let Some(username) = daemon.verify_forward_token(token, forward_id) {
            debug!(forward = forward_id, user = %username, "forward authed via forward token");
            // A navigation gets the token moved into a cookie, so the
            // page's own requests authenticate and the address bar is
            // left without a credential in it. Anything else is passed
            // straight through: a redirect would lose its method.
            if is_navigation(&header_str) {
                let uri = request_uri(&header_str).unwrap_or("/");
                let _ = write_token_cookie_redirect(tcp, uri, token).await;
                return ForwardAuthResult::Handled;
            }
            return ForwardAuthResult::Authenticated {
                header_bytes: buf,
                username,
            };
        }
    }

    // A forward credential that was offered and did not verify has
    // expired or belongs to another forward. This listener sends no
    // handoff of its own, so it says so rather than redirecting the
    // reader somewhere that cannot mint them a new one.
    if cookie_token.is_some() || query_token.is_some() {
        debug!(forward = forward_id, "forward token rejected as expired");
        let _ = write_expired_token_page(tcp, &header_str).await;
        return ForwardAuthResult::Rejected;
    }

    debug!(forward = forward_id, "forward auth rejected");
    let _ = write_auth_rejection(tcp, &header_str, daemon).await;
    ForwardAuthResult::Rejected
}

/// The request target from the request line.
fn request_uri(headers: &str) -> Option<&str> {
    headers.lines().next()?.split_whitespace().nth(1)
}

/// Whether this is a browser opening a page, which is the only case the
/// cookie handoff helps and the only one that can follow a redirect
/// without losing something. A POST keeps its method, and a client that
/// did not ask for HTML — curl, an API caller — keeps using the token in
/// the query string.
fn is_navigation(headers: &str) -> bool {
    let Some(line) = headers.lines().next() else {
        return false;
    };
    if !line
        .split_whitespace()
        .next()
        .is_some_and(|method| method.eq_ignore_ascii_case("GET"))
    {
        return false;
    }
    headers.lines().any(|l| {
        let lower = l.to_ascii_lowercase();
        lower.starts_with("accept:") && lower.contains("text/html")
    })
}

/// The request target with the forward token stripped, preserving any
/// other query parameters.
fn uri_without_token(uri: &str) -> String {
    let Some((path, query)) = uri.split_once('?') else {
        return uri.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|param| {
            param
                .split_once('=')
                .map(|(key, _)| key != FORWARD_TOKEN_PARAM)
                .unwrap_or(true)
        })
        .filter(|param| !param.is_empty())
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// Moves a verified query token into a cookie and sends the reader back
/// to the same page without it.
async fn write_token_cookie_redirect(
    tcp: &mut TcpStream,
    uri: &str,
    token: &str,
) -> io::Result<()> {
    let target = uri_without_token(uri);
    let max_age = FORWARD_TOKEN_TTL.as_secs();
    let body = "<html><body>Opening…</body></html>";
    let resp = format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {target}\r\n\
         Set-Cookie: {FORWARD_COOKIE}={token}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax\r\n\
         Cache-Control: no-store\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    tcp.write_all(resp.as_bytes()).await
}

/// What a reader sees when their forward link has aged out. It names the
/// one action that fixes it, and clears the stale cookie so a reload
/// does not present it again.
async fn write_expired_token_page(tcp: &mut TcpStream, header_str: &str) -> io::Result<()> {
    let clear = format!("Set-Cookie: {FORWARD_COOKIE}=; Path=/; Max-Age=0\r\n");
    let accepts_html = header_str.lines().any(|l| {
        l.to_ascii_lowercase().starts_with("accept:")
            && l.to_ascii_lowercase().contains("text/html")
    });
    if !accepts_html {
        let body = "This forward link has expired. Reopen the URL from Puppet Master.";
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\n\
             {clear}\
             Cache-Control: no-store\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        );
        return tcp.write_all(resp.as_bytes()).await;
    }
    let body = EXPIRED_TOKEN_HTML;
    let resp = format!(
        "HTTP/1.1 401 Unauthorized\r\n\
         {clear}\
         Cache-Control: no-store\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    tcp.write_all(resp.as_bytes()).await
}

/// Served on an expired or mismatched forward token. Self-contained: a
/// forward that cannot be reached cannot serve assets either.
const EXPIRED_TOKEN_HTML: &str = concat!(
    "<!doctype html><html><head><meta charset=\"utf-8\">",
    "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
    "<title>Link expired</title><style>",
    "body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;",
    "background:#14161a;color:#e6e8eb;",
    "font:16px/1.55 ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif}",
    "main{max-width:26rem;padding:2rem;text-align:center}",
    "h1{margin:0 0 .6rem;font-size:1.25rem;font-weight:600}",
    "p{margin:0;color:#a2a8b0}",
    "</style></head><body><main>",
    "<h1>This link has expired</h1>",
    "<p>Reopen the URL from Puppet Master to get a fresh one.</p>",
    "</main></body></html>"
);

/// Writes a 302 redirect to the login page for browser requests, or
/// a 401 for XHR / API clients.
async fn write_auth_rejection(
    tcp: &mut TcpStream,
    header_str: &str,
    daemon: &Arc<Daemon>,
) -> io::Result<()> {
    let is_xhr = header_str.lines().any(|l| {
        l.trim()
            .eq_ignore_ascii_case("x-requested-with: xmlhttprequest")
    });
    let accepts_html = header_str.lines().any(|l| {
        l.to_ascii_lowercase().starts_with("accept:")
            && l.to_ascii_lowercase().contains("text/html")
    });

    if is_xhr || !accepts_html {
        write_response(tcp, 401, "Unauthorized — sign in to the dashboard first").await
    } else {
        let login_url = daemon.login_url();
        let body =
            format!("<html><body>Redirecting to <a href=\"{login_url}\">login</a>…</body></html>");
        let resp = format!(
            "HTTP/1.1 302 Found\r\n\
             Location: {login_url}\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        );
        tcp.write_all(resp.as_bytes()).await
    }
}

async fn write_response(tcp: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        401 => "Unauthorized",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    tcp.write_all(resp.as_bytes()).await
}

/// Finds the byte offset just past the `\r\n\r\n` header terminator.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Extracts a cookie value by name from the raw HTTP header text.
fn extract_cookie(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("cookie:") {
            let value_part = &line[7..]; // skip "cookie:"
            for piece in value_part.split(';') {
                let piece = piece.trim();
                if let Some((k, v)) = piece.split_once('=') {
                    if k.trim() == name {
                        return Some(v.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

/// Extracts a Bearer token from the Authorization header.
fn extract_bearer(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization:") {
            let value = line[14..].trim();
            if let Some(token) = value.strip_prefix("Bearer ") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Extracts the `fwd_token` query parameter from the request line.
fn extract_query_token(headers: &str) -> Option<String> {
    let request_line = headers.lines().next()?;
    let uri = request_line.split_whitespace().nth(1)?;
    let query = uri.split_once('?').map(|(_, q)| q)?;
    for param in query.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            if key == FORWARD_TOKEN_PARAM {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Returns true when the scheme indicates an HTTP-family protocol
/// that can be authenticated at the HTTP layer.
pub(crate) fn is_http_scheme(scheme: &str) -> bool {
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss" | ""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn error_responses_declare_utf8_and_preserve_unicode() {
        use std::net::Ipv4Addr;
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::time::timeout(RESPONSE_TIMEOUT, async {
            tokio::join!(TcpStream::connect(address), listener.accept())
        })
        .await
        .unwrap();
        let mut client = client.unwrap();
        let (mut server, _) = accepted.unwrap();
        let body = "Unauthorized — sign in to the dashboard first";
        tokio::time::timeout(
            RESPONSE_TIMEOUT,
            write_response(
                &mut server,
                axum::http::StatusCode::UNAUTHORIZED.as_u16(),
                body,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(server);

        let mut response = String::new();
        tokio::time::timeout(RESPONSE_TIMEOUT, client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        let (headers, received) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(headers.contains(&format!("Content-Length: {}", body.len())));
        assert_eq!(received, body);
    }

    #[test]
    fn cookie_extraction() {
        let headers = "GET / HTTP/1.1\r\n\
                        Host: example.com\r\n\
                        Cookie: foo=bar; pm_session=abc123; other=val\r\n\
                        \r\n";
        assert_eq!(extract_cookie(headers, "pm_session"), Some("abc123".into()));
        assert_eq!(extract_cookie(headers, "foo"), Some("bar".into()));
        assert_eq!(extract_cookie(headers, "missing"), None);
    }

    #[test]
    fn cookie_extraction_no_cookie_header() {
        let headers = "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        assert_eq!(extract_cookie(headers, "pm_session"), None);
    }

    #[test]
    fn bearer_extraction() {
        let headers = "GET / HTTP/1.1\r\n\
                        Authorization: Bearer mytoken123\r\n\
                        \r\n";
        assert_eq!(extract_bearer(headers), Some("mytoken123".into()));
    }

    #[test]
    fn bearer_extraction_missing() {
        let headers = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(extract_bearer(headers), None);
    }

    #[test]
    fn a_browser_page_open_is_a_navigation() {
        let headers = "GET /?fwd_token=t HTTP/1.1\r\nAccept: text/html,*/*\r\n\r\n";
        assert!(is_navigation(headers));
    }

    #[test]
    fn an_api_call_or_a_post_is_not_a_navigation() {
        // No Accept for HTML: curl and API clients keep the query token
        // rather than being bounced through a cookie they may not store.
        assert!(!is_navigation(
            "GET /?fwd_token=t HTTP/1.1\r\nAccept: */*\r\n\r\n"
        ));
        // A redirect would turn this into a GET and lose the body.
        assert!(!is_navigation(
            "POST /?fwd_token=t HTTP/1.1\r\nAccept: text/html\r\n\r\n"
        ));
    }

    #[test]
    fn the_redirect_target_drops_only_the_token() {
        assert_eq!(uri_without_token("/?fwd_token=abc"), "/");
        assert_eq!(uri_without_token("/app?fwd_token=abc"), "/app");
        assert_eq!(
            uri_without_token("/app?a=1&fwd_token=abc&b=2"),
            "/app?a=1&b=2"
        );
        assert_eq!(uri_without_token("/app?a=1"), "/app?a=1");
        assert_eq!(uri_without_token("/app"), "/app");
    }

    #[test]
    fn query_token_extraction() {
        let headers = "GET /path?fwd_token=tok123&other=val HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(extract_query_token(headers), Some("tok123".into()));
    }

    #[test]
    fn query_token_extraction_missing() {
        let headers = "GET /path?other=val HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(extract_query_token(headers), None);
    }

    #[test]
    fn query_token_no_query_string() {
        let headers = "GET /path HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(extract_query_token(headers), None);
    }

    #[test]
    fn header_end_detection() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(18));
        assert_eq!(find_header_end(b"partial\r\n"), None);
        assert_eq!(find_header_end(b"\r\n\r\n"), Some(4));
    }

    #[test]
    fn http_scheme_detection() {
        assert!(is_http_scheme("http"));
        assert!(is_http_scheme("https"));
        assert!(is_http_scheme("HTTP"));
        assert!(is_http_scheme("ws"));
        assert!(is_http_scheme("wss"));
        assert!(is_http_scheme(""));
        assert!(!is_http_scheme("tcp"));
        assert!(!is_http_scheme("postgres"));
    }
}

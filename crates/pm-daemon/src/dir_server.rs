//! Serves one published directory over loopback HTTP.
//!
//! A directory share is the agent handing over a path instead of
//! standing up its own server, so this runs on whichever host holds the
//! files: in the daemon for a local session, in `pm worker` for a remote
//! one. Both sides bind the same server, and the controller forwards to
//! it exactly as it forwards to a port an agent published itself.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;
use tracing::{debug, info};

/// Entries beyond this are dropped from a generated listing. A share
/// pointed at a directory of that size is a mistake worth seeing as a
/// truncated page rather than a multi-megabyte one.
const LISTING_ENTRY_LIMIT: usize = 2_000;

/// Read chunk for streaming a file body.
const FILE_CHUNK_BYTES: usize = 64 * 1024;

/// Bytes that open a PEM armour block, which is how most private keys and
/// certificates are stored whatever they are called.
///
/// This is the check that does not rot. A list of names refuses the names it
/// knows, so `server.key` was refused and `pm_shared_key` was served, and no
/// amount of adding names fixes the shape of that: the next secret will have a
/// name nobody thought of. Reading the first bytes asks what the file is.
const PEM_ARMOUR: &[u8] = b"-----BEGIN ";

/// How much of a file to read to answer that. The armour is the first thing in
/// the file, so this needs only to span it.
const SNIFF_BYTES: usize = 64;

/// Extensions for key material that carries no detectable header, because it is
/// a binary container rather than armoured text. Content sniffing cannot answer
/// for these, so they stay a list — a short one, of formats that exist only to
/// hold keys.
const DENIED_EXTENSIONS: &[&str] = &[
    "der", "jks", "kdbx", "keystore", "key", "p12", "p8", "pem", "pfx", "pk8", "ppk",
];

/// Names a secret is kept under by something, rather than names that merely
/// look secret.
///
/// The ssh entries are ssh's own defaults for a private key. The rest are this
/// project's, derived from the code that writes them rather than copied: a
/// drifting constant there would otherwise quietly stop being refused here.
fn denied_name(lower: &str) -> bool {
    const SSH_PRIVATE_KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "identity"];
    const CLOUD_CREDENTIALS: &[&str] = &[
        "credentials",
        "credentials.json",
        "service-account.json",
        "secrets.yaml",
        "secrets.yml",
    ];
    if SSH_PRIVATE_KEYS.contains(&lower) || CLOUD_CREDENTIALS.contains(&lower) {
        return true;
    }
    // This installation's own sealing secret, which sits beside the database
    // under whatever that database is called.
    if lower.ends_with(crate::secrets::SECRET_FILE_SUFFIX) {
        return true;
    }
    // A host's own key and the config naming its controller.
    lower.starts_with("worker-key") || lower == "worker.toml"
}

/// Why a path cannot become a directory share.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShareRootError {
    #[error("the session working directory is not readable: {0}")]
    UnusableSessionRoot(String),
    #[error("a shared directory must be inside the session working directory")]
    Outside,
    #[error("a shared path must not traverse symlinks")]
    Symlink,
    #[error("cannot read the shared path: {0}")]
    Unreadable(String),
    #[error("a shared path must be a directory")]
    NotADirectory,
    #[error(
        "that directory holds a git repository, so sharing it would publish the \
         whole tree including files that are not meant to leave the host. Share \
         the subdirectory that holds the artifacts instead."
    )]
    RepositoryRoot,
}

/// Resolves a share's directory below the session working directory,
/// applying the same confinement the worker's scoped file read applies:
/// a descendant only, and no symlink crossed on the way down.
pub fn resolve_share_root(session_cwd: &str, path: &str) -> Result<PathBuf, ShareRootError> {
    let root = std::fs::canonicalize(session_cwd)
        .map_err(|e| ShareRootError::UnusableSessionRoot(e.to_string()))?;
    if !root.is_dir() {
        return Err(ShareRootError::UnusableSessionRoot(
            "it is not a directory".into(),
        ));
    }
    let requested = Path::new(path);
    let relative = if requested.is_absolute() {
        requested
            .strip_prefix(&root)
            .or_else(|_| requested.strip_prefix(session_cwd))
            .map_err(|_| ShareRootError::Outside)?
    } else {
        requested
    };
    let resolved = descend(&root, relative).map_err(|e| match e {
        DescendError::Traversal => ShareRootError::Outside,
        DescendError::Symlink => ShareRootError::Symlink,
        DescendError::Denied | DescendError::Missing => {
            ShareRootError::Unreadable("no such directory".into())
        }
        DescendError::Io(error) => ShareRootError::Unreadable(error),
    })?;
    let canonical =
        std::fs::canonicalize(&resolved).map_err(|e| ShareRootError::Unreadable(e.to_string()))?;
    if !canonical.starts_with(&root) {
        return Err(ShareRootError::Outside);
    }
    if !canonical.is_dir() {
        return Err(ShareRootError::NotADirectory);
    }
    if canonical.join(".git").exists() {
        return Err(ShareRootError::RepositoryRoot);
    }
    Ok(canonical)
}

/// A running directory server and the loopback port it answers on.
pub struct DirShareServer {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl DirShareServer {
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for DirShareServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds a loopback server rooted at `root` on an ephemeral port.
///
/// It answers both HTTP/1.1 and HTTP/2 over cleartext on that one port,
/// chosen per connection from the preface the client sends. The
/// controller speaks h2c so every request for one share rides a single
/// connection, and nothing that already speaks HTTP/1.1 to a share has
/// to change.
pub async fn serve(root: PathBuf) -> io::Result<DirShareServer> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = listener.local_addr()?.port();
    let app = axum::Router::new()
        .fallback(handle)
        .with_state(std::sync::Arc::new(root.clone()));
    let task = tokio::spawn(async move {
        let builder = auto::Builder::new(TokioExecutor::new());
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                debug!("directory share server stopped");
                break;
            };
            let service = TowerToHyperService::new(app.clone());
            let builder = builder.clone();
            tokio::spawn(async move {
                if let Err(e) = builder.serve_connection(TokioIo::new(tcp), service).await {
                    debug!(error = %e, "a directory share connection ended");
                }
            });
        }
    });
    info!(port, root = %root.display(), "serving a shared directory");
    Ok(DirShareServer { port, task })
}

async fn handle(State(root): State<std::sync::Arc<PathBuf>>, request: Request) -> Response {
    if !matches!(request.method(), &Method::GET | &Method::HEAD) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "a shared directory is read-only",
        )
            .into_response();
    }
    let requested = percent_decode(request.uri().path().trim_start_matches('/'));
    let Ok(target) = descend(&root, Path::new(&requested)) else {
        return not_found();
    };
    let Ok(metadata) = std::fs::metadata(&target) else {
        return not_found();
    };
    if metadata.is_dir() {
        let index = target.join("index.html");
        if index.is_file() && !holds_key_material(&index) {
            return file_response(&index, request.method() == Method::HEAD).await;
        }
        // A trailing slash is what makes the listing's relative links
        // resolve inside the directory rather than beside it.
        if !request.uri().path().ends_with('/') {
            let location = format!("{}/", request.uri().path());
            return match HeaderValue::from_str(&location) {
                Ok(value) => {
                    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, value)]).into_response()
                }
                Err(_) => not_found(),
            };
        }
        return listing_response(&root, &target);
    }
    if !metadata.is_file() {
        return not_found();
    }
    // Asked of the bytes, not the name, and asked here because this is where the
    // bytes are about to be read anyway. A key renamed to report.txt passes
    // every name rule there is.
    if holds_key_material(&target) {
        return not_found();
    }
    file_response(&target, request.method() == Method::HEAD).await
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found in this shared directory").into_response()
}

async fn file_response(path: &Path, head_only: bool) -> Response {
    let Ok(metadata) = std::fs::metadata(path) else {
        return not_found();
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type = if mime.type_() == "text" {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    };
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_str(&content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        ),
        // The share serves whatever the agent wrote, so a browser must
        // not talk itself into a more dangerous type than the extension.
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (header::CONTENT_LENGTH, HeaderValue::from(metadata.len())),
    ];
    if head_only {
        return (headers, Body::empty()).into_response();
    }
    match tokio::fs::File::open(path).await {
        Ok(file) => (
            headers,
            Body::from_stream(ReaderStream::with_capacity(file, FILE_CHUNK_BYTES)),
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

fn listing_response(root: &Path, dir: &Path) -> Response {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return not_found();
    };
    let mut rows: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_hidden(&name) || is_denied(&name) {
                return None;
            }
            // A symlink is not followed anywhere else in the share, so
            // listing one would only produce a link that 404s.
            if entry
                .path()
                .symlink_metadata()
                .ok()?
                .file_type()
                .is_symlink()
            {
                return None;
            }
            Some((name, entry.file_type().ok()?.is_dir()))
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let truncated = rows.len() > LISTING_ENTRY_LIMIT;
    rows.truncate(LISTING_ENTRY_LIMIT);
    let title = dir
        .strip_prefix(root)
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .map(|rest| rest.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".into());
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        listing_html(&title, &rows, truncated),
    )
        .into_response()
}

fn listing_html(title: &str, rows: &[(String, bool)], truncated: bool) -> String {
    let mut html = format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n\
         <style>body{{font:14px/1.6 system-ui,sans-serif;margin:2rem auto;max-width:44rem;\
         padding:0 1rem}}h1{{font-size:1rem;font-weight:600;color:#555}}\
         ul{{list-style:none;padding:0}}li{{padding:.2rem 0}}\
         a{{text-decoration:none;color:#06c}}a:hover{{text-decoration:underline}}\
         p{{color:#888}}</style>\n\
         <h1>{}</h1>\n<ul>\n",
        escape(title),
        escape(title)
    );
    if rows.is_empty() {
        html.push_str("<p>This directory is empty.</p>\n");
    }
    for (name, is_dir) in rows {
        let suffix = if *is_dir { "/" } else { "" };
        html.push_str(&format!(
            "<li><a href=\"{}{suffix}\">{}{suffix}</a></li>\n",
            escape(&url_encode(name)),
            escape(name)
        ));
    }
    html.push_str("</ul>\n");
    if truncated {
        html.push_str(&format!(
            "<p>Only the first {LISTING_ENTRY_LIMIT} entries are shown.</p>\n"
        ));
    }
    html
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// Whether a name alone is enough to refuse an entry.
///
/// A name is never the whole answer, so this is paired with [`holds_key_material`]
/// wherever the bytes are at hand.
fn is_denied(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if denied_name(&lower) {
        return true;
    }
    Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| DENIED_EXTENSIONS.contains(&ext))
}

/// Whether a file's own first bytes say it is key material, whatever it is
/// called. Anything unreadable answers no: refusing to serve is the file
/// check's job elsewhere, and guessing here would turn an I/O error into a
/// confusing refusal.
fn holds_key_material(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; SNIFF_BYTES];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    head[..read].starts_with(PEM_ARMOUR)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DescendError {
    Traversal,
    Symlink,
    Denied,
    Missing,
    Io(String),
}

/// Walks `relative` below `root` one component at a time, refusing
/// traversal, any symlink, and anything a share never serves. Checking
/// each step rather than the final path is what keeps a symlink in the
/// middle of the walk from placing the target outside the share.
fn descend(root: &Path, relative: &Path) -> Result<PathBuf, DescendError> {
    let mut checked = root.to_path_buf();
    for component in relative.components() {
        let name = match component {
            Component::Normal(name) => name,
            Component::CurDir => continue,
            _ => return Err(DescendError::Traversal),
        };
        let name = name.to_string_lossy();
        if is_hidden(&name) || is_denied(&name) {
            return Err(DescendError::Denied);
        }
        checked.push(component);
        match std::fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(DescendError::Symlink),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(DescendError::Missing)
            }
            Err(error) => return Err(DescendError::Io(error.to_string())),
        }
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("out/nested")).unwrap();
        fs::write(dir.path().join("out/index.html"), "<p>report</p>").unwrap();
        fs::write(dir.path().join("out/nested/data.json"), "{}").unwrap();
        fs::write(dir.path().join("out/.env"), "SECRET=1").unwrap();
        fs::write(dir.path().join("out/server.pem"), "key").unwrap();
        fs::write(dir.path().join("secret.txt"), "top level").unwrap();
        dir
    }

    /// The names the old list missed, and the reason the shape was wrong rather
    /// than the list short. A probe against the real predicate served
    /// pm_shared_key, AuthKey_*.p8, secrets.yaml, service-account.json, *.der,
    /// *.ppk and *.kdbx — and pm_shared_key is not hypothetical: a file of
    /// exactly that name sits in the directory sessions on this project run
    /// from. Adding those names would have left the next one out.
    #[test]
    fn key_material_is_refused_by_what_it_is_and_by_names_worth_knowing() {
        // Binary containers that exist only to hold keys, which no header
        // sniffing can answer for.
        for name in [
            "bundle.p12",
            "store.jks",
            "client.ppk",
            "vault.kdbx",
            "key.der",
            "AuthKey_ABC123.p8",
            "signing.pk8",
            "server.key",
            "cert.pem",
            "keys.keystore",
            "deploy.pfx",
        ] {
            assert!(is_denied(name), "{name} would be served");
        }
        // A secret kept under a name something else chose: ssh's defaults, a
        // cloud credential file, and this installation's own.
        for name in [
            "id_rsa",
            "id_ed25519",
            "identity",
            "credentials",
            "credentials.json",
            "service-account.json",
            "secrets.yaml",
            "secrets.yml",
            "pm.db.secret",
            "worker-key.pem",
            "worker-key-laptop.pem",
            "worker.toml",
        ] {
            assert!(is_denied(name), "{name} would be served");
        }
        // Case is not a way around any of it.
        for name in ["ID_RSA", "Server.KEY", "AuthKey_X.P8", "PM.DB.SECRET"] {
            assert!(is_denied(name), "{name} would be served");
        }
        // And ordinary artifacts are still served, or a share is useless.
        for name in [
            "index.html",
            "report.md",
            "data.json",
            "screenshot.png",
            "app.js",
            "style.css",
            "notes.txt",
            "keynote.md",
            "monkey.svg",
        ] {
            assert!(!is_denied(name), "{name} was refused");
        }
    }

    /// The case a list of names can never cover: the same key under a name
    /// nobody would refuse. This is why the check asks the bytes.
    #[test]
    fn an_armoured_key_is_refused_whatever_it_is_called() {
        let dir = tempfile::tempdir().unwrap();
        let innocuous = dir.path().join("report.txt");
        fs::write(
            &innocuous,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        assert!(
            !is_denied("report.txt"),
            "the name is unremarkable, which is the point"
        );
        assert!(holds_key_material(&innocuous));

        for armour in [
            "-----BEGIN RSA PRIVATE KEY-----\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "-----BEGIN EC PRIVATE KEY-----\n",
            "-----BEGIN CERTIFICATE-----\n",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\n",
        ] {
            let path = dir.path().join("artifact.dat");
            fs::write(&path, armour).unwrap();
            assert!(holds_key_material(&path), "{armour} was not recognised");
        }
    }

    /// Reading the first bytes must not start refusing ordinary files, or a
    /// share stops being useful and the check gets removed.
    #[test]
    fn ordinary_files_are_not_mistaken_for_keys() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("index.html", "<!doctype html><p>report</p>"),
            ("data.json", "{\"ok\":true}"),
            ("notes.md", "# BEGIN here\n\nnot a key"),
            // Mentions the armour without being it.
            (
                "guide.md",
                "paste the -----BEGIN PRIVATE KEY----- block below",
            ),
            ("empty.txt", ""),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            assert!(!holds_key_material(&path), "{name} was taken for a key");
        }
        // An unreadable path answers no rather than guessing.
        assert!(!holds_key_material(&dir.path().join("does-not-exist")));
    }

    #[test]
    fn a_descendant_directory_resolves() {
        let dir = tree();
        let root = resolve_share_root(dir.path().to_str().unwrap(), "out").unwrap();
        assert_eq!(root, fs::canonicalize(dir.path().join("out")).unwrap());
    }

    #[test]
    fn an_absolute_path_inside_the_session_resolves() {
        let dir = tree();
        let absolute = dir.path().join("out");
        let root =
            resolve_share_root(dir.path().to_str().unwrap(), absolute.to_str().unwrap()).unwrap();
        assert_eq!(root, fs::canonicalize(&absolute).unwrap());
    }

    /// macOS keeps its temporary directories under a symlinked `/var`,
    /// so a session cwd and an absolute path the agent spells through
    /// the same symlink never share the canonical prefix.
    #[test]
    fn an_absolute_path_spelled_through_a_symlinked_cwd_resolves() {
        let dir = tree();
        let link = tempfile::tempdir().unwrap();
        let alias = link.path().join("session");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        let cwd = alias.to_str().unwrap();
        let root = resolve_share_root(cwd, alias.join("out").to_str().unwrap()).unwrap();
        assert_eq!(root, fs::canonicalize(dir.path().join("out")).unwrap());
        assert_eq!(
            resolve_share_root(cwd, link.path().join("elsewhere").to_str().unwrap()),
            Err(ShareRootError::Outside)
        );
    }

    #[test]
    fn traversal_and_outside_paths_are_refused() {
        let dir = tree();
        let cwd = dir.path().to_str().unwrap();
        assert_eq!(
            resolve_share_root(cwd, "../elsewhere"),
            Err(ShareRootError::Outside)
        );
        assert_eq!(
            resolve_share_root(cwd, "/etc"),
            Err(ShareRootError::Outside)
        );
    }

    #[test]
    fn a_file_is_not_a_share_root() {
        let dir = tree();
        assert_eq!(
            resolve_share_root(dir.path().to_str().unwrap(), "secret.txt"),
            Err(ShareRootError::NotADirectory)
        );
    }

    #[test]
    fn a_repository_root_is_refused() {
        let dir = tree();
        fs::create_dir(dir.path().join("out/.git")).unwrap();
        assert_eq!(
            resolve_share_root(dir.path().to_str().unwrap(), "out"),
            Err(ShareRootError::RepositoryRoot)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_share_root_is_refused() {
        let dir = tree();
        std::os::unix::fs::symlink(dir.path().join("out"), dir.path().join("link")).unwrap();
        assert_eq!(
            resolve_share_root(dir.path().to_str().unwrap(), "link"),
            Err(ShareRootError::Symlink)
        );
    }

    #[test]
    fn descent_refuses_traversal_hidden_and_key_material() {
        let dir = tree();
        let root = resolve_share_root(dir.path().to_str().unwrap(), "out").unwrap();
        assert_eq!(
            descend(&root, Path::new("../secret.txt")),
            Err(DescendError::Traversal)
        );
        assert_eq!(descend(&root, Path::new(".env")), Err(DescendError::Denied));
        assert_eq!(
            descend(&root, Path::new("server.pem")),
            Err(DescendError::Denied)
        );
        assert!(descend(&root, Path::new("nested/data.json")).is_ok());
    }

    #[test]
    fn a_listing_links_relatively_and_escapes_names() {
        let html = listing_html(
            "out",
            &[("a b&c".into(), false), ("sub".into(), true)],
            false,
        );
        assert!(html.contains("href=\"a%20b%26c\""), "{html}");
        assert!(html.contains(">a b&amp;c<"), "{html}");
        assert!(html.contains("href=\"sub/\""), "{html}");
        assert!(
            !html.contains("href=\"/"),
            "listing must not link to the origin root"
        );
    }

    #[test]
    fn a_truncated_listing_says_so() {
        let rows: Vec<(String, bool)> = (0..LISTING_ENTRY_LIMIT + 1)
            .map(|i| (format!("f{i}"), false))
            .collect();
        assert!(listing_html("out", &rows[..LISTING_ENTRY_LIMIT], true).contains("Only the first"));
    }

    #[test]
    fn percent_decoding_reads_escaped_names() {
        assert_eq!(percent_decode("a%20b%26c"), "a b&c");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }
}

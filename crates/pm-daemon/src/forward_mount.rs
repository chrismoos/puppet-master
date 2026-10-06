//! Where a published HTTP forward is mounted, and the one function that
//! names it.
//!
//! Three mounts are possible and the configuration picks one for the
//! whole controller. A share domain gives every forward its own origin,
//! which is what applications that emit root-relative URLs need. A port
//! range gives every forward its own listener, for a deployment that can
//! open ports but cannot add a wildcard DNS record. Neither configured
//! leaves forwards under the dashboard's own `/forwards/{id}/` path.

use reqwest::Url;

/// Label prefix for a forward's subdomain, so the share domain's own
/// apex and any other name under it can never read as a forward.
const SHARE_LABEL_PREFIX: &str = "f";

/// A slug becomes a DNS label under a share domain, so its bounds are a
/// hostname label's rather than a display name's.
const SLUG_MIN_LEN: usize = 3;
const SLUG_MAX_LEN: usize = 40;

/// Names a deployment is likely to want for itself under its own share
/// domain, kept out of the pool a forward can claim.
const RESERVED_SLUGS: [&str; 7] = ["www", "api", "app", "admin", "share", "pm", "mail"];

/// How much of the dashboard's browser context a preview still shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginSharing {
    /// The preview is the dashboard's origin, so its scripts are the
    /// dashboard's scripts.
    SameOrigin,
    /// Its own origin, so it reads no dashboard response and reaches no
    /// dashboard storage, but the same site, so one cookie jar.
    SameSite,
    /// A site of its own, which is the arrangement that isolates.
    SeparateSite,
}

/// How published HTTP forwards are mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountMode {
    /// One subdomain per forward under a wildcard share domain, each at
    /// the root of its own origin.
    ShareDomain(String),
    /// One listener per forward, bound from a port range, each at the
    /// root of its own origin.
    PerForwardPort { lo: u16, hi: u16 },
    /// Every forward under `/forwards/{id}/` on the dashboard's origin.
    PathPrefix,
}

impl MountMode {
    /// Applies the configured precedence: a share domain wins, a port
    /// range answers when there is no share domain, and the path prefix
    /// is what an unconfigured controller keeps doing.
    pub fn resolve(share_domain: Option<&str>, share_port_range: Option<(u16, u16)>) -> Self {
        if let Some(domain) = share_domain
            .map(|d| d.trim().trim_matches('.'))
            .filter(|d| !d.is_empty())
        {
            return Self::ShareDomain(domain.to_ascii_lowercase());
        }
        match share_port_range {
            Some((lo, hi)) => Self::PerForwardPort { lo, hi },
            None => Self::PathPrefix,
        }
    }

    /// What the browser still shares between a preview and the dashboard under
    /// this mount, measured against the host the dashboard is reached at.
    ///
    /// The three mounts differ in two independent ways, and conflating them is
    /// how an operator ends up believing a preview is contained when it is not.
    /// An *origin* decides what a page can read: a response, and storage. A
    /// *site* decides what the browser hands it without being asked, which is
    /// the cookie jar. A port makes a new origin and not a new site, and
    /// neither does a share domain on the dashboard's own site.
    ///
    /// An empty `dashboard_host` means the controller does not know the name it
    /// is reached by, so a share domain gets the benefit of the doubt.
    pub fn origin_sharing(&self, dashboard_host: &str) -> OriginSharing {
        match self {
            Self::ShareDomain(domain) => {
                match same_site_reason(domain, &normalize_host(dashboard_host)) {
                    Some(_) => OriginSharing::SameSite,
                    None => OriginSharing::SeparateSite,
                }
            }
            Self::PerForwardPort { .. } => OriginSharing::SameSite,
            Self::PathPrefix => OriginSharing::SameOrigin,
        }
    }

    /// The name this mode is reported under on `/api/version` and in the
    /// startup log.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShareDomain(_) => "share-domain",
            Self::PerForwardPort { .. } => "per-forward-port",
            Self::PathPrefix => "path-prefix",
        }
    }

    /// Whether a forward under this mode needs a listener of its own.
    pub fn binds_a_listener_per_forward(&self) -> bool {
        matches!(self, Self::PerForwardPort { .. })
    }

    /// The port range per-forward listeners bind from.
    pub fn port_range(&self) -> Option<(u16, u16)> {
        match self {
            Self::PerForwardPort { lo, hi } => Some((*lo, *hi)),
            _ => None,
        }
    }

    /// The forward label a `Host` header names, or `None` when the host
    /// is not a single label under this mode's share domain. Turning
    /// that label into a forward needs the forward table, so it stops
    /// here.
    pub fn host_forward_label(&self, host: &str) -> Option<String> {
        let Self::ShareDomain(domain) = self else {
            return None;
        };
        let host = host.trim().to_ascii_lowercase();
        // A bracketed IPv6 literal is never a share host, and its colons
        // are not a port separator.
        if host.starts_with('[') {
            return None;
        }
        let label = host
            .split(':')
            .next()?
            .strip_suffix(domain.as_str())?
            .strip_suffix('.')?;
        (!label.is_empty() && !label.contains('.')).then(|| label.to_string())
    }
}

/// The subdomain label one forward is reached at: its slug, or its id
/// for a row published before a slug was required.
pub fn share_host_label(forward_id: u64, slug: &str) -> String {
    if slug.is_empty() {
        legacy_share_label(forward_id)
    } else {
        slug.to_string()
    }
}

fn legacy_share_label(forward_id: u64) -> String {
    format!("{SHARE_LABEL_PREFIX}{forward_id}")
}

/// The forward id a host label names, for the rows that predate slugs.
/// A slug can never take this form, so the two namings cannot collide.
pub fn legacy_forward_label_id(label: &str) -> Option<u64> {
    let id = label
        .strip_prefix(SHARE_LABEL_PREFIX)?
        .parse::<u64>()
        .ok()?;
    (legacy_share_label(id) == label).then_some(id)
}

/// Whether a label reads as a forward id at all, including the leading
/// zeroes that [`legacy_forward_label_id`] refuses to resolve. The
/// validator rejects the whole shape so no slug can ever be mistaken
/// for one of those hosts.
fn reads_as_forward_id(label: &str) -> bool {
    match label.strip_prefix(SHARE_LABEL_PREFIX) {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Why a proposed slug is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlugError {
    Empty,
    Uppercase,
    InvalidCharacter(char),
    Reserved,
    ReadsAsForwardId,
    TooShort,
    TooLong,
    EdgeNotAlphanumeric,
    ConsecutiveHyphens,
}

impl std::fmt::Display for SlugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "a slug is required"),
            Self::Uppercase => write!(
                f,
                "a slug is lowercase: uppercase letters are rejected, not folded"
            ),
            Self::InvalidCharacter(c) => write!(
                f,
                "a slug holds only a-z, 0-9 and hyphen, so {c:?} cannot appear in one"
            ),
            Self::Reserved => write!(f, "that slug is reserved ({})", RESERVED_SLUGS.join(", ")),
            Self::ReadsAsForwardId => write!(
                f,
                "a slug cannot be {SHARE_LABEL_PREFIX} followed by digits: that form names a \
                 forward by id"
            ),
            Self::TooShort => write!(f, "a slug is at least {SLUG_MIN_LEN} characters"),
            Self::TooLong => write!(f, "a slug is at most {SLUG_MAX_LEN} characters"),
            Self::EdgeNotAlphanumeric => {
                write!(f, "a slug starts and ends with a letter or digit")
            }
            Self::ConsecutiveHyphens => write!(f, "a slug has no consecutive hyphens"),
        }
    }
}

/// The one rule for what a forward may be called. It is a DNS label
/// because under a share domain it becomes one, and it is checked the
/// same way in every mount mode so moving a controller onto a share
/// domain cannot strand names that were already taken.
pub fn validate_slug(slug: &str) -> Result<(), SlugError> {
    if slug.is_empty() {
        return Err(SlugError::Empty);
    }
    if slug.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(SlugError::Uppercase);
    }
    if let Some(c) = slug
        .chars()
        .find(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '-')
    {
        return Err(SlugError::InvalidCharacter(c));
    }
    if RESERVED_SLUGS.contains(&slug) {
        return Err(SlugError::Reserved);
    }
    if reads_as_forward_id(slug) {
        return Err(SlugError::ReadsAsForwardId);
    }
    if slug.len() < SLUG_MIN_LEN {
        return Err(SlugError::TooShort);
    }
    if slug.len() > SLUG_MAX_LEN {
        return Err(SlugError::TooLong);
    }
    let alphanumeric = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    if !alphanumeric(slug.chars().next()) || !alphanumeric(slug.chars().next_back()) {
        return Err(SlugError::EdgeNotAlphanumeric);
    }
    if slug.contains("--") {
        return Err(SlugError::ConsecutiveHyphens);
    }
    Ok(())
}

/// The dashboard origin a forward URL is composed against, decomposed
/// from the base URL the controller answers requests with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOrigin {
    /// The base URL verbatim, without a trailing slash. Path-prefix URLs
    /// extend this rather than recomposing it, so a configured
    /// `--public-url` reaches clients exactly as it was written.
    base_url: String,
    scheme: String,
    /// Host, an IPv6 literal kept bracketed.
    host: String,
    /// `:8443`, or empty when the origin uses the scheme's default port.
    port_suffix: String,
    /// Deployment path prefix, without a trailing slash.
    base_path: String,
}

impl MountOrigin {
    pub fn parse(base_url: &str) -> Option<Self> {
        let base_url = base_url.trim_end_matches('/');
        let url = Url::parse(base_url).ok()?;
        let host = match url.host_str()?.parse::<std::net::Ipv6Addr>() {
            Ok(v6) => format!("[{v6}]"),
            Err(_) => url.host_str()?.to_string(),
        };
        Some(Self {
            base_url: base_url.to_string(),
            scheme: url.scheme().to_string(),
            host,
            port_suffix: url.port().map(|p| format!(":{p}")).unwrap_or_default(),
            base_path: url.path().trim_end_matches('/').to_string(),
        })
    }

    pub fn base_path(&self) -> &str {
        &self.base_path
    }
}

/// One forward's public mount. Every client-facing forward URL comes
/// from [`forward_mount`], so the dashboard, iOS, `publish_port`, and the
/// proxy itself cannot disagree about where a forward lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardMount {
    /// The URL a client opens, always ending in `/`.
    pub url: String,
    /// Origin and deployment path the forward is reached through,
    /// without a trailing slash: what the upstream sees in
    /// `X-Forwarded-Host` and `X-Forwarded-Proto`.
    pub base: String,
    /// Path prefix requests arrive under, ending in `/`. `None` when the
    /// forward owns its whole origin, which is what removes the prefix
    /// machinery: no `X-Forwarded-Prefix`, and no `Location` or
    /// cookie-path mounting.
    pub prefix: Option<String>,
}

impl ForwardMount {
    pub fn accepts_destination(&self, destination: &str) -> bool {
        let Ok(url) = Url::parse(destination) else {
            return false;
        };
        let Ok(root) = Url::parse(&self.url) else {
            return false;
        };
        url.origin() == root.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path().starts_with(root.path())
    }

    /// Whether the mount is served over TLS, which decides the `Secure`
    /// attribute on the forward's own auth cookie.
    pub fn is_secure(&self) -> bool {
        self.url.starts_with("https://")
    }

    /// The path the forward's auth cookie is scoped to.
    pub fn cookie_path(&self) -> &str {
        self.prefix.as_deref().unwrap_or("/")
    }
}

/// The public mount of one forward under the active mode.
///
/// `listener_port` is the port a per-forward listener actually bound, and
/// is read in that mode only. `None` means the controller cannot name the
/// forward yet, which happens when a per-forward listener is not up.
pub fn forward_mount(
    mode: &MountMode,
    origin: &MountOrigin,
    forward_id: u64,
    slug: &str,
    listener_port: u16,
) -> Option<ForwardMount> {
    let rooted = |base: String| {
        Some(ForwardMount {
            url: format!("{base}/"),
            base,
            prefix: None,
        })
    };
    match mode {
        MountMode::ShareDomain(domain) => rooted(format!(
            "{}://{}.{domain}{}",
            origin.scheme,
            share_host_label(forward_id, slug),
            origin.port_suffix,
        )),
        MountMode::PerForwardPort { .. } => {
            if listener_port == 0 {
                return None;
            }
            rooted(format!(
                "{}://{}:{listener_port}",
                origin.scheme, origin.host
            ))
        }
        MountMode::PathPrefix => {
            let prefix = format!("{}/forwards/{forward_id}/", origin.base_path);
            Some(ForwardMount {
                url: format!("{}/forwards/{forward_id}/", origin.base_url),
                base: origin.base_url.clone(),
                prefix: Some(prefix),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_stay_inside_each_mount() {
        for mode in [
            MountMode::PathPrefix,
            MountMode::ShareDomain("pm-preview.example".into()),
            MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100,
            },
        ] {
            let mount = forward_mount(
                &mode,
                &origin("https://pm.example/dash"),
                7,
                "preview",
                40007,
            )
            .unwrap();
            assert!(mount.accepts_destination(&format!("{}deep/page?x=one%20two&y=3", mount.url)));
            assert!(!mount.accepts_destination(&format!("{}#fragment", mount.url)));
            assert!(!mount.accepts_destination(&mount.url.replace("https:", "http:")));
            assert!(!mount.accepts_destination(&mount.url.replace("example", "elsewhere")));
            assert!(!mount.accepts_destination(&mount.url.replace("://", "://user@")));
            if mount.prefix.is_some() {
                assert!(!mount.accepts_destination(&mount.url.replace("/7/", "/8/")));
                assert!(!mount.accepts_destination(&format!("{}../8/", mount.url)));
            }
        }
    }

    fn origin(base: &str) -> MountOrigin {
        MountOrigin::parse(base).unwrap()
    }

    #[test]
    fn a_share_domain_wins_over_a_port_range() {
        assert_eq!(
            MountMode::resolve(Some("pm-preview.example"), Some((40000, 40100))),
            MountMode::ShareDomain("pm-preview.example".into())
        );
    }

    #[test]
    fn a_port_range_answers_when_no_share_domain_is_set() {
        assert_eq!(
            MountMode::resolve(None, Some((40000, 40100))),
            MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100
            }
        );
    }

    #[test]
    fn neither_configured_keeps_the_path_prefix() {
        assert_eq!(MountMode::resolve(None, None), MountMode::PathPrefix);
    }

    /// An empty or whitespace value is an unset one, so a blank
    /// environment variable does not turn on a mode that cannot work.
    #[test]
    fn a_blank_share_domain_falls_through_to_the_next_mode() {
        assert_eq!(
            MountMode::resolve(Some("   "), Some((40000, 40100))),
            MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100
            }
        );
        assert_eq!(MountMode::resolve(Some(""), None), MountMode::PathPrefix);
    }

    #[test]
    fn a_share_domain_is_normalized_for_matching() {
        let mode = MountMode::resolve(Some(".PM-Preview.Example."), None);
        assert_eq!(mode, MountMode::ShareDomain("pm-preview.example".into()));
        assert_eq!(
            mode.host_forward_label("f7.pm-preview.example").as_deref(),
            Some("f7")
        );
    }

    /// The URL the item specifies: the forward at the root of its own
    /// subdomain, with the scheme the public URL sets.
    #[test]
    fn share_domain_mounts_a_forward_at_the_root_of_its_own_subdomain() {
        let mount = forward_mount(
            &MountMode::ShareDomain("pm-preview.example".into()),
            &origin("https://pm.example"),
            42,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://f42.pm-preview.example/");
        assert_eq!(mount.base, "https://f42.pm-preview.example");
        assert_eq!(mount.prefix, None);
        assert_eq!(mount.cookie_path(), "/");
        assert!(mount.is_secure());
    }

    /// The same daemon serves both hosts, so a dashboard on a
    /// non-default port puts its forwards on that port too.
    #[test]
    fn a_share_host_keeps_the_dashboard_port() {
        let mount = forward_mount(
            &MountMode::ShareDomain("pm-preview.example".into()),
            &origin("https://pm.example:8443"),
            42,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://f42.pm-preview.example:8443/");
    }

    /// A share mount ignores the deployment path prefix in the public
    /// URL: the forward owns its own origin from the root.
    #[test]
    fn a_share_host_ignores_the_dashboard_path_prefix() {
        let mount = forward_mount(
            &MountMode::ShareDomain("pm-preview.example".into()),
            &origin("https://pm.example/dash"),
            9,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://f9.pm-preview.example/");
        assert_eq!(mount.prefix, None);
    }

    #[test]
    fn a_plain_http_share_host_sets_no_secure_cookie() {
        let mount = forward_mount(
            &MountMode::ShareDomain("share.local".into()),
            &origin("http://pm.local:7676"),
            3,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "http://f3.share.local:7676/");
        assert!(!mount.is_secure());
    }

    #[test]
    fn a_per_forward_port_mounts_at_the_root_of_the_public_host() {
        let mount = forward_mount(
            &MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100,
            },
            &origin("https://pm.example:8443/dash"),
            42,
            "",
            40007,
        )
        .unwrap();
        assert_eq!(mount.url, "https://pm.example:40007/");
        assert_eq!(mount.base, "https://pm.example:40007");
        assert_eq!(mount.prefix, None);
    }

    /// Until the listener is up there is no port to name, and an
    /// unnamed forward is better than a guessed one.
    #[test]
    fn a_per_forward_port_has_no_url_before_its_listener_binds() {
        assert_eq!(
            forward_mount(
                &MountMode::PerForwardPort {
                    lo: 40000,
                    hi: 40100
                },
                &origin("https://pm.example"),
                42,
                "",
                0,
            ),
            None
        );
    }

    /// The path-prefix mount is what existing links use, so it extends
    /// the configured public URL verbatim, deployment prefix included.
    #[test]
    fn the_path_prefix_mount_extends_the_public_url_verbatim() {
        let mount = forward_mount(
            &MountMode::PathPrefix,
            &origin("https://controller.example:8443/pm"),
            42,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://controller.example:8443/pm/forwards/42/");
        assert_eq!(mount.base, "https://controller.example:8443/pm");
        assert_eq!(mount.prefix.as_deref(), Some("/pm/forwards/42/"));
        assert_eq!(mount.cookie_path(), "/pm/forwards/42/");
    }

    #[test]
    fn the_path_prefix_mount_has_no_deployment_prefix_at_a_bare_origin() {
        let mount = forward_mount(
            &MountMode::PathPrefix,
            &origin("http://127.0.0.1:7676"),
            5,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "http://127.0.0.1:7676/forwards/5/");
        assert_eq!(mount.prefix.as_deref(), Some("/forwards/5/"));
    }

    #[test]
    fn an_ipv6_origin_stays_bracketed_in_a_per_forward_port_url() {
        let mount = forward_mount(
            &MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100,
            },
            &origin("http://[2001:db8::5]:7676"),
            1,
            "",
            40000,
        )
        .unwrap();
        assert_eq!(mount.url, "http://[2001:db8::5]:40000/");
    }

    #[test]
    fn a_share_host_yields_its_single_label() {
        let mode = MountMode::ShareDomain("pm-preview.example".into());
        assert_eq!(
            mode.host_forward_label("docs-preview.pm-preview.example")
                .as_deref(),
            Some("docs-preview")
        );
        // The host is a name, so its case carries no meaning even
        // though a slug's does.
        assert_eq!(
            mode.host_forward_label("Docs-Preview.PM-Preview.Example:8443")
                .as_deref(),
            Some("docs-preview")
        );
    }

    /// Everything that is not a single label under the share domain must
    /// fall through to the dashboard rather than being dispatched at a
    /// forward.
    #[test]
    fn hosts_that_are_not_share_labels_do_not_dispatch() {
        let mode = MountMode::ShareDomain("pm-preview.example".into());
        for host in [
            // The share domain's own apex.
            "pm-preview.example",
            // The dashboard.
            "pm.example",
            // A deeper name under a label.
            "app.f42.pm-preview.example",
            "app.docs.pm-preview.example",
            // A different domain that merely ends the same way.
            "f42.evil-pm-preview.example",
            "",
            "[2001:db8::5]:7676",
        ] {
            assert_eq!(
                mode.host_forward_label(host),
                None,
                "host {host:?} dispatched"
            );
        }
    }

    /// The id form names a forward published before slugs existed.
    #[test]
    fn a_legacy_label_resolves_only_in_the_exact_form_it_was_published() {
        assert_eq!(legacy_forward_label_id("f42"), Some(42));
        assert_eq!(legacy_forward_label_id("f0"), Some(0));
        for label in ["f042", "f", "fx1", "f42x", "docs-preview", "42", ""] {
            assert_eq!(
                legacy_forward_label_id(label),
                None,
                "label {label:?} resolved"
            );
        }
    }

    /// Only the share-domain mode dispatches by host at all.
    #[test]
    fn the_other_modes_never_dispatch_by_host() {
        assert_eq!(
            MountMode::PathPrefix.host_forward_label("f42.pm-preview.example"),
            None
        );
        assert_eq!(
            MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100
            }
            .host_forward_label("f42.pm-preview.example"),
            None
        );
    }

    /// A base URL the controller cannot decompose names no forward, so
    /// the caller leaves the URL empty rather than composing a guess.
    #[test]
    fn a_base_url_without_a_host_is_not_an_origin() {
        assert_eq!(MountOrigin::parse("not a url"), None);
        assert_eq!(MountOrigin::parse(""), None);
        assert_eq!(MountOrigin::parse("mailto:dev@example.com"), None);
    }

    /// The slug is the whole share host under a share domain, which is
    /// the point of taking one.
    #[test]
    fn a_share_domain_mounts_a_named_forward_under_its_slug() {
        let mount = forward_mount(
            &MountMode::ShareDomain("pm-preview.example".into()),
            &origin("https://pm.example"),
            42,
            "docs-preview",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://docs-preview.pm-preview.example/");
        assert_eq!(mount.base, "https://docs-preview.pm-preview.example");
        assert_eq!(mount.prefix, None);
    }

    /// A row published before slugs existed keeps the host it was
    /// already handed out under.
    #[test]
    fn a_share_domain_keeps_the_id_host_for_an_unnamed_forward() {
        let mount = forward_mount(
            &MountMode::ShareDomain("pm-preview.example".into()),
            &origin("https://pm.example"),
            42,
            "",
            0,
        )
        .unwrap();
        assert_eq!(mount.url, "https://f42.pm-preview.example/");
    }

    /// The other two mounts address a forward by id, so a slug changes
    /// nothing about where they serve it.
    #[test]
    fn a_slug_does_not_move_the_other_mounts() {
        let per_port = forward_mount(
            &MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100,
            },
            &origin("https://pm.example:8443/dash"),
            42,
            "docs-preview",
            40007,
        )
        .unwrap();
        assert_eq!(per_port.url, "https://pm.example:40007/");
        let prefixed = forward_mount(
            &MountMode::PathPrefix,
            &origin("https://controller.example:8443/pm"),
            42,
            "docs-preview",
            0,
        )
        .unwrap();
        assert_eq!(
            prefixed.url,
            "https://controller.example:8443/pm/forwards/42/"
        );
        assert_eq!(prefixed.prefix.as_deref(), Some("/pm/forwards/42/"));
    }

    #[test]
    fn a_slug_is_a_dns_label() {
        for slug in [
            "api-docs",
            "docs-preview",
            "web",
            "a1b",
            "0ab",
            "vite-5173",
            "a-b-c-d",
        ] {
            assert_eq!(validate_slug(slug), Ok(()), "slug {slug:?} rejected");
        }
    }

    #[test]
    fn each_way_a_slug_can_be_wrong_has_its_own_reason() {
        for (slug, reason) in [
            ("", SlugError::Empty),
            ("Docs", SlugError::Uppercase),
            ("DOCS-PREVIEW", SlugError::Uppercase),
            ("docs_preview", SlugError::InvalidCharacter('_')),
            ("docs.preview", SlugError::InvalidCharacter('.')),
            ("docs preview", SlugError::InvalidCharacter(' ')),
            ("dócs", SlugError::InvalidCharacter('ó')),
            ("ab", SlugError::TooShort),
            ("-abc", SlugError::EdgeNotAlphanumeric),
            ("abc-", SlugError::EdgeNotAlphanumeric),
            ("a--b", SlugError::ConsecutiveHyphens),
            ("a---b", SlugError::ConsecutiveHyphens),
        ] {
            assert_eq!(validate_slug(slug), Err(reason), "slug {slug:?}");
        }
        assert_eq!(
            validate_slug(&"a".repeat(SLUG_MAX_LEN + 1)),
            Err(SlugError::TooLong)
        );
    }

    #[test]
    fn the_length_bounds_are_inclusive() {
        assert_eq!(validate_slug(&"a".repeat(SLUG_MIN_LEN)), Ok(()));
        assert_eq!(
            validate_slug(&"a".repeat(SLUG_MIN_LEN - 1)),
            Err(SlugError::TooShort)
        );
        assert_eq!(validate_slug(&"a".repeat(SLUG_MAX_LEN)), Ok(()));
        assert_eq!(
            validate_slug(&"a".repeat(SLUG_MAX_LEN + 1)),
            Err(SlugError::TooLong)
        );
    }

    /// The names a deployment is likely to want under its own share
    /// domain stay out of the pool, whatever their length.
    #[test]
    fn reserved_names_are_not_slugs() {
        for slug in RESERVED_SLUGS {
            assert_eq!(validate_slug(slug), Err(SlugError::Reserved), "{slug}");
        }
        // Only the exact name is reserved.
        assert_eq!(validate_slug("api-docs"), Ok(()));
        assert_eq!(validate_slug("my-app"), Ok(()));
    }

    /// The two ways a forward can be named share one namespace, so the
    /// id form is refused as a slug even in the spellings that would
    /// never resolve to a forward.
    #[test]
    fn a_slug_can_never_read_as_a_forward_id() {
        for slug in ["f42", "f0", "f042", "f00000000000000000000042"] {
            assert_eq!(
                validate_slug(slug),
                Err(SlugError::ReadsAsForwardId),
                "slug {slug:?}"
            );
        }
        for slug in ["f42a", "fa42", "forty2", "f-42"] {
            assert_eq!(validate_slug(slug), Ok(()), "slug {slug:?} rejected");
        }
    }

    /// Uppercase is rejected rather than folded, so a slug reads the
    /// same way everywhere it is shown.
    #[test]
    fn uppercase_is_refused_rather_than_lowercased() {
        assert_eq!(validate_slug("Docs-Preview"), Err(SlugError::Uppercase));
        assert_eq!(validate_slug("docs-preview"), Ok(()));
    }

    #[test]
    fn every_rejection_says_what_is_wrong() {
        for reason in [
            SlugError::Empty,
            SlugError::Uppercase,
            SlugError::InvalidCharacter('_'),
            SlugError::Reserved,
            SlugError::ReadsAsForwardId,
            SlugError::TooShort,
            SlugError::TooLong,
            SlugError::EdgeNotAlphanumeric,
            SlugError::ConsecutiveHyphens,
        ] {
            assert!(!reason.to_string().is_empty(), "{reason:?} said nothing");
        }
        assert!(
            SlugError::Reserved.to_string().contains("admin"),
            "the reserved list is not named"
        );
    }

    #[test]
    fn mount_modes_report_their_configured_name() {
        assert_eq!(MountMode::PathPrefix.as_str(), "path-prefix");
        assert_eq!(
            MountMode::ShareDomain("s.example".into()).as_str(),
            "share-domain"
        );
        assert_eq!(
            MountMode::PerForwardPort { lo: 1, hi: 2 }.as_str(),
            "per-forward-port"
        );
    }
}

/// A share domain that cannot carry forwards at all, which is the one thing
/// about it the daemon refuses rather than reports.
///
/// Whether previews under it are *isolated* from the dashboard is a separate
/// question, and the answer is reported: see [`SameSiteReason`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShareDomainError {
    #[error(
        "--share-domain {share:?} is not a name, so no forward could be given a \
         subdomain of it. Use a domain whose wildcard record points at this \
         controller."
    )]
    NotAName { share: String },
}

/// Why previews under a share domain share the dashboard's cookie jar.
///
/// A preview on its own subdomain is a different origin, so CORS keeps it from
/// reading a dashboard response and it reaches no dashboard storage. It is not
/// necessarily a different *site*, and `SameSite` is what decides whether the
/// browser attaches the session cookie to requests the preview makes to the
/// dashboard. A share domain under the dashboard's own registrable domain
/// therefore separates origins without separating cookie jars, which is the
/// arrangement a port range already gives, so it is the operator's call and
/// the mount reports it instead of the daemon refusing to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SameSiteReason {
    /// The share domain is the host the dashboard itself is reached at, so
    /// every preview is a label directly under it.
    DashboardHost,
    /// One contains the other, which no public suffix list can change.
    Subdomain,
    /// They share their final labels, which is the same site unless those
    /// labels are a public suffix this does not recognize.
    SharedRegistrableDomain(String),
}

impl std::fmt::Display for SameSiteReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DashboardHost => {
                write!(f, "the share domain is the dashboard's own host")
            }
            Self::Subdomain => write!(f, "one is a subdomain of the other"),
            Self::SharedRegistrableDomain(shared) => {
                write!(f, "both sit under {shared:?}")
            }
        }
    }
}

/// Second-level labels that national registries keep for their own hierarchy,
/// so `example.co.uk` and `other.co.uk` are different registrable domains
/// rather than neighbors under one.
///
/// This is a short list and not the Public Suffix List, because it is consulted
/// for one decision only: whether two hosts that share their last two labels
/// might still be different sites. A suffix it does not know is treated as an
/// ordinary domain, so a doubtful configuration is reported as same-site rather
/// than vouched for.
const REGISTRY_SECOND_LEVEL_LABELS: [&str; 16] = [
    "ac", "biz", "co", "com", "edu", "go", "gov", "id", "in", "info", "mil", "ne", "net", "or",
    "org", "sch",
];

/// Checks a configured share domain against the host the dashboard is reached
/// at. `dashboard_host` is a host or authority, normally from `--public-url`.
///
/// `Ok(Some(reason))` says previews would share the dashboard's cookie jar, for
/// the caller to report. `Ok(None)` says they are a site of their own, or that
/// there is no configured dashboard host to compare them against.
pub fn check_share_domain(
    share_domain: &str,
    dashboard_host: &str,
) -> Result<Option<SameSiteReason>, ShareDomainError> {
    let share = normalize_host(share_domain);
    if share.is_empty() {
        return Ok(None);
    }
    // A forward is named by prefixing a label, which an address cannot carry.
    if domain_labels(&share).is_none() {
        return Err(ShareDomainError::NotAName { share });
    }
    Ok(same_site_reason(&share, &normalize_host(dashboard_host)))
}

/// Why previews under `share` sit on the dashboard's site, or `None` when they
/// do not or when either host is unknown. Both arguments are normalized hosts.
fn same_site_reason(share: &str, dashboard: &str) -> Option<SameSiteReason> {
    if share.is_empty() || dashboard.is_empty() {
        return None;
    }
    if share == dashboard {
        return Some(SameSiteReason::DashboardHost);
    }
    let share_labels = domain_labels(share)?;
    // A cookie for an address host has no domain matching at all, so an
    // address and a name are never the same site.
    let dashboard_labels = domain_labels(dashboard)?;
    if is_subdomain_of(&share_labels, &dashboard_labels)
        || is_subdomain_of(&dashboard_labels, &share_labels)
    {
        return Some(SameSiteReason::Subdomain);
    }
    let shared = registrable_domain(&share_labels);
    (shared == registrable_domain(&dashboard_labels))
        .then_some(SameSiteReason::SharedRegistrableDomain(shared))
}

fn normalize_host(value: &str) -> String {
    let value = value.trim();
    let rest = value.split_once("://").map_or(value, |(_, rest)| rest);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        // A bare v6 literal is all colons and none of them a port separator.
        None if authority.matches(':').count() > 1 => authority,
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.trim_matches('.').to_ascii_lowercase()
}

/// The host's labels, or `None` when it is an address rather than a name.
fn domain_labels(host: &str) -> Option<Vec<&str>> {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    (!labels.iter().any(|label| label.is_empty())).then_some(labels)
}

/// Whether `inner` sits under `outer`, on a label boundary.
fn is_subdomain_of(inner: &[&str], outer: &[&str]) -> bool {
    inner.len() > outer.len() && inner[inner.len() - outer.len()..] == *outer
}

/// The labels a browser treats as one site. A single-label host is its own
/// registrable domain, and a recognised registry suffix takes one label more.
fn registrable_domain(labels: &[&str]) -> String {
    let suffix_labels = match labels {
        [.., second, last]
            if last.len() == 2
                && last.chars().all(|c| c.is_ascii_alphabetic())
                && REGISTRY_SECOND_LEVEL_LABELS.contains(second) =>
        {
            3
        }
        _ => 2,
    };
    labels[labels.len().saturating_sub(suffix_labels)..].join(".")
}

#[cfg(test)]
mod share_domain_validation {
    use super::{check_share_domain, SameSiteReason, ShareDomainError};

    fn same_site(share: &str, dashboard: &str) -> SameSiteReason {
        check_share_domain(share, dashboard)
            .unwrap_or_else(|e| panic!("{share} under {dashboard} was refused: {e}"))
            .unwrap_or_else(|| panic!("{share} under {dashboard} was called a separate site"))
    }

    fn separate_site(share: &str, dashboard: &str) {
        let reason = check_share_domain(share, dashboard)
            .unwrap_or_else(|e| panic!("{share} under {dashboard} was refused: {e}"));
        assert!(
            reason.is_none(),
            "{share} under {dashboard} was called the same site: {reason:?}"
        );
    }

    /// The configuration the check exists for. It is allowed, because the
    /// operator owns the trade, and it is named, because the isolation a share
    /// domain reads as buying is not there: a different origin, but one cookie
    /// jar.
    #[test]
    fn a_share_domain_under_the_dashboards_own_domain_is_reported_not_refused() {
        assert!(matches!(
            same_site("share.example.com", "https://pm.example.com"),
            SameSiteReason::SharedRegistrableDomain(ref shared) if shared == "example.com"
        ));
        assert_eq!(
            same_site("previews.example.com", "example.com"),
            SameSiteReason::Subdomain
        );
        // And the other way round, in case the dashboard is the deeper name.
        assert_eq!(
            same_site("example.com", "pm.example.com"),
            SameSiteReason::Subdomain
        );
    }

    /// Previews still get a label of their own under it, so the arrangement
    /// works and shares the cookie jar like any other same-site one.
    #[test]
    fn the_dashboards_own_host_as_the_share_domain_is_named_as_such() {
        assert_eq!(
            same_site("pm.example.com", "https://pm.example.com:8443/"),
            SameSiteReason::DashboardHost
        );
    }

    /// The arrangement every hosting provider uses, a dashboard on one
    /// registrable domain and user content on another.
    #[test]
    fn a_separate_registrable_domain_is_a_separate_site() {
        separate_site("pm-preview.app", "https://pm.example.com");
        separate_site("share.pm-preview.app", "https://pm.example.com");
        separate_site("previews.example.net", "https://pm.example.com");
    }

    /// A multi-label public suffix. Hosts under one registry suffix are
    /// different sites, and the recognized ones are treated that way.
    #[test]
    fn hosts_under_a_recognized_registry_suffix_are_different_sites() {
        separate_site("previews.other.co.uk", "https://pm.example.co.uk");
        separate_site("previews.other.com.au", "https://pm.example.com.au");
        separate_site("previews.other.ac.uk", "https://pm.example.ac.uk");
        // But the same registrable domain under one is still one site.
        assert!(matches!(
            same_site("share.example.co.uk", "https://pm.example.co.uk"),
            SameSiteReason::SharedRegistrableDomain(ref shared) if shared == "example.co.uk"
        ));
    }

    /// A suffix the short list does not know is treated as an ordinary domain,
    /// so the answer errs toward naming a shared cookie jar rather than
    /// vouching for isolation it cannot confirm.
    #[test]
    fn an_unrecognized_multi_label_suffix_errs_toward_reporting() {
        assert_eq!(
            same_site("previews.bob.github.io", "https://pm.alice.github.io"),
            SameSiteReason::SharedRegistrableDomain("github.io".to_string())
        );
    }

    /// An address cannot carry a forward's label in front of it, so it is not a
    /// share domain at all. This is the one refusal left.
    #[test]
    fn an_address_is_not_a_share_domain() {
        for share in ["127.0.0.1", "192.168.1.10", "::1", "[::1]:8443"] {
            assert!(
                matches!(
                    check_share_domain(share, "https://pm.example.com"),
                    Err(ShareDomainError::NotAName { .. })
                ),
                "{share} was taken for a domain"
            );
        }
    }

    /// A cookie for an address host has no domain matching, so a dashboard
    /// reached by address and a preview reached by name are never one site.
    #[test]
    fn a_dashboard_reached_by_address_is_never_the_same_site_as_a_name() {
        separate_site("previews.example.com", "http://127.0.0.1:7676");
        separate_site("previews.example.com", "http://[::1]:7676");
        separate_site("previews.example.com", "http://192.168.1.10:7676");
    }

    /// A single-label host is its own registrable domain, so a label under it
    /// is the same site and an unrelated name is not.
    #[test]
    fn localhost_is_its_own_registrable_domain() {
        assert_eq!(
            same_site("share.localhost", "http://localhost:7676"),
            SameSiteReason::Subdomain
        );
        assert_eq!(
            same_site("localhost", "http://localhost:7676"),
            SameSiteReason::DashboardHost
        );
        separate_site("previews.test", "http://localhost:7676");
        separate_site("share.localhost", "http://pm.example.com");
    }

    /// Without a configured public URL the daemon does not know the name it is
    /// reached by, so there is nothing to compare and the caller says so.
    #[test]
    fn nothing_to_compare_against_is_not_a_same_site_answer() {
        separate_site("share.example.com", "");
        separate_site("", "https://pm.example.com");
    }

    #[test]
    fn comparison_ignores_case_scheme_port_path_and_a_trailing_dot() {
        assert!(matches!(
            same_site(
                "Share.Example.COM.",
                "HTTPS://user@PM.example.com:8443/dashboard?x=1"
            ),
            SameSiteReason::SharedRegistrableDomain(_)
        ));
    }
}

#[cfg(test)]
mod origin_sharing_per_mount {
    use super::{MountMode, OriginSharing};

    const DASHBOARD: &str = "https://pm.example.com";

    /// The distinction the three mounts turn on, and the one an operator
    /// choosing between them is most likely to get wrong: a port makes a new
    /// origin and not a new site, so it separates storage and responses while
    /// leaving the cookie jar shared.
    #[test]
    fn a_port_range_is_a_separate_origin_but_not_a_separate_site() {
        assert_eq!(
            MountMode::PerForwardPort {
                lo: 40000,
                hi: 40100
            }
            .origin_sharing(DASHBOARD),
            OriginSharing::SameSite
        );
    }

    #[test]
    fn the_path_prefix_mount_shares_the_dashboards_own_origin() {
        assert_eq!(
            MountMode::PathPrefix.origin_sharing(DASHBOARD),
            OriginSharing::SameOrigin
        );
    }

    #[test]
    fn a_share_domain_on_its_own_registrable_domain_is_a_site_of_its_own() {
        assert_eq!(
            MountMode::ShareDomain("pm-preview.example".into()).origin_sharing(DASHBOARD),
            OriginSharing::SeparateSite
        );
    }

    /// The answer a share domain under the dashboard's own domain must give,
    /// since it is allowed: the same cookie jar a port range leaves shared.
    #[test]
    fn a_share_domain_on_the_dashboards_site_is_no_more_separate_than_a_port() {
        assert_eq!(
            MountMode::ShareDomain("share.example.com".into()).origin_sharing(DASHBOARD),
            OriginSharing::SameSite
        );
        assert_eq!(
            MountMode::ShareDomain("pm.example.com".into()).origin_sharing(DASHBOARD),
            OriginSharing::SameSite
        );
    }

    /// With no configured public URL there is nothing to compare against.
    #[test]
    fn an_unknown_dashboard_host_leaves_a_share_domain_its_benefit_of_the_doubt() {
        assert_eq!(
            MountMode::ShareDomain("share.example.com".into()).origin_sharing(""),
            OriginSharing::SeparateSite
        );
    }
}

//! Works out which address a push came from when the relay sits
//! behind reverse proxies.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use axum::http::header::{HeaderMap, HeaderName};

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// A network of this size is what one IPv6 customer is typically
/// assigned, so limiting single addresses would limit nothing.
const IPV6_SOURCE_PREFIX_BITS: u32 = 64;

/// The proxies whose `X-Forwarded-For` entries are believed. Empty
/// means the header is ignored and the TCP peer is the client.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<IpAddr>);

impl TrustedProxies {
    pub fn new(proxies: impl IntoIterator<Item = IpAddr>) -> Self {
        Self(proxies.into_iter().map(|ip| ip.to_canonical()).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.0.contains(&ip.to_canonical())
    }
}

/// The address the request came from. When the peer is a trusted
/// proxy, that is the rightmost `X-Forwarded-For` entry that is not
/// itself a trusted proxy: entries further left are whatever the
/// client chose to send. Falls back to the peer when the header has no
/// such entry or the entry does not parse.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &TrustedProxies) -> IpAddr {
    let peer = peer.to_canonical();
    if !trusted.contains(peer) {
        return peer;
    }
    let hops = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .rev();
    for hop in hops {
        match parse_hop(hop) {
            Some(ip) if trusted.contains(ip) => continue,
            Some(ip) => return ip,
            None => return peer,
        }
    }
    peer
}

fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        .map(|ip| ip.to_canonical())
}

/// The key a source is limited and blocked under: the address for
/// IPv4, the enclosing /64 for IPv6.
pub fn source_key(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let mask = u128::MAX << (u128::BITS - IPV6_SOURCE_PREFIX_BITS);
            let network = Ipv6Addr::from(u128::from(v6) & mask);
            format!("{network}/{IPV6_SOURCE_PREFIX_BITS}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY: &str = "127.0.0.1";

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn trusted(proxies: &[&str]) -> TrustedProxies {
        TrustedProxies::new(proxies.iter().map(|p| ip(p)))
    }

    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(X_FORWARDED_FOR, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn the_header_is_ignored_when_no_proxy_is_trusted() {
        let headers = forwarded(&["203.0.113.9"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &TrustedProxies::default()),
            ip(PROXY)
        );
    }

    /// Anyone reaching the relay directly could otherwise pick their
    /// own source address.
    #[test]
    fn the_header_is_ignored_from_a_peer_that_is_not_a_trusted_proxy() {
        let headers = forwarded(&["203.0.113.9"]);
        assert_eq!(
            client_ip(ip("198.51.100.7"), &headers, &trusted(&[PROXY])),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn a_trusted_proxy_supplies_the_client_address() {
        let headers = forwarded(&["203.0.113.9"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY])),
            ip("203.0.113.9")
        );
    }

    /// The left of the list is client-supplied when the proxy appends,
    /// so only the entry the trusted proxy added counts.
    #[test]
    fn a_spoofed_leading_entry_does_not_win() {
        let headers = forwarded(&["10.66.66.66, 203.0.113.9"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn trusted_proxies_in_the_chain_are_skipped() {
        let headers = forwarded(&["203.0.113.9, 10.0.0.2"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY, "10.0.0.2"])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn repeated_headers_read_as_one_list() {
        let headers = forwarded(&["10.66.66.66", "203.0.113.9"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn an_entry_may_carry_a_port() {
        let headers = forwarded(&["203.0.113.9:4711"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY])),
            ip("203.0.113.9")
        );
        let headers = forwarded(&["[2001:db8::1]:4711"]);
        assert_eq!(
            client_ip(ip(PROXY), &headers, &trusted(&[PROXY])),
            ip("2001:db8::1")
        );
    }

    #[test]
    fn a_missing_or_unreadable_header_falls_back_to_the_peer() {
        let proxies = trusted(&[PROXY]);
        assert_eq!(client_ip(ip(PROXY), &HeaderMap::new(), &proxies), ip(PROXY));
        assert_eq!(
            client_ip(ip(PROXY), &forwarded(&["unknown"]), &proxies),
            ip(PROXY)
        );
        assert_eq!(
            client_ip(ip(PROXY), &forwarded(&[PROXY]), &proxies),
            ip(PROXY)
        );
    }

    #[test]
    fn an_ipv4_mapped_peer_matches_its_ipv4_proxy_entry() {
        let headers = forwarded(&["203.0.113.9"]);
        assert_eq!(
            client_ip(ip("::ffff:127.0.0.1"), &headers, &trusted(&[PROXY])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn an_ipv4_source_is_keyed_by_address() {
        assert_eq!(source_key(ip("203.0.113.9")), "203.0.113.9");
        assert_eq!(source_key(ip("::ffff:203.0.113.9")), "203.0.113.9");
    }

    #[test]
    fn ipv6_sources_in_one_network_share_a_key() {
        let key = source_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"));
        assert_eq!(key, "2001:db8:1:2::/64");
        assert_eq!(source_key(ip("2001:db8:1:2::1")), key);
        assert_ne!(source_key(ip("2001:db8:1:3::1")), key);
    }
}

//! Redirect handling, including the SSRF guard behind [`FollowRedirects::Safe`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// What to do with 3xx responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowRedirects {
    /// Follow redirects, but refuse ones pointing at loopback, link-local or private IPs.
    #[default]
    Safe,
    /// Follow every redirect.
    All,
    /// Follow none; return the 3xx response as-is.
    None,
}

/// Why a redirect target was refused, so the message reaching [`crate::Error::Http`] says what
/// happened.
pub(crate) const BLOCKED_MESSAGE: &str =
    "refused to follow a redirect to a loopback, link-local, private or unspecified address; \
     pass FollowRedirects::All to allow it";

/// Why the URL a request was aimed at was refused.
pub(crate) const BLOCKED_TARGET_MESSAGE: &str =
    "refused to request a loopback, link-local, private or unspecified address; \
     pass FetcherBuilder::allow_private_addresses(true) to allow it";

/// Whether a redirect to `url` must be refused under [`FollowRedirects::Safe`].
///
/// # Limitation
///
/// A `reqwest` redirect policy is a synchronous callback on the connection path, so it cannot
/// resolve a hostname without blocking the runtime (`std::net::ToSocketAddrs` is blocking, and
/// doing it here would stall the reactor). The guard therefore works on the URL's *host* rather
/// than on its resolved address:
///
/// * a host that is written as an IP literal is checked against the loopback, link-local,
///   private, unspecified, broadcast and documentation ranges (IPv4 and IPv6, including
///   IPv4-mapped IPv6 literals);
/// * the hostnames `localhost`, `*.localhost`, `*.local`, `*.internal` and `*.home.arpa` are
///   refused by name;
/// * any other hostname is allowed here, so a public name whose DNS record points at a private
///   address (a "DNS rebinding" redirect) is **not** caught by this function.
///
/// The resolved-address half is covered elsewhere: unless
/// [`crate::http::FetcherBuilder::allow_private_addresses`] is on, the client resolves every host
/// it connects to through a guard that runs [`is_blocked_ip`] over the addresses `getaddrinfo`
/// returned, so a rebinding name fails to connect even though this function let it through.
pub fn is_blocked_redirect_target(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => is_blocked_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => is_blocked_ipv6(ip),
        Some(url::Host::Domain(domain)) => is_blocked_domain(domain),
        None => true,
    }
}

/// Whether an already-resolved address falls in one of the refused ranges.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        // 100.64.0.0/10, carrier-grade NAT.
        || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 0x40)
        // 0.0.0.0/8, "this network".
        || ip.octets()[0] == 0
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // `::ffff:a.b.c.d` (IPv4-mapped), `::a.b.c.d` (IPv4-compatible) and `64:ff9b::a.b.c.d`
    // (the NAT64 well-known prefix) all carry an IPv4 address that the guard has to see;
    // otherwise `http://[::127.0.0.1]/` walks straight past it.
    if let Some(embedded) = embedded_ipv4(ip) {
        return is_blocked_ipv4(embedded);
    }
    let segments = ip.segments();
    // fc00::/7, unique local addresses.
    (ip.octets()[0] & 0xfe) == 0xfc
        // fe80::/10, link-local unicast.
        || (segments[0] & 0xffc0) == 0xfe80
        // 2001:db8::/32, documentation.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

/// The IPv4 address an IPv6 literal embeds, if it embeds one.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }
    let segments = ip.segments();
    let tail = |segments: [u16; 8]| {
        Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        )
    };
    // `::a.b.c.d`, excluding `::` and `::1`, which the loopback/unspecified checks already own.
    if segments[..6].iter().all(|part| *part == 0) && (segments[6] != 0 || segments[7] > 1) {
        return Some(tail(segments));
    }
    // `64:ff9b::/96` and `64:ff9b:1::/48`, the NAT64 prefixes.
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        return Some(tail(segments));
    }
    None
}

fn is_blocked_domain(domain: &str) -> bool {
    let host = domain.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
}

/// A `reqwest` DNS resolver that refuses a name resolving into one of the blocked ranges.
///
/// [`is_blocked_redirect_target`] can only read the host as it is written, so a perfectly public
/// name whose `A` record is `127.0.0.1` walks past it. Resolution, unlike a redirect policy, is
/// an `async` hook, so the addresses can be checked before anything connects to them — and
/// `reqwest` asks the resolver again for every hop, so this covers redirects as well as the URL
/// the caller passed. A host written as an IP literal never reaches a resolver; that case is the
/// URL check's job.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PrivateAddressGuard;

impl reqwest::dns::Resolve for PrivateAddressGuard {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            // `tokio::net::lookup_host` runs the blocking `getaddrinfo` on the blocking pool, so
            // the reactor keeps turning while it waits. The port is irrelevant here: `reqwest`
            // replaces it with the one from the URL.
            let addresses: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(blocked) = addresses.iter().find(|address| is_blocked_ip(address.ip())) {
                let address = blocked.ip();
                return Err(
                    format!("{BLOCKED_TARGET_MESSAGE}: {host} resolves to {address}").into(),
                );
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Turn a [`FollowRedirects`] plus a hop limit into a `reqwest` policy.
pub(crate) fn policy(kind: FollowRedirects, max_redirects: usize) -> reqwest::redirect::Policy {
    match kind {
        FollowRedirects::None => reqwest::redirect::Policy::none(),
        FollowRedirects::All => reqwest::redirect::Policy::limited(max_redirects),
        FollowRedirects::Safe => reqwest::redirect::Policy::custom(move |attempt| {
            // `Policy::custom` does not enforce the hop limit for us. `previous` starts with the
            // original URL, which is not a redirect, so the comparison is `>` — the same one
            // `Policy::limited` makes, so `Safe` and `All` allow exactly as many hops.
            if attempt.previous().len() > max_redirects {
                return attempt.error(format!("too many redirects (limit is {max_redirects})"));
            }
            if is_blocked_redirect_target(attempt.url()) {
                let target = attempt.url().clone();
                return attempt.error(format!("{BLOCKED_MESSAGE}: {target}"));
            }
            attempt.follow()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(url: &str) -> bool {
        let parsed = url::Url::parse(url).expect("valid url");
        is_blocked_redirect_target(&parsed)
    }

    #[test]
    fn loopback_and_private_literals_are_blocked() {
        assert!(blocked("http://127.0.0.1/"));
        assert!(blocked("http://127.9.9.9:8080/x"));
        assert!(blocked("http://10.0.0.1/"));
        assert!(blocked("http://172.16.5.4/"));
        assert!(blocked("http://172.31.255.255/"));
        assert!(blocked("http://192.168.1.1/"));
        assert!(blocked("http://169.254.169.254/latest/meta-data/"));
        assert!(blocked("http://0.0.0.0/"));
        assert!(blocked("http://100.64.0.1/"));
        assert!(blocked("http://[::1]/"));
        assert!(blocked("http://[::]/"));
        assert!(blocked("http://[fd00::1]/"));
        assert!(blocked("http://[fe80::1]/"));
        assert!(blocked("http://[::ffff:127.0.0.1]/"));
        // IPv4 smuggled through an IPv6 literal.
        assert!(blocked("http://[::127.0.0.1]/"));
        assert!(blocked("http://[::ffff:169.254.169.254]/"));
        assert!(blocked("http://[64:ff9b::10.0.0.1]/"));
        assert!(blocked("http://[2001:db8::1]/"));
    }

    #[test]
    fn public_literals_are_allowed() {
        assert!(!blocked("http://8.8.8.8/"));
        assert!(!blocked("http://172.32.0.1/"));
        assert!(!blocked("http://172.15.0.1/"));
        assert!(!blocked("https://[2606:4700:4700::1111]/"));
    }

    #[test]
    fn internal_names_are_blocked_by_name() {
        assert!(blocked("http://localhost:8080/"));
        assert!(blocked("http://LOCALHOST/"));
        assert!(blocked("http://api.localhost/"));
        assert!(blocked("http://printer.local/"));
        assert!(blocked("http://db.internal/"));
        assert!(blocked("http://router.home.arpa/"));
        assert!(!blocked("https://www.example.com/"));
    }

    #[test]
    fn resolved_addresses_can_be_checked_directly() {
        assert!(is_blocked_ip("127.0.0.1".parse().expect("ip")));
        assert!(!is_blocked_ip("1.1.1.1".parse().expect("ip")));
    }

    /// The name-based guard cannot see where a hostname points, so the resolver guard checks the
    /// addresses `getaddrinfo` actually returned. `localhost` is the one name that resolves into
    /// a blocked range without needing the network.
    #[tokio::test]
    async fn the_resolver_guard_refuses_a_name_pointing_into_a_blocked_range() {
        use reqwest::dns::Resolve as _;

        let Ok(resolved) = tokio::net::lookup_host(("localhost", 0)).await else {
            // No resolver on this machine; there is nothing for the guard to catch.
            return;
        };
        let addresses: Vec<_> = resolved.collect();
        assert!(
            addresses.iter().all(|address| is_blocked_ip(address.ip())),
            "localhost resolved to {addresses:?}"
        );

        let name: reqwest::dns::Name = "localhost".parse().expect("a valid name");
        let refused = PrivateAddressGuard.resolve(name).await;
        let Err(error) = refused else {
            panic!("a name resolving into loopback must not connect");
        };
        let message = error.to_string();
        assert!(message.contains("resolves to"), "got `{message}`");
        assert!(message.contains(BLOCKED_TARGET_MESSAGE), "got `{message}`");
    }

    #[test]
    fn default_policy_is_safe() {
        assert_eq!(FollowRedirects::default(), FollowRedirects::Safe);
    }
}

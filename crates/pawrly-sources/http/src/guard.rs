//! Outbound-target policy for SQL-driven HTTP requests: same-origin with
//! `base_url` (or a listed `allowed_host`) is trusted; other pivots must be
//! public. Enforced lexically in [`check_target`] and on resolved IPs in
//! [`GuardedResolver`] (which catches DNS rebinding).

use std::net::{IpAddr, SocketAddr};

use url::{Host, Url};

#[derive(Clone, Debug)]
pub(crate) enum HostPattern {
    Exact(String),
    Suffix(String),
}

impl HostPattern {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        if let Some(rest) = s.strip_prefix("*.") {
            let rest = rest.trim_end_matches('.');
            if rest.is_empty() {
                return Err("wildcard needs a suffix".into());
            }
            if psl::suffix_str(rest).is_some_and(|suf| suf.eq_ignore_ascii_case(rest)) {
                return Err(format!(
                    "`*.{rest}` spans a whole public suffix; list exact hosts instead"
                ));
            }
            return Ok(Self::Suffix(format!(".{rest}")));
        }
        let s = s.trim_end_matches('.');
        if s.is_empty() || s.contains('*') {
            return Err("expected a hostname or `*.suffix`".into());
        }
        Ok(Self::Exact(s.to_string()))
    }

    pub(crate) fn is_wildcard(&self) -> bool {
        matches!(self, Self::Suffix(_))
    }

    fn matches(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        match self {
            Self::Exact(h) => host == *h,
            Self::Suffix(suffix) => host.len() > suffix.len() && host.ends_with(suffix.as_str()),
        }
    }
}

pub(crate) fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

pub(crate) fn is_trusted(url: &Url, base: &Url, allowed: &[HostPattern]) -> bool {
    same_origin(url, base) || host_allowed(url, allowed)
}

fn host_allowed(url: &Url, allowed: &[HostPattern]) -> bool {
    url.host_str()
        .is_some_and(|h| allowed.iter().any(|p| p.matches(h)))
}

enum IpClass {
    Allow,
    Private,
    AlwaysRefuse,
}

fn classify(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                IpClass::Allow
            } else if v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() {
                IpClass::AlwaysRefuse
            } else {
                let o = v4.octets();
                if v4.is_private() || (o[0] == 100 && (64..128).contains(&o[1])) {
                    IpClass::Private
                } else {
                    IpClass::Allow
                }
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                IpClass::Allow
            } else {
                let seg = v6.segments()[0];
                if v6.is_unspecified() || (seg & 0xffc0) == 0xfe80 {
                    IpClass::AlwaysRefuse
                } else if (seg & 0xfe00) == 0xfc00 {
                    IpClass::Private
                } else {
                    IpClass::Allow
                }
            }
        }
    }
}

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

pub(crate) fn check_target(url: &Url, base: &Url, allowed: &[HostPattern]) -> Result<(), String> {
    if same_origin(url, base) {
        return Ok(());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "refusing `{url}`: only http(s) targets are allowed"
        ));
    }
    match url.host() {
        None => Err(refusal(url, "host-less")),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if host_allowed(url, allowed) || d == "localhost" || d.ends_with(".localhost") {
                Ok(())
            } else if d.ends_with(".internal") {
                Err(refusal(url, "`.internal`"))
            } else {
                Ok(())
            }
        }
        Some(Host::Ipv4(ip)) => check_ip(IpAddr::V4(ip), url, allowed),
        Some(Host::Ipv6(ip)) => check_ip(normalize(IpAddr::V6(ip)), url, allowed),
    }
}

fn check_ip(ip: IpAddr, url: &Url, allowed: &[HostPattern]) -> Result<(), String> {
    match classify(ip) {
        IpClass::Allow => Ok(()),
        IpClass::AlwaysRefuse => Err(format!(
            "refusing cross-origin request to `{url}`: link-local/metadata targets are never \
             reachable from a query"
        )),
        IpClass::Private if host_allowed(url, allowed) => Ok(()),
        IpClass::Private => Err(refusal(url, "private")),
    }
}

fn refusal(url: &Url, what: &str) -> String {
    format!(
        "refusing cross-origin request to `{url}`: {what} targets are not reachable from a query \
         (declare the host as a source's `base_url` or `allowed_hosts`)"
    )
}

/// An authed source refuses a hop to an untrusted host: reqwest only strips
/// `Authorization` cross-origin, so a custom header/body credential would ride along.
pub(crate) fn redirect_policy(
    base: Url,
    allowed: Vec<HostPattern>,
    has_auth: bool,
) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 5 {
            return attempt.error("too many redirects");
        }
        let url = attempt.url();
        if let Err(e) = check_target(url, &base, &allowed) {
            return attempt.error(e);
        }
        if has_auth && !is_trusted(url, &base, &allowed) {
            return attempt
                .error("refusing to follow a credentialed redirect to an untrusted host");
        }
        attempt.follow()
    })
}

/// A `reqwest` resolver that refuses hostnames resolving into private/metadata space — closes DNS rebinding.
pub(crate) struct GuardedResolver {
    base_host: Option<String>,
    allowed: Vec<HostPattern>,
}

impl GuardedResolver {
    pub(crate) fn new(base: &Url, allowed: Vec<HostPattern>) -> Self {
        Self {
            base_host: base.host_str().map(|h| h.to_ascii_lowercase()),
            allowed,
        }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let base_host = self.base_host.clone();
        let allowed = self.allowed.clone();
        Box::pin(async move {
            let host = name.as_str().to_ascii_lowercase();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            screen_resolved(&host, &addrs, base_host.as_deref(), &allowed).map_err(
                |e| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(std::io::Error::other(e))
                },
            )?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn name_trusted(name: &str, base_host: Option<&str>, allowed: &[HostPattern]) -> bool {
    base_host.is_some_and(|b| b.eq_ignore_ascii_case(name))
        || allowed.iter().any(|p| p.matches(name))
}

fn screen_resolved(
    name: &str,
    addrs: &[SocketAddr],
    base_host: Option<&str>,
    allowed: &[HostPattern],
) -> Result<(), String> {
    let trusted = name_trusted(name, base_host, allowed);
    for addr in addrs {
        match classify(normalize(addr.ip())) {
            IpClass::Allow => {}
            IpClass::AlwaysRefuse => {
                return Err(format!(
                    "refusing `{name}`: resolves to a link-local/metadata address"
                ));
            }
            IpClass::Private if !trusted => {
                return Err(format!(
                    "refusing `{name}`: resolves to a private address {}",
                    addr.ip()
                ));
            }
            IpClass::Private => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn patterns(specs: &[&str]) -> Vec<HostPattern> {
        specs
            .iter()
            .filter_map(|s| HostPattern::parse(s).ok())
            .collect()
    }

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn same_origin_allows_private_bases() {
        let base = u("http://10.0.0.5:8080/api/");
        assert!(check_target(&u("http://10.0.0.5:8080/api/v2/x"), &base, &[]).is_ok());
        assert!(check_target(&u("http://10.0.0.5:9090/x"), &base, &[]).is_err());
    }

    #[test]
    fn pivots_to_public_and_loopback_pass() {
        let base = u("http://localhost/");
        for ok in [
            "https://api.github.com/meta",
            "http://93.184.216.34/x",
            "http://127.0.0.1:9999/x",
            "http://localhost:3000/x",
        ] {
            assert!(check_target(&u(ok), &base, &[]).is_ok(), "{ok}");
        }
    }

    #[test]
    fn pivots_to_internal_networks_are_refused() {
        let base = u("https://api.example.com/");
        for bad in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.1.2.3/admin",
            "http://172.16.0.1/",
            "http://192.168.1.1/router",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "ftp://example.com/x",
        ] {
            assert!(check_target(&u(bad), &base, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn allowed_hosts_extend_trust_including_private() {
        let base = u("https://api.example.com/");
        let allowed = patterns(&["uploads.example.net", "*.internal.example", "10.9.9.9"]);
        for ok in [
            "https://uploads.example.net/blob",
            "https://cache.internal.example/x",
            "http://10.9.9.9/admin",
        ] {
            assert!(check_target(&u(ok), &base, &allowed).is_ok(), "{ok}");
            assert!(is_trusted(&u(ok), &base, &allowed), "{ok}");
        }
        assert!(check_target(&u("http://10.9.9.10/x"), &base, &allowed).is_err());
        assert!(check_target(&u("ftp://uploads.example.net/x"), &base, &allowed).is_err());
    }

    #[test]
    fn allowlisted_metadata_is_still_refused() {
        let base = u("https://api.example.com/");
        let allowed = patterns(&["169.254.169.254"]);
        assert!(check_target(&u("http://169.254.169.254/latest/"), &base, &allowed).is_err());
    }

    #[test]
    fn suffix_wildcard_matches_subdomains_only() {
        let HostPattern::Suffix(_) = HostPattern::parse("*.example.com").unwrap() else {
            panic!("expected suffix");
        };
        let p = HostPattern::parse("*.example.com").unwrap();
        assert!(p.matches("a.example.com"));
        assert!(p.matches("a.b.example.com"));
        assert!(!p.matches("example.com"));
        assert!(!p.matches("notexample.com"));
    }

    #[test]
    fn over_broad_wildcards_are_rejected() {
        for bad in [
            "*.com",
            "*.co.uk",
            "*.io",
            "*.githubusercontent.com",
            "*.s3.amazonaws.com",
            "*.",
            "*",
        ] {
            assert!(HostPattern::parse(bad).is_err(), "{bad}");
        }
        for ok in ["*.example.com", "*.api.acme.example", "uploads.github.com"] {
            assert!(HostPattern::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn screen_resolved_blocks_rebinding() {
        let allowed = patterns(&["internal.trusted"]);
        let base = Some("api.example.com");
        assert!(screen_resolved("cdn.x.com", &[sa("93.184.216.34:0")], base, &allowed).is_ok());
        assert!(screen_resolved("evil.example", &[sa("10.0.0.5:0")], base, &allowed).is_err());
        assert!(screen_resolved("api.example.com", &[sa("10.0.0.5:0")], base, &allowed).is_ok());
        assert!(screen_resolved("internal.trusted", &[sa("10.0.0.5:0")], base, &allowed).is_ok());
        assert!(
            screen_resolved(
                "api.example.com",
                &[sa("169.254.169.254:0")],
                base,
                &allowed
            )
            .is_err()
        );
    }
}

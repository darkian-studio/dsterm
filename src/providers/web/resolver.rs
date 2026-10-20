//! Bind-time SSRF guard for the web fetch clients.
//!
//! `NetworkPermissions::check` screens URL text and never sees what DNS
//! answers, so a hostname that passes the host check can still point at
//! 169.254.169.254 or 127.0.0.1 the moment resolution happens. Every client
//! built by `HttpService` resolves through [`ValidatingResolver`], which
//! runs each concern: the system resolver's answer is screened against the
//! same private-network rules as the URL checks before hyper may connect,
//! and an answer containing even one forbidden address is refused whole -
//! a mixed A/AAAA list must not be free to steer the connection onto its
//! private entry.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use super::permissions::{is_ipv6_private, is_private_ip_detailed, NetworkPermissions};

pub struct ValidatingResolver {
    permissions: NetworkPermissions,
}

impl ValidatingResolver {
    pub fn new(permissions: NetworkPermissions) -> Self {
        Self { permissions }
    }

    /// Build the shared resolver each fetch client resolves through.
    pub fn shared(permissions: NetworkPermissions) -> Arc<Self> {
        Arc::new(Self::new(permissions))
    }
}

impl reqwest::dns::Resolve for ValidatingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let permissions = self.permissions.clone();
        Box::pin(async move {
            resolve_checked(&host, permissions)
                .await
                .map(|addrs| Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
                .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

/// Resolve `host` and screen every address the system resolver returns.
/// IP literals skip lookup (hyper-util connects them without consulting a
/// resolver) and are screened directly.
async fn resolve_checked(
    host: &str,
    permissions: NetworkPermissions,
) -> Result<Vec<SocketAddr>, io::Error> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        if address_forbidden(&ip) && !permissions.allow_private {
            return Err(forbidden(format!(
                "IP literal {host} is a private network address; connection refused"
            )));
        }
        return Ok(vec![SocketAddr::new(ip, 0)]);
    }

    let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|e| io::Error::other(format!("DNS resolution of {host} failed: {e}")))?
        .collect();

    if addrs.is_empty() {
        return Err(io::Error::other(format!(
            "DNS resolution of {host} returned no addresses"
        )));
    }

    if !permissions.allow_private {
        for addr in &addrs {
            let ip = addr.ip();
            if address_forbidden(&ip) {
                return Err(forbidden(format!(
                    "DNS resolution of {host} yielded private network address {ip}; connection refused"
                )));
            }
        }
    }

    Ok(addrs)
}

fn forbidden(detail: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, detail)
}

/// Screen a resolved address with the same rules the URL-text checks use:
/// the v4 path through `is_private_ip_detailed`, the v6 path through
/// `is_ipv6_private`, which already unwraps IPv4-mapped forms such as
/// ::ffff:127.0.0.1.
fn address_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ip_detailed(&v4.to_string()).is_some(),
        IpAddr::V6(v6) => is_ipv6_private(&v6.to_string()).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the deny matrix of the permissions.rs tests, but at
    /// resolved-address granularity - what the resolver actually screens.
    #[test]
    fn forbids_rebinding_targets() {
        for literal in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.7",
            "172.16.0.9",
            "172.31.255.255",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "fe80::1",
            "fc00::1",
            "fd00:dead:beef::1",
            "ff02::1",
        ] {
            let ip: IpAddr = literal.parse().expect(literal);
            assert!(address_forbidden(&ip), "allowed: {literal}");
        }
    }

    #[test]
    fn allows_public_resolved_addresses() {
        for literal in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = literal.parse().expect(literal);
            assert!(!address_forbidden(&ip), "forbidden: {literal}");
        }
    }

    #[tokio::test]
    async fn literal_screening_respects_allow_private() {
        let open = NetworkPermissions {
            allow_private: true,
            ..Default::default()
        };
        let addrs = resolve_checked("127.0.0.1", open)
            .await
            .expect("allow_private is an opt-in");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip().to_string(), "127.0.0.1");

        let strict = NetworkPermissions::default();
        let err = resolve_checked("127.0.0.1", strict)
            .await
            .expect_err("loopback literal must be screened under default permissions");
        assert!(err.to_string().contains("private network address"));
    }
}

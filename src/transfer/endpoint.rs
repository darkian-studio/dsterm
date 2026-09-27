//! Endpoint parsing for the transfer sender.
//!
//! Unbracketed `::1:8770` has two valid parses (port vs. last hextet), so we
//! refuse to guess and tell the user to bracket instead of picking one.

use crate::transfer::DEFAULT_TRANSFER_PORT;
use anyhow::{anyhow, Result};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub is_ipv6: bool,
}

pub fn parse_endpoint(input: &str) -> Result<Endpoint> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("empty endpoint: expected <host>[:port]");
    }
    if input.starts_with('[') {
        let end = input.find(']').ok_or_else(|| {
            anyhow!(
                "invalid endpoint '{input}': missing closing ']' (expected [ipv6] or [ipv6]:port)"
            )
        })?;
        let host = &input[1..end];
        if host.is_empty() {
            anyhow::bail!("invalid endpoint '{input}': empty IPv6 address inside brackets");
        }
        let rest = &input[end + 1..];
        if rest.is_empty() {
            return Ok(Endpoint {
                host: host.to_string(),
                port: DEFAULT_TRANSFER_PORT,
                is_ipv6: true,
            });
        }
        let port_str = rest.strip_prefix(':').ok_or_else(|| {
            anyhow!("invalid endpoint '{input}': unexpected trailing characters after ']' (expected [ipv6] or [ipv6]:port)")
        })?;
        if port_str.is_empty() {
            anyhow::bail!("invalid endpoint '{input}': empty port after ':'");
        }
        let port = parse_port(port_str)?;
        return Ok(Endpoint {
            host: host.to_string(),
            port,
            is_ipv6: true,
        });
    }
    let colon_count = input.matches(':').count();
    if colon_count == 0 {
        return Ok(Endpoint {
            host: input.to_string(),
            port: DEFAULT_TRANSFER_PORT,
            is_ipv6: false,
        });
    }
    if colon_count == 1 {
        let (host, port_str) = input.rsplit_once(':').unwrap();
        if host.is_empty() {
            anyhow::bail!("invalid endpoint '{input}': empty host before ':'");
        }
        if port_str.is_empty() {
            anyhow::bail!("invalid endpoint '{input}': empty port after ':'");
        }
        let port = parse_port(port_str)?;
        return Ok(Endpoint {
            host: host.to_string(),
            port,
            is_ipv6: false,
        });
    }
    anyhow::bail!(
        "ambiguous endpoint '{input}': IPv6 literals must use bracket notation, e.g. [::1]:{DEFAULT_TRANSFER_PORT} (got an unbracketed address with a port suffix)"
    );
}

fn parse_port(s: &str) -> Result<u16> {
    let port: u32 = s
        .parse()
        .map_err(|_| anyhow!("invalid port '{s}': expected 1-65535"))?;
    if port == 0 || port > 65535 {
        anyhow::bail!("invalid port '{s}': expected 1-65535");
    }
    Ok(port as u16)
}

// Dual-stack hosts often resolve to both families; trying IPv6 first avoids
// a long IPv4 timeout when the v6 path is the healthy one.
pub async fn resolve_and_connect(endpoint: &Endpoint) -> Result<TcpStream> {
    use tokio::net::lookup_host;
    let addr_str = format!("{}:{}", endpoint.host, endpoint.port);
    let mut addrs = lookup_host(addr_str).await.map_err(|e| {
        anyhow!(
            "destination unreachable: cannot resolve '{}': {e}",
            endpoint.host
        )
    })?;
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for a in addrs.by_ref() {
        if a.is_ipv6() {
            v6.push(a);
        } else {
            v4.push(a);
        }
    }
    let mut ordered = v6;
    ordered.extend(v4);
    if ordered.is_empty() {
        anyhow::bail!(
            "destination unreachable: no addresses for '{}'",
            endpoint.host
        );
    }
    let mut last_err = None;
    for addr in ordered {
        match tokio::time::timeout(std::time::Duration::from_secs(10), TcpStream::connect(addr))
            .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last_err = Some(e.to_string()),
            Err(_) => last_err = Some("connection timeout".to_string()),
        }
    }
    Err(anyhow!(
        "connection failed to {}:{} ({})",
        endpoint.host,
        endpoint.port,
        last_err.unwrap_or_else(|| "no address succeeded".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_uses_default_port() {
        let e = parse_endpoint("192.168.1.42").unwrap();
        assert_eq!(e.host, "192.168.1.42");
        assert_eq!(e.port, DEFAULT_TRANSFER_PORT);
    }

    #[test]
    fn host_port_split() {
        let e = parse_endpoint("192.168.1.42:8770").unwrap();
        assert_eq!(e.port, 8770);
    }

    #[test]
    fn bracketed_ipv6_with_and_without_port() {
        let e = parse_endpoint("[::1]:8770").unwrap();
        assert_eq!(e.host, "::1");
        assert_eq!(e.port, 8770);
        assert!(e.is_ipv6);
        let e2 = parse_endpoint("[::1]").unwrap();
        assert_eq!(e2.port, DEFAULT_TRANSFER_PORT);
    }

    #[test]
    fn unbracketed_ipv6_with_port_is_rejected() {
        let err = parse_endpoint("::1:8770").unwrap_err().to_string();
        assert!(err.contains("bracket"), "unexpected: {err}");
    }

    #[test]
    fn invalid_ports_rejected() {
        assert!(parse_endpoint("host:0").is_err());
        assert!(parse_endpoint("host:notaport").is_err());
        assert!(parse_endpoint("[::1").is_err());
    }
}

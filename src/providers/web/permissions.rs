use reqwest::Url;
use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
pub struct NetworkPermissions {
    pub allow_private: bool,
    pub allow_file: bool,
    pub allow_data_uri: bool,
    pub extra_denied_hosts: Vec<String>,
}

impl Default for NetworkPermissions {
    fn default() -> Self {
        Self {
            allow_private: false,
            allow_file: false,
            allow_data_uri: false,
            extra_denied_hosts: vec![],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("file:// URLs are not allowed")]
    FileUrlDenied,
    #[error("data: URIs are not allowed")]
    DataUriDenied,
    #[error("localhost/loopback URLs are not allowed")]
    LocalhostDenied,
    #[error("private network (RFC1918) target {0} is not allowed")]
    PrivateNetworkDenied(String),
    #[error("IPv6 link-local or ULA address {0} is not allowed")]
    IPv6PrivateDenied(String),
    #[error("host {0} is explicitly denied")]
    HostDenied(String),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),
}

impl NetworkPermissions {
    pub fn check(&self, url_str: &str) -> Result<(), PermissionError> {
        let url = Url::parse(url_str).map_err(|e| PermissionError::InvalidUrl(e.to_string()))?;

        match url.scheme() {
            "file" => {
                if !self.allow_file {
                    return Err(PermissionError::FileUrlDenied);
                }
                return Ok(());
            }
            "data" => {
                if !self.allow_data_uri {
                    return Err(PermissionError::DataUriDenied);
                }
                return Ok(());
            }
            "http" | "https" => {}
            other => {
                return Err(PermissionError::UnsupportedScheme(other.to_string()));
            }
        }

        let host = url
            .host_str()
            .ok_or_else(|| PermissionError::InvalidUrl("no host".into()))?;

        if self.extra_denied_hosts.iter().any(|h| h == host) {
            return Err(PermissionError::HostDenied(host.to_string()));
        }

        if !self.allow_private {
            if is_localhost(host) {
                return Err(PermissionError::LocalhostDenied);
            }
            if let Some(ip_str) = is_private_ip_detailed(host) {
                return Err(PermissionError::PrivateNetworkDenied(ip_str));
            }
            if let Some(ip_str) = is_ipv6_private(host) {
                return Err(PermissionError::IPv6PrivateDenied(ip_str));
            }
        }

        Ok(())
    }
}

fn is_localhost(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host == "[::1]"
        || host == "0.0.0.0"
        || host == "127.0.0.1"
        || host == "127.0.0.2"
        || host.starts_with("127.")
}

fn is_private_ip_detailed(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        if ip.is_loopback() {
            return Some(host.to_string());
        }
        if ip.is_private() {
            return Some(host.to_string());
        }
        if ip.is_link_local() {
            return Some(host.to_string());
        }
        if ip.is_broadcast() {
            return Some(host.to_string());
        }
        if ip.is_unspecified() {
            return Some(host.to_string());
        }
        if is_reserved_ipv4(&ip) {
            return Some(host.to_string());
        }
        return None;
    }

    if host.starts_with("192.168.") || host.starts_with("10.") {
        return Some(host.to_string());
    }

    if host.starts_with("172.") {
        if let Some(octet2) = host.split('.').nth(1) {
            if let Ok(o) = octet2.parse::<u8>() {
                if (16..=31).contains(&o) {
                    return Some(host.to_string());
                }
            }
        }
    }

    if host == "255.255.255.255" {
        return Some(host.to_string());
    }

    None
}

fn is_reserved_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    match octets[0] {
        0 => true,
        100 => (64..=127).contains(&octets[1]),
        169 => octets[1] == 254,
        192 => octets[1] == 0 && octets[2] == 0,
        198 => octets[1] == 18 || octets[1] == 19,
        203 => octets[1] == 0 && octets[2] == 113,
        _ => false,
    }
}

fn is_ipv6_private(host: &str) -> Option<String> {
    let addr_str = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };

    if let Ok(ip) = addr_str.parse::<std::net::Ipv6Addr>() {
        let segments = ip.segments();

        if segments[0] == 0xfe80 {
            return Some(host.to_string());
        }

        if (segments[0] & 0xfe00) == 0xfc00 {
            return Some(host.to_string());
        }

        if ip.is_loopback() {
            return Some(host.to_string());
        }

        if ip.is_unspecified() {
            return Some(host.to_string());
        }

        if segments[0] == 0xff00 {
            return Some(host.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_file_urls() {
        let p = NetworkPermissions::default();
        assert!(p.check("file:///etc/passwd").is_err());
    }

    #[test]
    fn blocks_data_uris() {
        let p = NetworkPermissions::default();
        assert!(p.check("data:text/html,<script>alert(1)</script>").is_err());
    }

    #[test]
    fn blocks_localhost() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://localhost:8080").is_err());
        assert!(p.check("http://127.0.0.1").is_err());
        assert!(p.check("http://127.0.0.2").is_err());
        assert!(p.check("http://[::1]").is_err());
    }

    #[test]
    fn blocks_private_ranges() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://192.168.1.1").is_err());
        assert!(p.check("http://10.0.0.1").is_err());
        assert!(p.check("http://172.16.0.1").is_err());
        assert!(p.check("http://172.31.255.255").is_err());
    }

    #[test]
    fn blocks_reserved_ipv4() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://100.64.0.1").is_err());
        assert!(p.check("http://169.254.1.1").is_err());
        assert!(p.check("http://192.0.0.1").is_err());
        assert!(p.check("http://198.18.0.1").is_err());
        assert!(p.check("http://198.19.0.1").is_err());
    }

    #[test]
    fn blocks_ipv6_link_local() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://[fe80::1]").is_err());
        assert!(p.check("http://[fe80::abcd:1234]").is_err());
    }

    #[test]
    fn blocks_ipv6_ula() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://[fc00::1]").is_err());
        assert!(p.check("http://[fd00::1]").is_err());
    }

    #[test]
    fn blocks_ipv6_loopback() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://[::1]").is_err());
    }

    #[test]
    fn blocks_ipv6_multicast() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://[ff02::1]").is_err());
    }

    #[test]
    fn allows_public_hosts() {
        let p = NetworkPermissions::default();
        assert!(p.check("https://example.com").is_ok());
        assert!(p.check("https://github.com").is_ok());
        assert!(p.check("https://docs.rust-lang.org").is_ok());
    }

    #[test]
    fn allows_public_ipv4() {
        let p = NetworkPermissions::default();
        assert!(p.check("http://8.8.8.8").is_ok());
        assert!(p.check("http://1.1.1.1").is_ok());
    }

    #[test]
    fn allow_private_opt_in() {
        let p = NetworkPermissions {
            allow_private: true,
            ..Default::default()
        };
        assert!(p.check("http://192.168.1.1").is_ok());
        assert!(p.check("http://10.0.0.1").is_ok());
    }

    #[test]
    fn allow_data_uri_opt_in() {
        let p = NetworkPermissions {
            allow_data_uri: true,
            ..Default::default()
        };
        assert!(p.check("data:text/html,hello").is_ok());
    }

    #[test]
    fn deny_specific_host() {
        let p = NetworkPermissions {
            extra_denied_hosts: vec!["evil.com".to_string()],
            ..Default::default()
        };
        assert!(p.check("https://evil.com").is_err());
        assert!(p.check("https://good.com").is_ok());
    }

    #[test]
    fn unsupported_scheme() {
        let p = NetworkPermissions::default();
        assert!(p.check("ftp://example.com").is_err());
        assert!(p.check("javascript:void(0)").is_err());
    }
}

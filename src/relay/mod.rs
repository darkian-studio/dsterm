pub mod agents;
pub mod clients;
pub mod crypto;
pub mod dispatch;
pub mod loopback;
pub mod pairing;
pub mod proxy_ws;
pub mod register;
pub mod sysmon;
pub mod terminal;
pub mod transport;
pub mod wire;

use url::Url;

/// Gate the relay base URL before any socket or register call is made.
///
/// The relay session is only as confidential as its transport: `http://`
/// (and its `ws://` upgrade) hands the frame stream to every network
/// observer, and the encryption inside the frames rests on one host-wide
/// shared key, so cleartext links in front of the relay expose pairing
/// traffic and full replay material to anyone on the path. The scheme is
/// therefore hard-gated: `https://`/`wss://` always; `http://`/`ws://`
/// only for loopback speakers (local relay development). Any other
/// scheme is a config error.
pub fn validated_relay_base(server_url: &str) -> anyhow::Result<String> {
    let base = server_url.trim_end_matches('/');
    let parsed = Url::parse(base)
        .map_err(|e| anyhow::anyhow!("relay server_url `{base}` is not a valid URL: {e}"))?;
    let cleartext = match parsed.scheme() {
        "https" | "wss" => false,
        "http" | "ws" => true,
        other => anyhow::bail!("relay server_url scheme `{other}://` is not http(s) or ws(s)"),
    };
    if cleartext && !is_loopback_host(parsed.host_str().unwrap_or("")) {
        anyhow::bail!(
            "relay server_url `{base}` uses cleartext {}:// for a non-loopback host; use https://",
            parsed.scheme()
        );
    }
    Ok(base.to_string())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "localhost" | "::1") || host.starts_with("127.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_wss_pass() {
        assert_eq!(
            validated_relay_base("https://relay.example.com/").unwrap(),
            "https://relay.example.com"
        );
        assert!(validated_relay_base("wss://relay.example.com").is_ok());
    }

    /// Cleartext stays legal for loopback relays - that is the local
    /// development story - and dies everywhere else.
    #[test]
    fn cleartext_allowed_only_on_loopback() {
        assert_eq!(
            validated_relay_base("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert!(validated_relay_base("http://127.0.0.1:9/").is_ok());
        assert!(validated_relay_base("ws://[::1]:3000").is_ok());
        assert!(validated_relay_base("http://relay.example.com").is_err());
        assert!(validated_relay_base("http://192.168.1.5:3000").is_err());
        assert!(validated_relay_base("ws://100.64.0.1").is_err());
    }

    #[test]
    fn foreign_schemes_and_junk_fail() {
        assert!(validated_relay_base("ftp://relay.example.com").is_err());
        assert!(validated_relay_base("javascript:alert(1)").is_err());
        assert!(validated_relay_base("not a url").is_err());
    }
}

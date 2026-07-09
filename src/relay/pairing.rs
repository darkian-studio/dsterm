use qrcode::{render::unicode, QrCode};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingPayload {
    #[serde(rename = "hostId")]
    pub host_id: String,
    #[serde(rename = "keyBase64")]
    pub key_base64: String,
}

impl PairingPayload {
    pub fn new(host_id: impl Into<String>, key_base64: impl Into<String>) -> anyhow::Result<Self> {
        let host_id = host_id.into();
        if host_id.trim().is_empty() {
            anyhow::bail!("hostId is required for pairing");
        }
        let key_base64 = key_base64.into();
        if key_base64.trim().is_empty() {
            anyhow::bail!("E2E key is required for pairing");
        }
        Ok(Self {
            host_id,
            key_base64,
        })
    }

    pub fn qr_text(&self) -> String {
        format!("{}:{}", self.host_id, self.key_base64)
    }
}

pub fn render_qr(payload: &PairingPayload) -> anyhow::Result<String> {
    let code = QrCode::new(payload.qr_text().as_bytes())?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

pub fn resolve_host_id(
    explicit: Option<&str>,
    host_id_file: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(host_id) = explicit {
        return normalize_host_id(host_id);
    }
    let Some(path) = host_id_file else {
        anyhow::bail!("hostId is required; pass --host-id or configure relay.host_id_file");
    };
    read_host_id(path)
}

pub fn read_host_id(path: impl AsRef<Path>) -> anyhow::Result<String> {
    normalize_host_id(fs::read_to_string(path)?.trim())
}

fn normalize_host_id(host_id: &str) -> anyhow::Result<String> {
    let host_id = host_id.trim();
    if host_id.is_empty() {
        anyhow::bail!("hostId is empty");
    }
    Ok(host_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_payload_matches_shellular_qr_shape() {
        let payload = PairingPayload::new("host_123", "abc=").unwrap();
        assert_eq!(payload.qr_text(), "host_123:abc=");
    }

    #[test]
    fn rejects_empty_host_id() {
        assert!(PairingPayload::new("", "abc=").is_err());
    }
}

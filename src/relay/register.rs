#![allow(dead_code)]

use serde_json::json;
use std::path::PathBuf;

fn default_host_id_file() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(home.join(".dsterm").join("host_id"))
}

fn host_id_path(configured: Option<&str>) -> anyhow::Result<PathBuf> {
    match configured {
        Some(path) => Ok(PathBuf::from(path)),
        None => default_host_id_file(),
    }
}

/// Read the persisted hostId, if present and non-empty.
pub fn read_cached(configured: Option<&str>) -> Option<String> {
    let path = host_id_path(configured).ok()?;
    let value = std::fs::read_to_string(path).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Register this host with the relay and persist the returned hostId.
pub async fn register_host(
    http: &reqwest::Client,
    server_url: &str,
    configured_file: Option<&str>,
    machine_id: &str,
) -> anyhow::Result<String> {
    let base = server_url.trim_end_matches('/');
    let url = format!("{base}/host/register");
    let body = json!({ "machineId": machine_id, "platform": std::env::consts::OS });
    let resp = http.post(url).json(&body).send().await?;
    let value = resp.json::<serde_json::Value>().await?;
    let host_id = value
        .get("hostId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("relay did not return a hostId"))?
        .to_string();

    let path = host_id_path(configured_file)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &host_id)?;
    Ok(host_id)
}

/// Best-effort stable machine identifier used in the relay handshake.
pub fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

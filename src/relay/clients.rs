#![allow(dead_code)]

use crate::config::UnknownClientPolicy;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClientApproval {
    Approved,
    Rejected,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRecord {
    #[serde(rename = "clientId")]
    pub client_id: String,
    pub platform: Option<String>,
    #[serde(rename = "appVersion")]
    pub app_version: Option<String>,
    pub device: Option<String>,
    #[serde(rename = "firstSeen")]
    pub first_seen: u64,
    #[serde(rename = "lastSeen")]
    pub last_seen: u64,
    pub approval: ClientApproval,
    /// This client's sealing key, bound at first authenticated use (#72).
    /// Absent = a legacy client that paired when the QR carried the shared
    /// master; those keep working through the master fallback until
    /// re-paired (or `dsterm clients unpin`).
    #[serde(
        rename = "identityKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub identity_key: Option<String>,
    #[serde(
        rename = "identityBoundAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub identity_bound_at: Option<u64>,
}

/// A key minted by `dsterm pair`, awaiting its holder's first approved
/// frame: at QR time the device has no relay `clientId` yet, so the pairing
/// key sits in this pool until decryption binds it to one (#72).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPairing {
    #[serde(rename = "keyBase64")]
    pub key_base64: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// FIFO cap: `dsterm pair` runs are rare, but the pool must not outlive
/// unbounded (same rule as every other map here).
const MAX_PENDING_PAIRINGS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientsFile {
    pub clients: BTreeMap<String, ClientRecord>,
    #[serde(rename = "pendingPairings", default)]
    pub pending_pairings: Vec<PendingPairing>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientInfo {
    pub platform: Option<String>,
    #[serde(rename = "appVersion")]
    pub app_version: Option<String>,
    pub device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Reject,
    Pending,
}

pub struct ClientStore {
    path: PathBuf,
    file: ClientsFile,
}

impl ClientStore {
    pub fn load_or_default(path: Option<&str>) -> anyhow::Result<Self> {
        let path = clients_path(path)?;
        let file = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            ClientsFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn decide(
        &mut self,
        client_id: &str,
        info: ClientInfo,
        unknown_policy: &UnknownClientPolicy,
    ) -> anyhow::Result<ApprovalDecision> {
        let now = unix_secs();
        if let Some(record) = self.file.clients.get_mut(client_id) {
            // Approval is anchored on a relay-supplied plaintext id, and
            // the stored fingerprint is the only strength that id has.
            // A join claiming an approved id while presenting a DIFFERENT
            // platform is a different device (appVersion/device churn
            // benignly between joins, platform does not): stop inheriting
            // approval and let the operator see the takeover attempt as a
            // fresh pending decision. The claimant's fingerprint is kept
            // so `dsterm clients list` shows who is claiming. Per-client
            // keys on the wire supersede this once the relay protocol
            // carries real client identity.
            let platform_changed = record.approval == ClientApproval::Approved
                && record.platform.is_some()
                && info.platform.is_some()
                && record.platform != info.platform;
            let stored_platform = record.platform.clone();
            record.last_seen = now;
            update_info(record, info.clone());
            let decision = if platform_changed {
                record.approval = ClientApproval::Pending;
                ApprovalDecision::Pending
            } else {
                match record.approval {
                    ClientApproval::Approved => ApprovalDecision::Allow,
                    ClientApproval::Rejected => ApprovalDecision::Reject,
                    ClientApproval::Pending => ApprovalDecision::Pending,
                }
            };
            if platform_changed {
                tracing::warn!(
                    "client {client_id} was approved as platform {:?} but now claims {:?}; demoted to pending",
                    stored_platform,
                    record.platform
                );
            }
            self.save()?;
            return Ok(decision);
        }

        let approval = match unknown_policy {
            UnknownClientPolicy::AlwaysAllow => ClientApproval::Approved,
            UnknownClientPolicy::AlwaysReject => ClientApproval::Rejected,
            UnknownClientPolicy::RequiresApproval => ClientApproval::Pending,
        };
        let decision = match approval {
            ClientApproval::Approved => ApprovalDecision::Allow,
            ClientApproval::Rejected => ApprovalDecision::Reject,
            ClientApproval::Pending => ApprovalDecision::Pending,
        };
        self.file.clients.insert(
            client_id.to_string(),
            ClientRecord {
                client_id: client_id.to_string(),
                platform: info.platform,
                app_version: info.app_version,
                device: info.device,
                first_seen: now,
                last_seen: now,
                approval,
                identity_key: None,
                identity_bound_at: None,
            },
        );
        self.save()?;
        Ok(decision)
    }

    /// Remember a pairing key until its holder's first frame binds it.
    pub fn add_pairing(&mut self, key_base64: String) -> anyhow::Result<()> {
        self.file.pending_pairings.push(PendingPairing {
            key_base64,
            created_at: unix_secs(),
        });
        while self.file.pending_pairings.len() > MAX_PENDING_PAIRINGS {
            self.file.pending_pairings.remove(0);
        }
        self.save()
    }

    /// Base64 pairing keys awaiting first use, oldest first.
    pub fn pending_keys(&self) -> Vec<String> {
        self.file
            .pending_pairings
            .iter()
            .map(|pairing| pairing.key_base64.clone())
            .collect()
    }

    pub fn pending_count(&self) -> usize {
        self.file.pending_pairings.len()
    }

    /// The key this client is pinned to, if any.
    pub fn identity_key(&self, client_id: &str) -> Option<String> {
        self.file
            .clients
            .get(client_id)
            .and_then(|record| record.identity_key.clone())
    }

    /// Bind the pairing key that decrypted this client's first frame.
    /// Returns whether the binding happened: a pinned client keeps its key
    /// until [`ClientStore::unpin`], so a later pairing race cannot steal
    /// the identity of an already-bound client.
    pub fn pin_identity(&mut self, client_id: &str, key_base64: &str) -> anyhow::Result<bool> {
        let Some(record) = self.file.clients.get_mut(client_id) else {
            anyhow::bail!("Unknown client: {client_id}");
        };
        if record.identity_key.is_some() {
            return Ok(false);
        }
        record.identity_key = Some(key_base64.to_string());
        record.identity_bound_at = Some(unix_secs());
        self.file
            .pending_pairings
            .retain(|pairing| pairing.key_base64 != key_base64);
        self.save()?;
        Ok(true)
    }

    /// Drop the pinned key so the next pairing can bind (rotation path:
    /// unpin, re-pair, first frame pins the new key).
    pub fn unpin(&mut self, client_id: &str) -> anyhow::Result<()> {
        let Some(record) = self.file.clients.get_mut(client_id) else {
            anyhow::bail!("Unknown client: {client_id}");
        };
        record.identity_key = None;
        record.identity_bound_at = None;
        self.save()
    }

    pub fn approve(&mut self, client_id: &str) -> anyhow::Result<()> {
        self.set_approval(client_id, ClientApproval::Approved)
    }

    pub fn reject(&mut self, client_id: &str) -> anyhow::Result<()> {
        self.set_approval(client_id, ClientApproval::Rejected)
    }

    pub fn list(&self) -> Vec<ClientRecord> {
        self.file.clients.values().cloned().collect()
    }

    fn set_approval(&mut self, client_id: &str, approval: ClientApproval) -> anyhow::Result<()> {
        let Some(record) = self.file.clients.get_mut(client_id) else {
            anyhow::bail!("Unknown client: {client_id}");
        };
        record.approval = approval;
        record.last_seen = unix_secs();
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self.file)?;
        fs::write(&self.path, content)?;
        // Pairing keys now live in this file, so it becomes owner-only on
        // unix. Windows keeps the default ACL: key-file protection there
        // lives in crypto.rs's `write_key_file` (#35) and covering this
        // file the same way is a parity gap, not silent exposure.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

pub fn clients_path(configured: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(path) = configured {
        return Ok(PathBuf::from(path));
    }
    Ok(dsterm_home()?.join("clients.json"))
}

fn dsterm_home() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(home.join(".dsterm"))
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn update_info(record: &mut ClientRecord, info: ClientInfo) {
    if info.platform.is_some() {
        record.platform = info.platform;
    }
    if info.app_version.is_some() {
        record.app_version = info.app_version;
    }
    if info.device.is_some() {
        record.device = info.device;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("dsterm-clients-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn unknown_client_requires_approval_by_default() {
        let path = temp_path();
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        let decision = store
            .decide(
                "client-a",
                ClientInfo::default(),
                &UnknownClientPolicy::RequiresApproval,
            )
            .unwrap();
        assert_eq!(decision, ApprovalDecision::Pending);
        assert_eq!(store.list()[0].approval, ClientApproval::Pending);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn approved_client_is_allowed_on_next_join() {
        let path = temp_path();
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        let _ = store
            .decide(
                "client-a",
                ClientInfo::default(),
                &UnknownClientPolicy::RequiresApproval,
            )
            .unwrap();
        store.approve("client-a").unwrap();
        let decision = store
            .decide(
                "client-a",
                ClientInfo::default(),
                &UnknownClientPolicy::RequiresApproval,
            )
            .unwrap();
        assert_eq!(decision, ApprovalDecision::Allow);
        let _ = fs::remove_file(path);
    }

    fn android_join(app_version: &str, device: &str) -> ClientInfo {
        ClientInfo {
            platform: Some("android".to_string()),
            app_version: Some(app_version.to_string()),
            device: Some(device.to_string()),
        }
    }

    /// Approval anchored on a relay-supplied plaintext id legally hinges on
    /// the stored fingerprint: an approved id re-joining with a different
    /// platform stops inheriting the approval.
    #[test]
    fn platform_change_on_approved_id_demotes_to_pending() {
        let path = temp_path();
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        let unknown = UnknownClientPolicy::RequiresApproval;

        store
            .decide("victim-id", android_join("1.13.0", "Pixel 8"), &unknown)
            .unwrap();
        store.approve("victim-id").unwrap();
        assert_eq!(
            store
                .decide("victim-id", android_join("1.13.1", "Pixel 8"), &unknown)
                .unwrap(),
            ApprovalDecision::Allow
        );

        // Same id, different platform: no silent inheritance. Appversion or
        // device churn alone never triggers this (both are updated in place).
        let attacker = ClientInfo {
            platform: Some("ios".to_string()),
            app_version: Some("1.13.1".to_string()),
            device: Some("iPhone 15".to_string()),
        };
        assert_eq!(
            store.decide("victim-id", attacker, &unknown).unwrap(),
            ApprovalDecision::Pending
        );
        let record = &store.list()[0];
        assert_eq!(record.approval, ClientApproval::Pending);
        // The record keeps the claimant's fingerprint for operator review.
        assert_eq!(record.platform.as_deref(), Some("ios"));

        // Still pending until the operator decides, no oscillation.
        assert_eq!(
            store
                .decide("victim-id", android_join("1.13.1", "Pixel 8"), &unknown)
                .unwrap(),
            ApprovalDecision::Pending
        );
        store.approve("victim-id").unwrap();
        assert_eq!(
            store
                .decide("victim-id", android_join("1.13.1", "Pixel 8"), &unknown)
                .unwrap(),
            ApprovalDecision::Allow
        );

        let _ = fs::remove_file(path);
    }

    fn pairing_key(seed: u8) -> String {
        crate::relay::crypto::Secretbox::from_key_bytes([seed; 32]).key_base64()
    }

    /// #72 pairing lifecycle: mint -> first bind takes -> the key leaves the
    /// pool -> a second pin cannot steal the identity -> unpin frees it.
    #[test]
    fn pairing_binds_once_then_unpin_frees() {
        let path = temp_path();
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        let unknown = UnknownClientPolicy::RequiresApproval;
        let key = pairing_key(3);
        let other = pairing_key(4);

        store.add_pairing(key.clone()).unwrap();
        assert_eq!(store.pending_count(), 1);
        assert_eq!(store.pending_keys(), vec![key.clone()]);

        // Binding needs an existing record; approval comes first in prod.
        store
            .decide("client-a", ClientInfo::default(), &unknown)
            .unwrap();
        assert!(store.pin_identity("client-a", &key).unwrap());
        assert_eq!(
            store.identity_key("client-a").as_deref(),
            Some(key.as_str())
        );
        assert_eq!(store.pending_count(), 0);

        // Re-pairing must not rebind an already-pinned client.
        assert!(!store.pin_identity("client-a", &other).unwrap());
        assert_eq!(
            store.identity_key("client-a").as_deref(),
            Some(key.as_str())
        );

        store.unpin("client-a").unwrap();
        assert!(store.identity_key("client-a").is_none());

        // Unknown client is an error, not a silent write.
        assert!(store.pin_identity("ghost", &other).is_err());
        assert!(store.unpin("ghost").is_err());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn pairing_pool_is_capped_fifo() {
        let path = temp_path();
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        for seed in 0..10u8 {
            store.add_pairing(pairing_key(seed)).unwrap();
        }
        let keys = store.pending_keys();
        assert_eq!(store.pending_count(), 8);
        // Oldest pairings evicted first, newest survive.
        assert_eq!(keys.first().unwrap(), &pairing_key(2));
        assert_eq!(keys.last().unwrap(), &pairing_key(9));
        let _ = fs::remove_file(path);
    }

    /// A pre-#72 clients.json (no identity fields, no pairing pool) parses
    /// unchanged and defaults the new state.
    #[test]
    fn legacy_clients_file_parses_without_identity_fields() {
        let path = temp_path();
        fs::write(
            &path,
            r#"{"clients":{"old":{"clientId":"old","platform":null,"appVersion":null,"device":null,"firstSeen":1,"lastSeen":2,"approval":"approved"}}}"#,
        )
        .unwrap();
        let store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(store.pending_count(), 0);
        let record = &store.list()[0];
        assert_eq!(record.client_id, "old");
        assert!(record.identity_key.is_none());
        assert_eq!(record.approval, ClientApproval::Approved);
        let _ = fs::remove_file(path);
    }
}

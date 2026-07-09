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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientsFile {
    pub clients: BTreeMap<String, ClientRecord>,
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
            record.last_seen = now;
            update_info(record, info);
            let decision = match record.approval {
                ClientApproval::Approved => ApprovalDecision::Allow,
                ClientApproval::Rejected => ApprovalDecision::Reject,
                ClientApproval::Pending => ApprovalDecision::Pending,
            };
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
            },
        );
        self.save()?;
        Ok(decision)
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
}

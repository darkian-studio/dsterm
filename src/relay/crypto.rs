#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use crypto_secretbox::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Key, Nonce, XSalsa20Poly1305,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone)]
pub struct Secretbox {
    key: [u8; KEY_BYTES],
}

impl Secretbox {
    pub fn load_or_create(path: Option<&str>) -> anyhow::Result<Self> {
        let path = key_path(path)?;
        // Atomic TOCTOU-safe: try load, then create_new, retry on AlreadyExists
        match Self::load(&path) {
            Ok(s) => return Ok(s),
            Err(e) => {
                // Only retry create if file truly missing (NotFound)
                if let Some(ioe) = e.downcast_ref::<std::io::Error>() {
                    if ioe.kind() != std::io::ErrorKind::NotFound {
                        // Unexpected load error (e.g., permission) — propagate
                        // unless file simply doesn't exist
                        if path.exists() {
                            return Err(e);
                        }
                    }
                } else if path.exists() {
                    return Err(e);
                }
            }
        }
        match Self::create(&path) {
            Ok(s) => Ok(s),
            Err(e) => {
                // Race: another process created between our load and create
                if let Some(ioe) = e.downcast_ref::<std::io::Error>() {
                    if ioe.kind() == std::io::ErrorKind::AlreadyExists {
                        return Self::load(&path);
                    }
                }
                // For write_key_file's inner OpenOptions error wrapped via `?`, downcast may be nested.
                // Fallback: if file now exists, try load once more.
                if path.exists() {
                    if let Ok(s) = Self::load(&path) {
                        return Ok(s);
                    }
                }
                Err(e)
            }
        }
    }

    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = fs::read(path)?;
        let key: [u8; KEY_BYTES] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid secretbox key length"))?;
        Ok(Self { key })
    }

    pub fn create(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let key = XSalsa20Poly1305::generate_key(&mut OsRng);
        let key_bytes: [u8; KEY_BYTES] = key.as_slice().try_into()?;
        write_key_file(path, &key_bytes)?;
        Ok(Self { key: key_bytes })
    }

    pub fn key_base64(&self) -> String {
        BASE64.encode(self.key)
    }

    pub fn encrypt(&self, plaintext: impl AsRef<[u8]>) -> anyhow::Result<EncryptedPayload> {
        let cipher = self.cipher();
        let nonce = XSalsa20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_ref())
            .map_err(|_| anyhow::anyhow!("Secretbox encryption failed"))?;
        Ok(EncryptedPayload {
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(ciphertext),
        })
    }

    pub fn decrypt(&self, payload: &EncryptedPayload) -> anyhow::Result<Vec<u8>> {
        self.decrypt_parts(&payload.nonce, &payload.ciphertext)
    }

    pub fn decrypt_parts(&self, nonce: &str, ciphertext: &str) -> anyhow::Result<Vec<u8>> {
        let nonce = BASE64.decode(nonce)?;
        if nonce.len() != NONCE_BYTES {
            anyhow::bail!("Invalid secretbox nonce length");
        }
        let ciphertext = BASE64.decode(ciphertext)?;
        let cipher = self.cipher();
        cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| anyhow::anyhow!("Secretbox decryption failed"))
    }

    fn cipher(&self) -> XSalsa20Poly1305 {
        XSalsa20Poly1305::new(Key::from_slice(&self.key))
    }
}

pub fn key_path(configured: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(path) = configured {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(home
        .join(".dsterm")
        .join(format!("dsterm-{}.e2ee", machine_id())))
}

#[cfg(windows)]
pub(crate) fn windows_computer_name() -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

    // The documented maximum is much smaller, but leave room for unusual host names.
    let mut buffer = vec![0u16; 256];
    let mut len = buffer.len() as u32;
    if unsafe { GetComputerNameW(buffer.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    String::from_utf16(&buffer[..len as usize]).ok()
}

#[cfg(windows)]
fn machine_id() -> String {
    [
        windows_computer_name(),
        std::env::var("COMPUTERNAME").ok(),
        std::env::var("HOSTNAME").ok(),
    ]
    .into_iter()
    .flatten()
    .map(|value| sanitize_id(value.trim()))
    .find(|value| !value.is_empty())
    .unwrap_or_else(|| "default".to_string())
}

#[cfg(not(windows))]
fn machine_id() -> String {
    let candidates = [
        fs::read_to_string("/etc/machine-id").ok(),
        fs::read_to_string("/proc/sys/kernel/hostname").ok(),
        std::env::var("HOSTNAME").ok(),
        std::env::var("COMPUTERNAME").ok(),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|value| sanitize_id(value.trim()))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(unix)]
fn write_key_file(path: &Path, key: &[u8; KEY_BYTES]) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    std::io::Write::write_all(&mut options.open(path)?, key)?;
    Ok(())
}

#[cfg(windows)]
fn write_key_file(path: &Path, key: &[u8; KEY_BYTES]) -> anyhow::Result<()> {
    // The default path lives under USERPROFILE, whose inherited NTFS DACL is
    // user-restricted. Configured paths may use a different ACL, so this is a
    // best-effort Windows equivalent of the Unix 0600 creation mode.
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    std::io::Write::write_all(&mut options.open(path)?, key)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn write_key_file(path: &Path, key: &[u8; KEY_BYTES]) -> anyhow::Result<()> {
    fs::write(path, key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let secretbox = Secretbox {
            key: [7; KEY_BYTES],
        };
        let payload = secretbox.encrypt(b"hello").unwrap();
        let plaintext = secretbox.decrypt(&payload).unwrap();
        assert_eq!(plaintext, b"hello");
    }

    #[test]
    fn key_base64_is_shellular_compatible_length() {
        let secretbox = Secretbox {
            key: [1; KEY_BYTES],
        };
        assert_eq!(secretbox.key_base64().len(), 44);
    }

    #[test]
    fn create_and_load_key_file() {
        let path = std::env::temp_dir().join(format!("dsterm-test-{}.e2ee", uuid::Uuid::new_v4()));
        let created = Secretbox::create(&path).unwrap();
        let loaded = Secretbox::load(&path).unwrap();
        assert_eq!(created.key_base64(), loaded.key_base64());
        let _ = fs::remove_file(path);
    }
}

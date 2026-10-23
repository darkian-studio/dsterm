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

    /// Wrap key material held elsewhere: a per-client identity key out of
    /// `clients.json`, or a freshly minted pairing key (#72).
    pub fn from_key_bytes(key: [u8; KEY_BYTES]) -> Self {
        Self { key }
    }

    /// Decode a standard-base64 32-byte key as stored in the `identityKey`
    /// and `pendingPairings` fields of `clients.json`.
    pub fn from_base64_key(key_base64: &str) -> anyhow::Result<Self> {
        let bytes = BASE64.decode(key_base64.trim())?;
        let key: [u8; KEY_BYTES] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid secretbox key length"))?;
        Ok(Self { key })
    }

    /// A fresh random key. `dsterm pair` mints one of these per pairing so
    /// a QR carries that device's key alone, never the shared master.
    pub fn random_key() -> [u8; KEY_BYTES] {
        let key = XSalsa20Poly1305::generate_key(&mut OsRng);
        key.as_slice()
            .try_into()
            .expect("generate_key always yields KEY_BYTES")
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
    use anyhow::Context;
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let file = options.open(path)?;

    // Write an empty file first and only persist the key material after the
    // DACL is locked down: if the ACL step fails (unsupported filesystem,
    // missing rights), the empty file is removed and nothing secret was ever
    // persisted with weak permissions.
    if let Err(harden_err) = windows_acl::protect(file.as_raw_handle(), path) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(
            harden_err.context("relay key file DACL could not be restricted; key file not written")
        );
    }

    (&file).write_all(key)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn write_key_file(path: &Path, key: &[u8; KEY_BYTES]) -> anyhow::Result<()> {
    fs::write(path, key)?;
    Ok(())
}

/// Windows equivalent of the Unix 0600 creation mode: replaces the file's
/// DACL with a PROTECTED one (inheritance broken) granting full control to
/// the file owner, SYSTEM, and BUILTIN\Administrators - the same three-ACE
/// shape OpenSSH on Windows uses for private keys. Everything else,
/// including other interactive users, is excluded. Applied at creation only,
/// mirroring the Unix side's create-time mode.
///
/// The FFI path exists because the std OpenOptionsExt has no
/// SECURITY_ATTRIBUTES support, so the descriptor must be adjusted after
/// the file is created. Unlike the Unix mode bit (atomic in create), that
/// leaves a create-then-restrict window; the caller mitigates it by
/// deleting the still-empty file when this fails.
#[cfg(windows)]
mod windows_acl {
    use anyhow::anyhow;
    use core::mem::size_of;
    use core::slice;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, GENERIC_ALL, HANDLE, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, CreateWellKnownSid, GetLengthSid, InitializeAcl,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, WELL_KNOWN_SID_TYPE,
    };

    pub fn protect(handle: HANDLE, path: &std::path::Path) -> anyhow::Result<()> {
        // Read back the owner the filesystem assigned at creation; granting
        // that exact SID sidesteps process-token plumbing (OpenProcessToken
        // is not exposed by windows-sys).
        let mut owner_sid: PSID = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let fetch_status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner_sid,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if fetch_status != ERROR_SUCCESS {
            return Err(anyhow!("GetSecurityInfo failed with {:#x}", fetch_status));
        }
        // The returned SID points inside the LocalFree-managed descriptor;
        // copy it out before freeing.
        let owner_len = unsafe { GetLengthSid(owner_sid) };
        if owner_len == 0 {
            unsafe { LocalFree(sd as HLOCAL) };
            return Err(anyhow!("owner SID from GetSecurityInfo has no length"));
        }
        let owner_sid_bytes =
            unsafe { slice::from_raw_parts(owner_sid.cast::<u8>(), owner_len as usize).to_vec() };
        unsafe { LocalFree(sd as HLOCAL) };

        let system_sid = builtin_sid(WinLocalSystemSid)?;
        let admin_sid = builtin_sid(WinBuiltinAdministratorsSid)?;
        build_and_apply(path, &owner_sid_bytes, &system_sid, &admin_sid)
    }

    fn builtin_sid(kind: WELL_KNOWN_SID_TYPE) -> anyhow::Result<Vec<u8>> {
        let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut len = sid.len() as u32;
        let created = unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if created == 0 {
            return Err(anyhow!("CreateWellKnownSid failed with error {}", unsafe {
                windows_sys::Win32::Foundation::GetLastError()
            }));
        }
        if len as usize > sid.len() {
            return Err(anyhow!("well-known SID exceeded the max size"));
        }
        Ok(sid)
    }

    fn build_and_apply(
        path: &std::path::Path,
        owner_sid: &[u8],
        system_sid: &[u8],
        admin_sid: &[u8],
    ) -> anyhow::Result<()> {
        // One ACCESS_ALLOWED ACE per trustee. The API reserves room for the
        // SID after the struct; SidStart is a u32 placeholder whose offset
        // marks where the raw SID bytes begin, so the real ACE size is
        // (SidStart offset) + sid length.
        let ace_base = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
        let acl_size =
            size_of::<ACL>() + 3 * ace_base + owner_sid.len() + system_sid.len() + admin_sid.len();

        // Aligned to u32, not to the byte a Vec<u8> guarantees: ACL and SID
        // fields downstream keep u16/u32 members.
        let mut acl_bytes = vec![0u32; (acl_size + size_of::<u32>() - 1) / size_of::<u32>()];
        let acl = acl_bytes.as_mut_ptr().cast::<ACL>();
        let acl_ok = unsafe {
            InitializeAcl(
                acl,
                (acl_bytes.len() * size_of::<u32>()) as u32,
                ACL_REVISION,
            )
        };
        if acl_ok == 0 {
            return Err(anyhow!("InitializeAcl failed with error {}", unsafe {
                windows_sys::Win32::Foundation::GetLastError()
            }));
        }

        for sid in [owner_sid, system_sid, admin_sid] {
            let added = unsafe {
                AddAccessAllowedAceEx(
                    acl,
                    ACL_REVISION,
                    0, // no inheritance flags: files carry their own ACLs
                    GENERIC_ALL,
                    sid.as_ptr() as PSID,
                )
            };
            if added == 0 {
                return Err(anyhow!(
                    "AddAccessAllowedAceEx failed with error {}",
                    unsafe { windows_sys::Win32::Foundation::GetLastError() }
                ));
            }
        }

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(core::iter::once(0))
            .collect();
        let applied = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null_mut(),
            )
        };
        if applied != ERROR_SUCCESS {
            return Err(anyhow!("SetNamedSecurityInfoW failed with {:#x}", applied));
        }
        Ok(())
    }
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

    #[test]
    fn base64_key_roundtrip() {
        let secretbox = Secretbox {
            key: [9; KEY_BYTES],
        };
        let decoded = Secretbox::from_base64_key(&secretbox.key_base64()).unwrap();
        assert_eq!(decoded.key, secretbox.key);
    }

    #[test]
    fn base64_key_rejects_wrong_length_and_garbage() {
        let short = BASE64.encode([0u8; 16]);
        assert!(Secretbox::from_base64_key(&short).is_err());
        assert!(Secretbox::from_base64_key("not base64 !").is_err());
    }

    #[test]
    fn random_keys_are_fresh_and_roundtrip() {
        let a = Secretbox::random_key();
        let b = Secretbox::random_key();
        assert_eq!(a.len(), KEY_BYTES);
        assert_ne!(a, b);
        // The minted key must survive the clients.json encoding it is
        // stored through (#72).
        let sealed = Secretbox::from_key_bytes(a);
        assert_eq!(
            Secretbox::from_base64_key(&sealed.key_base64())
                .unwrap()
                .key,
            a
        );
    }
}

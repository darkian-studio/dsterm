use minisign_verify::{PublicKey, Signature};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
const GITHUB_API_URL: &str = "https://api.github.com/repos/darkian-studio/dsterm/releases/latest";
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_FILE: &str = ".dsterm_update_cache";
/// The release signing key, embedded at compile time. The matching secret
/// lives only in the `MINISIGN_SECRET_KEY` GitHub Actions secret that
/// `release.yml` signs every asset with (#73). Rotating means replacing
/// this file, re-issuing a release, and moving the secret — the
/// `generate_release_keypair` test is the documented minter.
const MINISIGN_PUBLIC_KEY: &str = include_str!("../minisign.pub");

/// SHA-256 of `bytes` as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parse a checksum asset (`<hex> [filename]`) and confirm it covers
/// `bytes`. Anything that is not exactly 64 hex characters, or a
/// mismatch, is a hard error — #73: never skip verification.
fn verify_checksum_body(
    checksum_body: &str,
    bytes: &[u8],
    asset_name: &str,
) -> Result<String, String> {
    let expected = checksum_body
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "Malformed checksum asset {asset_name}: expected 64 hex characters"
        ));
    }
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(format!(
            "Checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        ));
    }
    Ok(actual)
}

/// Verify a `.minisig` asset against the compile-time-embedded public
/// key. `allow_legacy` stays false: signatures this old format would
/// accept are not something a dsterm release has ever produced.
fn verify_minisign(sig_text: &str, bytes: &[u8], pubkey_text: &str) -> Result<(), String> {
    let public_key = PublicKey::decode(pubkey_text)
        .map_err(|e| format!("embedded minisign public key is unusable: {e}"))?;
    let signature =
        Signature::decode(sig_text).map_err(|e| format!("malformed minisign signature: {e}"))?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|e| format!("minisign signature rejected: {e}"))
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

struct UpdateCache {
    last_check: SystemTime,
    latest_version: String,
}

pub struct FetchedUpdate {
    pub version: String,
    pub bytes: Vec<u8>,
    pub sha256_hex: String,
}

pub struct StagedUpdate {
    pub version: String,
    pub path: PathBuf,
}

#[derive(Deserialize)]
struct StagedMeta {
    version: String,
    sha256_hex: String,
    size: u64,
}

pub struct UpdateChecker {
    client: Client,
    current_version: Version,
}

impl UpdateChecker {
    pub fn new(current_version: &str) -> Self {
        Self {
            client: Client::new(),
            current_version: Version::parse(current_version).unwrap(),
        }
    }

    async fn get_cache() -> Option<UpdateCache> {
        let cache_path = Self::get_cache_path()?;
        let content = fs::read_to_string(cache_path).await.ok()?;
        let parts: Vec<&str> = content.split(',').collect();
        if parts.len() != 2 {
            return None;
        }

        let timestamp = parts[0].parse::<u64>().ok()?;
        Some(UpdateCache {
            last_check: SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp),
            latest_version: parts[1].to_string(),
        })
    }

    async fn save_cache(version: &str) -> tokio::io::Result<()> {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let content = format!("{now},{version}");
            let tmp = cache_path.with_extension("tmp");
            fs::write(&tmp, content).await?;
            fs::rename(&tmp, &cache_path).await?;
        }
        Ok(())
    }

    fn get_cache_path() -> Option<PathBuf> {
        match std::env::var_os("HOME") {
            Some(home) => {
                let mut path = PathBuf::from(home);
                path.push(".cache");
                path.push("dsterm");
                path.push(CACHE_FILE);
                Some(path)
            }
            None => match std::env::var_os("TMPDIR").or_else(|| std::env::var_os("TMP")) {
                Some(tmp) => {
                    let mut path = PathBuf::from(tmp);
                    path.push("dsterm");
                    path.push(CACHE_FILE);
                    Some(path)
                }
                None => None,
            },
        }
    }

    pub async fn check_update(
        &self,
        force: bool,
    ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        if !force {
            if let Some(cache) = Self::get_cache().await {
                let elapsed = SystemTime::now()
                    .duration_since(cache.last_check)
                    .unwrap_or(UPDATE_CHECK_INTERVAL);
                if elapsed < UPDATE_CHECK_INTERVAL {
                    let cached_version = Version::parse(&cache.latest_version)?;
                    if cached_version > self.current_version {
                        return Ok(Some(cache.latest_version));
                    }
                    return Ok(None);
                }
            }
        }

        let release: GithubRelease = self
            .client
            .get(GITHUB_API_URL)
            .header("User-Agent", "dsterm-update-checker")
            .send()
            .await?
            .json()
            .await?;

        let latest_version = Version::parse(release.tag_name.trim_start_matches('v'))?;
        Self::save_cache(&latest_version.to_string()).await?;

        if latest_version > self.current_version {
            Ok(Some(release.tag_name))
        } else {
            Ok(None)
        }
    }

    /// Downloads and verifies the latest release asset for
    /// this platform/arch WITHOUT touching the installed
    /// binary or staging anything. Same checks the in-place
    /// updater applies (size, sha256, minisign signature,
    /// magic bytes) — shared by `update` and `--self-update`
    /// so staging can never be weaker than replacing. Every
    /// check fails closed (#73): a release missing any of them
    /// aborts the update instead of skipping verification.
    pub async fn fetch_update(
        &self,
    ) -> Result<FetchedUpdate, Box<dyn std::error::Error + Send + Sync>> {
        let release: GithubRelease = self
            .client
            .get(GITHUB_API_URL)
            .header("User-Agent", "dsterm-update-checker")
            .send()
            .await?
            .json()
            .await?;

        // The target OS is baked into the binary at compile
        // time; read it directly instead of probing the
        // filesystem. A `/data/data/...` probe resolves to
        // `C:\data\data\...` on Windows and mis-detects
        // Android.
        let platform = match std::env::consts::OS {
            "android" => "android",
            "linux" => {
                // A linux-target binary running inside Termux
                // still needs the Android assets;
                // TERMUX_VERSION is the concrete signal Termux
                // always sets (never a path guess).
                if std::env::var("TERMUX_VERSION").is_ok() {
                    "android"
                } else {
                    "linux"
                }
            }
            os @ ("macos" | "windows") => os,
            other => return Err(format!("Unsupported OS: {other}").into()),
        };

        let arch_suffix = match std::env::consts::ARCH {
            "arm" => "armv7",
            "aarch64" => "arm64",
            "x86_64" => "x86_64",
            other => return Err(format!("Unsupported architecture: {other}").into()),
        };

        let ext = if platform == "windows" { ".exe" } else { "" };
        let binary_name = format!("dsterm-{platform}-{arch_suffix}{ext}");

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == binary_name)
            .ok_or_else(|| format!("No matching binary found for {binary_name}"))?;

        let response = self
            .client
            .get(&asset.browser_download_url)
            .send()
            .await?
            .bytes()
            .await?;

        if response.len() as u64 != asset.size {
            return Err(format!(
                "Size mismatch for {binary_name}: expected {} bytes, got {}. \
                 Aborting update; the installed binary was NOT touched.",
                asset.size,
                response.len()
            )
            .into());
        }

        // #73 fail closed: a release without a published checksum is a
        // broken release. The old behavior silently skipped this check.
        let checksum_name = format!("{binary_name}.sha256");
        let checksum_asset = release
            .assets
            .iter()
            .find(|a| a.name == checksum_name)
            .ok_or_else(|| {
                format!(
                    "Release {} publishes no {checksum_name} (fail closed); refusing to update. \
                     The installed binary was NOT touched.",
                    release.tag_name
                )
            })?;
        let checksum_body = self
            .client
            .get(&checksum_asset.browser_download_url)
            .send()
            .await?
            .text()
            .await?;
        let sha256_hex = verify_checksum_body(&checksum_body, &response, &checksum_name)
            .map_err(|e| format!("{e}. Aborting update; the installed binary was NOT touched."))?;

        // #73 fail closed: the same release must carry a minisign
        // signature over the binary, made by the embedded key.
        let sig_name = format!("{binary_name}.minisig");
        let sig_asset = release
            .assets
            .iter()
            .find(|a| a.name == sig_name)
            .ok_or_else(|| {
                format!(
                    "Release {} publishes no {sig_name} (fail closed); refusing to update. \
                     The installed binary was NOT touched.",
                    release.tag_name
                )
            })?;
        let sig_body = self
            .client
            .get(&sig_asset.browser_download_url)
            .send()
            .await?
            .text()
            .await?;
        verify_minisign(&sig_body, &response, MINISIGN_PUBLIC_KEY).map_err(|e| {
            format!("{sig_name}: {e}. Aborting update; the installed binary was NOT touched.")
        })?;

        // Independent of both: this is the exact
        // failure already observed (an asset named
        // dsterm-windows-x86_64.exe served ELF bytes).
        // Confirm the payload's magic number matches the
        // platform before it goes near current_exe.
        if platform == "windows" && !response.starts_with(b"MZ") {
            return Err(format!(
                "{binary_name} does not have a Windows PE header (expected 'MZ'). \
                 Aborting update; the installed binary was NOT touched."
            )
            .into());
        }

        Ok(FetchedUpdate {
            version: release.tag_name.trim_start_matches('v').to_string(),
            bytes: response.to_vec(),
            sha256_hex,
        })
    }

    async fn activate(bytes: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let current_exe = std::env::current_exe()?;
        let temp_path = current_exe.with_extension("new");

        let mut file = File::create(&temp_path).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&temp_path).await?;
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&temp_path, perms).await?;
        }

        #[cfg(windows)]
        {
            let old_path = current_exe.with_extension("old");
            let _ = fs::remove_file(&old_path).await;
            fs::rename(&current_exe, &old_path).await?;
            fs::rename(&temp_path, &current_exe).await?;
        }
        #[cfg(not(windows))]
        {
            fs::rename(&temp_path, &current_exe).await?;
        }

        if let Some(cache_path) = Self::get_cache_path() {
            let _ = fs::remove_file(cache_path).await;
        }

        Ok(())
    }

    pub async fn update(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let fetched = self.fetch_update().await?;
        Self::activate(&fetched.bytes).await
    }

    fn binary_file_name() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        std::env::current_exe()?
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .ok_or_else(|| "Cannot determine binary file name".into())
    }

    pub async fn stage_update(
        fetched: &FetchedUpdate,
    ) -> Result<StagedUpdate, Box<dyn std::error::Error + Send + Sync>> {
        let current_exe = std::env::current_exe()?;
        let dir = current_exe
            .parent()
            .ok_or("Cannot determine binary directory")?;
        let bin_name = Self::binary_file_name()?;
        let new_path = dir.join(format!("{bin_name}.new"));
        let meta_path = dir.join(format!("{bin_name}.new.meta.json"));
        let temp_path = dir.join(format!(".{bin_name}.update-{}.tmp", std::process::id()));

        let staged: Result<StagedUpdate, Box<dyn std::error::Error + Send + Sync>> = async {
            let mut file = File::create(&temp_path).await?;
            file.write_all(&fetched.bytes).await?;
            file.sync_all().await?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = fs::metadata(&temp_path).await?;
                let mut perms = metadata.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&temp_path, perms).await?;
            }

            fs::rename(&temp_path, &new_path).await?;

            let meta = json!({
                "version": fetched.version,
                "sha256_hex": fetched.sha256_hex,
                "size": fetched.bytes.len() as u64,
                "staged_at": SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            fs::write(&meta_path, meta.to_string()).await?;

            Ok(StagedUpdate {
                version: fetched.version.clone(),
                path: new_path.clone(),
            })
        }
        .await;

        if staged.is_err() {
            let _ = fs::remove_file(&temp_path).await;
            let _ = fs::remove_file(&new_path).await;
            let _ = fs::remove_file(&meta_path).await;
        }
        staged
    }

    pub async fn staged_update() -> Option<StagedUpdate> {
        let current_exe = std::env::current_exe().ok()?;
        let dir = current_exe.parent()?;
        let bin_name = current_exe.file_name().and_then(|n| n.to_str())?;
        let new_path = dir.join(format!("{bin_name}.new"));
        let meta_path = dir.join(format!("{bin_name}.new.meta.json"));
        let meta_raw = fs::read_to_string(&meta_path).await.ok()?;
        let meta: StagedMeta = serde_json::from_str(&meta_raw).ok()?;
        let on_disk = fs::read(&new_path).await.ok()?;
        if on_disk.len() as u64 != meta.size {
            return None;
        }
        if sha256_hex(&on_disk) != meta.sha256_hex {
            return None;
        }
        Some(StagedUpdate {
            version: meta.version,
            path: new_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-off release-key minter (and the documented rotation tool):
    /// `cargo test generate_release_keypair -- --ignored --nocapture`
    /// prints the two armored boxes; the public box becomes the committed
    /// `minisign.pub`, the secret box goes into the `MINISIGN_SECRET_KEY`
    /// GitHub secret (never into this repo).
    #[test]
    #[ignore = "run manually to mint or rotate the release signing key"]
    fn generate_release_keypair() {
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = keypair.pk.to_box().unwrap().to_string();
        let sk_box = keypair.sk.to_box(None).unwrap().to_string();
        println!("===PUBLIC-KEY-BEGIN===");
        println!("{pk_box}");
        println!("===PUBLIC-KEY-END===");
        println!("===SECRET-KEY-BEGIN===");
        println!("{sk_box}");
        println!("===SECRET-KEY-END===");
    }

    /// Guards the embedded key itself: a mangled `minisign.pub` must not
    /// silently reach a build where every update would then fail closed.
    #[test]
    fn embedded_public_key_parses() {
        PublicKey::decode(MINISIGN_PUBLIC_KEY).expect("minisign.pub must stay parseable");
    }

    #[test]
    fn checksum_body_accepts_match_and_normalizes_case() {
        let bytes = b"release bytes";
        let good = sha256_hex(bytes);
        assert_eq!(
            verify_checksum_body(&format!("{good}  dsterm-linux-x86_64\n"), bytes, "x.sha256")
                .unwrap(),
            good
        );
        assert_eq!(
            verify_checksum_body(&good.to_uppercase(), bytes, "x.sha256").unwrap(),
            good
        );
    }

    #[test]
    fn checksum_body_fails_closed_on_mismatch_and_garbage() {
        let bytes = b"release bytes";
        let wrong = sha256_hex(b"something else");
        let err = verify_checksum_body(&format!("{wrong}  x"), bytes, "x.sha256").unwrap_err();
        assert!(err.contains("mismatch"), "unexpected error: {err}");

        // Malformed assets are errors too, never a skipped check.
        for bad in ["", "deadbeef", "not a checksum", "zzzz...x"] {
            let err = verify_checksum_body(bad, bytes, "x.sha256").unwrap_err();
            assert!(
                err.contains("Malformed"),
                "unexpected error for {bad:?}: {err}"
            );
        }
    }

    /// End-to-end roundtrip with an ephemeral keypair: sign bytes, verify
    /// with that key's box — the exact code path `fetch_update` runs
    /// against `minisign.pub`, minus the committed key.
    #[test]
    fn minisign_roundtrip_rejects_tampering() {
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = keypair.pk.to_box().unwrap().to_string();
        let signature_box = minisign::sign(
            None,
            &keypair.sk,
            std::io::Cursor::new(b"release bytes"),
            None,
            None,
        )
        .unwrap();
        let sig_text = signature_box.into_string();

        verify_minisign(&sig_text, b"release bytes", &pk_box).unwrap();

        // Tampered payload.
        let err = verify_minisign(&sig_text, b"release byteX", &pk_box).unwrap_err();
        assert!(err.contains("rejected"), "unexpected error: {err}");

        // Tampered signature: corrupt one base64 char of the signature
        // line itself (byte id + sig payload), length preserved so it
        // still decodes — verification must reject it.
        let mut lines: Vec<String> = sig_text.lines().map(str::to_string).collect();
        assert!(lines.len() >= 2, "signature box lost its key line");
        let sig_line = &mut lines[1];
        let pos = sig_line.len() / 2;
        let old = sig_line.as_bytes()[pos];
        let new = if old == b'A' { b'B' } else { b'A' };
        sig_line.replace_range(pos..pos + 1, &(new as char).to_string());
        let flipped = lines.join("\n");
        assert!(verify_minisign(&flipped, b"release bytes", &pk_box).is_err());

        // Signed with a different key than the one embedded.
        let other_pk = minisign::KeyPair::generate_unencrypted_keypair()
            .unwrap()
            .pk
            .to_box()
            .unwrap()
            .to_string();
        assert!(verify_minisign(&sig_text, b"release bytes", &other_pk).is_err());

        // Garbage inputs.
        assert!(verify_minisign("garbage", b"x", &pk_box).is_err());
        assert!(verify_minisign(&sig_text, b"x", "garbage").is_err());
    }
}

use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

const GITHUB_API_URL: &str = "https://api.github.com/repos/darkian-studio/dsterm/releases/latest";
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_FILE: &str = ".dsterm_update_cache";

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
            fs::write(cache_path, content).await?;
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
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
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

    pub async fn update(&self) -> Result<(), Box<dyn std::error::Error>> {
        let release: GithubRelease = self
            .client
            .get(GITHUB_API_URL)
            .header("User-Agent", "dsterm-update-checker")
            .send()
            .await?
            .json()
            .await?;

        // Detect the platform. Termux (Android) is detected at runtime; every
        // other OS maps straight from the compiled target.
        let is_termux = std::env::var("TERMUX_VERSION").is_ok()
            || std::path::Path::new("/data/data/com.termux").exists();

        let platform = if is_termux {
            "android"
        } else {
            match std::env::consts::OS {
                os @ ("linux" | "macos" | "windows") => os,
                other => return Err(format!("Unsupported OS: {other}").into()),
            }
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

        // Verify before touching anything on disk. A bad download here (wrong
        // platform, truncated transfer, mismatched redirect target) must fail
        // loudly, never silently replace a working binary. Nothing past this
        // block runs unless every check passes.

        if response.len() as u64 != asset.size {
            return Err(format!(
                "Size mismatch for {binary_name}: expected {} bytes, got {}. \
                 Aborting update; the installed binary was NOT touched.",
                asset.size,
                response.len()
            )
            .into());
        }

        let checksum_name = format!("{binary_name}.sha256");
        if let Some(checksum_asset) = release.assets.iter().find(|a| a.name == checksum_name) {
            let checksum_body = self
                .client
                .get(&checksum_asset.browser_download_url)
                .send()
                .await?
                .text()
                .await?;
            let expected = checksum_body
                .split_whitespace()
                .next()
                .ok_or("Malformed checksum asset")?
                .to_lowercase();

            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(response.as_ref());
            let actual: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();

            if actual != expected {
                return Err(format!(
                    "Checksum mismatch for {binary_name}: expected {expected}, got {actual}. \
                     Aborting update; the installed binary was NOT touched."
                )
                .into());
            }
        }

        // Independent of the checksum: this is the exact failure already observed
        // (an asset named dsterm-windows-x86_64.exe served ELF bytes). Confirm the
        // payload's magic number matches the platform before it goes near current_exe.
        if platform == "windows" && !response.starts_with(b"MZ") {
            return Err(format!(
                "{binary_name} does not have a Windows PE header (expected 'MZ'). \
                 Aborting update; the installed binary was NOT touched."
            )
            .into());
        }

        let current_exe = std::env::current_exe()?;
        let temp_path = current_exe.with_extension("new");

        let mut file = File::create(&temp_path).await?;
        file.write_all(&response).await?;
        file.sync_all().await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&temp_path).await?;
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&temp_path, perms).await?;
        }

        // A running executable cannot be overwritten on Windows, so move the
        // current binary aside first, then drop the new one into place. On Unix
        // an atomic rename over the running binary is fine.
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
}

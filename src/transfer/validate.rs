//! The receiver resolves every sender-supplied path against its own --dest
//! anchor and never through a symlink planted mid-transfer. A symlink target
//! is stored verbatim but never followed while placing later entries —
//! otherwise one malicious entry redirects all following writes.
//!
//! Windows names are rejected rather than transliterated: silently renaming
//! `CON` or stripping a trailing dot creates a different file than the sender
//! meant, which is its own hazard.

use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::transfer::{MAX_ENTRY_PATH_LEN, MAX_PATH_COMPONENT_LEN};

fn lexical_parts(path: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = Vec::new();
    let normalized = path.replace('\\', "/");
    for comp in normalized.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c.to_string()),
        }
    }
    Some(parts)
}

// Resolved lexically on purpose: canonicalize() would follow a symlink the
// sender just planted, defeating the check it is meant to perform.
pub fn join_within(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    let parts = lexical_parts(rel).ok_or_else(|| anyhow!("path escapes transfer root: '{rel}'"))?;
    let mut out = root.to_path_buf();
    for p in parts {
        validate_component(&p, false)?;
        out.push(p);
    }
    Ok(out)
}

pub fn validate_component(comp: &str, is_windows: bool) -> Result<()> {
    if comp.is_empty() {
        anyhow::bail!("empty path component");
    }
    if comp == "." || comp == ".." {
        anyhow::bail!("path escapes transfer root: '{comp}'");
    }
    if comp.contains('\0') {
        anyhow::bail!("path contains NUL byte");
    }
    if comp.len() > MAX_PATH_COMPONENT_LEN {
        anyhow::bail!("path component too long: '{comp}'");
    }
    if comp.contains('/') || comp.contains('\\') {
        anyhow::bail!("invalid path component: '{comp}'");
    }
    if comp.len() >= 2 && comp.as_bytes()[1] == b':' {
        anyhow::bail!("absolute path rejected: '{comp}'");
    }
    if is_windows {
        check_windows_component(comp)?;
    }
    Ok(())
}

// Root names stay single-component so placement is always
// <dest>/<one name>. Anything with a separator is a sender dictating layout.
pub fn validate_root_name(name: &str, is_windows: bool) -> Result<String> {
    if name.is_empty() {
        anyhow::bail!("empty transfer name");
    }
    if name.contains('\0') {
        anyhow::bail!("transfer name contains NUL byte");
    }
    if name.len() > MAX_ENTRY_PATH_LEN {
        anyhow::bail!("transfer name too long");
    }
    if name.contains('/') || name.contains('\\') {
        anyhow::bail!("transfer name must be a single file/directory name, got '{name}'");
    }
    if name == "." || name == ".." {
        anyhow::bail!("invalid transfer name: '{name}'");
    }
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        anyhow::bail!("absolute transfer name rejected: '{name}'");
    }
    if name.starts_with("\\\\") {
        anyhow::bail!("absolute transfer name rejected: '{name}'");
    }
    validate_component(name, is_windows)?;
    Ok(name.to_string())
}

// Hints are advisory: a malicious hint must fail the transfer, never move it.
pub fn validate_dest_hint(hint: &Option<String>, is_windows: bool) -> Result<Option<String>> {
    let Some(h) = hint else { return Ok(None) };
    if h.is_empty() {
        return Ok(None);
    }
    if h.contains('\0') {
        anyhow::bail!("destination hint contains NUL byte");
    }
    if h.len() > MAX_ENTRY_PATH_LEN {
        anyhow::bail!("destination hint too long");
    }
    let t = h.replace('\\', "/");
    if t.starts_with('/') {
        anyhow::bail!("destination hint must be relative, got '{h}'");
    }
    if t.len() >= 2 && t.as_bytes()[1] == b':' {
        anyhow::bail!("destination hint must be relative, got '{h}'");
    }
    if t.starts_with("//") || t.starts_with("\\\\") {
        anyhow::bail!("destination hint must be relative, got '{h}'");
    }
    let parts =
        lexical_parts(&t).ok_or_else(|| anyhow!("destination hint escapes destination: '{h}'"))?;
    if parts.is_empty() {
        return Ok(None);
    }
    for p in &parts {
        validate_component(p, is_windows)?;
    }
    Ok(Some(parts.join("/")))
}

pub fn validate_entry_path(path: &str, is_windows: bool) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    if path.contains('\0') {
        anyhow::bail!("entry path contains NUL byte: '{path}'");
    }
    if path.len() > MAX_ENTRY_PATH_LEN {
        anyhow::bail!("entry path too long: '{path}'");
    }
    let t = path.replace('\\', "/");
    if t.starts_with('/') {
        anyhow::bail!("absolute entry path rejected: '{path}'");
    }
    if t.len() >= 2 && t.as_bytes()[1] == b':' {
        anyhow::bail!("absolute entry path rejected: '{path}'");
    }
    let parts =
        lexical_parts(&t).ok_or_else(|| anyhow!("entry path escapes transfer root: '{path}'"))?;
    if parts.is_empty() {
        anyhow::bail!("invalid entry path: '{path}'");
    }
    for p in &parts {
        validate_component(p, is_windows)?;
    }
    Ok(())
}

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn is_windows_reserved_name(comp: &str) -> bool {
    let stem = comp.split('.').next().unwrap_or(comp);
    WINDOWS_RESERVED.contains(&stem.to_ascii_uppercase().as_str())
}

pub fn check_windows_component(comp: &str) -> Result<()> {
    if is_windows_reserved_name(comp) {
        anyhow::bail!("reserved Windows device name: '{comp}'");
    }
    for ch in [':', '*', '?', '"', '<', '>', '|'] {
        if comp.contains(ch) {
            anyhow::bail!("character '{ch}' is not valid in Windows filenames: '{comp}'");
        }
    }
    if comp.chars().any(|c| (c as u32) < 0x20) {
        anyhow::bail!("control character in Windows filename: '{comp}'");
    }
    if comp.ends_with('.') || comp.ends_with(' ') {
        anyhow::bail!("trailing dot/space is stripped by Windows, rejected: '{comp}'");
    }
    Ok(())
}

// Two POSIX names differing only by case would silently overwrite each other
// on a case-insensitive receiver; treat the second as a conflict instead.
pub fn check_case_collision(seen: &mut HashSet<String>, path: &str) -> Result<()> {
    let key = path.to_lowercase();
    if !seen.insert(key) {
        anyhow::bail!("case-insensitive filename collision: '{path}'");
    }
    Ok(())
}

// An unattended receiver has no human checking where writes land, so its
// default is a dedicated directory — never whatever cwd a service happened
// to start with.
pub fn resolve_dest_base(dest: &Option<String>, auto_receive: bool) -> Result<PathBuf> {
    if let Some(d) = dest {
        let p = PathBuf::from(d);
        ensure_dest_dir(&p)?;
        return Ok(p);
    }
    if auto_receive {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let p = home.join(".dsterm").join("incoming");
        ensure_dest_dir(&p)?;
        return Ok(p);
    }
    let cwd = std::env::current_dir().map_err(|e| anyhow!("cannot resolve cwd: {e}"))?;
    ensure_dest_dir(&cwd)?;
    Ok(cwd)
}

pub fn ensure_dest_dir(dir: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "destination '{}' is a symlink; refusing to receive into it",
                dir.display()
            );
        }
        if !meta.is_dir() {
            anyhow::bail!(
                "destination '{}' exists and is not a directory",
                dir.display()
            );
        }
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| anyhow!("cannot create destination '{}': {e}", dir.display()))?;
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "destination '{}' is a symlink; refusing to receive into it",
                dir.display()
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ConflictInfo {
    pub is_conflict: bool,
    pub detail: String,
}

// An empty directory accepts a same-named incoming tree (least surprise);
// anything else — file, non-empty dir, symlink, type mismatch — is a
// conflict the receiver resolves explicitly, never by accident.
pub fn destination_conflict(final_path: &Path, incoming_is_dir: bool) -> ConflictInfo {
    let meta = match std::fs::symlink_metadata(final_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ConflictInfo {
                is_conflict: false,
                detail: "does not exist".to_string(),
            }
        }
        Err(e) => {
            return ConflictInfo {
                is_conflict: true,
                detail: format!("cannot stat destination: {e}"),
            }
        }
        Ok(m) => m,
    };
    if meta.file_type().is_symlink() {
        return ConflictInfo {
            is_conflict: true,
            detail: "destination is a symlink (never followed)".to_string(),
        };
    }
    if meta.is_file() {
        return ConflictInfo {
            is_conflict: true,
            detail: "destination already exists as a file".to_string(),
        };
    }
    if meta.is_dir() {
        if !incoming_is_dir {
            return ConflictInfo {
                is_conflict: true,
                detail: "destination exists as a directory but incoming transfer is a file"
                    .to_string(),
            };
        }
        match std::fs::read_dir(final_path) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    ConflictInfo {
                        is_conflict: true,
                        detail: "destination directory already exists and is not empty".to_string(),
                    }
                } else {
                    ConflictInfo {
                        is_conflict: false,
                        detail: "existing empty directory".to_string(),
                    }
                }
            }
            Err(e) => ConflictInfo {
                is_conflict: true,
                detail: format!("cannot list destination directory: {e}"),
            },
        }
    } else {
        ConflictInfo {
            is_conflict: true,
            detail: "destination exists with an unsupported file type".to_string(),
        }
    }
}

pub fn resolve_rename(dest_base: &Path, name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i..])),
        _ => (name, None),
    };
    for n in 1..=999 {
        let candidate = match ext {
            Some(e) => format!("{stem} ({n}){e}"),
            None => format!("{name} ({n})"),
        };
        if !dest_base.join(&candidate).exists()
            && std::fs::symlink_metadata(dest_base.join(&candidate)).is_err()
        {
            return candidate;
        }
    }
    let suffix = &uuid::Uuid::new_v4().to_string()[..8];
    match ext {
        Some(e) => format!("{stem} ({suffix}){e}"),
        None => format!("{name} ({suffix})"),
    }
}

#[cfg(unix)]
pub fn free_space(path: &Path) -> u64 {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return u64::MAX;
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return u64::MAX;
    }
    (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
}

#[cfg(windows)]
pub fn free_space(_path: &Path) -> u64 {
    u64::MAX
}

pub fn pretty_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

// Loopback bounds network origin, not local identity: any user on a shared
// host can reach a loopback port, so auto-receive treats it as a weak
// boundary worth keeping but never worth trusting alone.
pub fn is_loopback_peer(addr: &std::net::SocketAddr) -> bool {
    match addr {
        std::net::SocketAddr::V4(v4) => v4.ip().is_loopback(),
        std::net::SocketAddr::V6(v6) => v6.ip().is_loopback(),
    }
}

#[allow(dead_code)]
pub fn path_components(p: &Path) -> Vec<Component<'_>> {
    p.components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_rejected() {
        assert!(validate_entry_path("../../etc/passwd", false).is_err());
        assert!(validate_entry_path("/abs/path", false).is_err());
        assert!(validate_entry_path("C:/windows", false).is_err());
        assert!(validate_entry_path("ok/sub/file.txt", false).is_ok());
        assert!(validate_entry_path("", false).is_ok());
    }

    #[test]
    fn root_names_must_be_single_component() {
        assert!(validate_root_name("project", false).is_ok());
        assert!(validate_root_name("a/b", false).is_err());
        assert!(validate_root_name("..", false).is_err());
        assert!(validate_root_name("", false).is_err());
    }

    #[test]
    fn dest_hint_traversal_rejected() {
        assert!(validate_dest_hint(&Some("../../x".into()), false).is_err());
        assert!(validate_dest_hint(&Some("/abs".into()), false).is_err());
        assert!(validate_dest_hint(&Some("sub/dir".into()), false).is_ok());
    }

    #[test]
    fn windows_reserved_names_rejected() {
        assert!(check_windows_component("CON").is_err());
        assert!(check_windows_component("con.txt").is_err());
        assert!(check_windows_component("ok.txt").is_ok());
        assert!(check_windows_component("bad|name").is_err());
        assert!(check_windows_component("trailing.").is_err());
    }

    #[test]
    fn case_collision_detected() {
        let mut seen = HashSet::new();
        check_case_collision(&mut seen, "Foo.txt").unwrap();
        assert!(check_case_collision(&mut seen, "foo.txt").is_err());
    }

    #[test]
    fn join_stays_within_root() {
        let root = Path::new("/tmp/dest");
        assert!(join_within(root, "a/b").unwrap().starts_with(root));
        assert!(join_within(root, "../x").is_err());
    }
}

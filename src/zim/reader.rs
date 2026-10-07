//! Blocking ZIM reader primitives over the `zim` crate (Z2).
//!
//! Everything here runs off the async executor (dedicated blocking
//! pool, `mod.rs`). No HTTP, no DS concepts, no global state: pure
//! functions over an opened [`Zim`], so unit tests exercise the real
//! parsing without a server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use zim_reader::{DirectoryEntry, MimeType, Namespace, Zim};

/// Domain failures that cross the blocking boundary with stable meaning.
/// Transport mapping lives in `mod.rs`; anything else is `Unexpected`.
#[derive(Debug)]
pub enum Fault {
    RedirectLoop,
    NoTitleIndex,
    TooLarge { bytes: usize },
    Unexpected(anyhow::Error),
}

impl From<anyhow::Error> for Fault {
    fn from(error: anyhow::Error) -> Self {
        Fault::Unexpected(error)
    }
}

impl From<zim_reader::Error> for Fault {
    fn from(error: zim_reader::Error) -> Self {
        Fault::Unexpected(anyhow::Error::from(error))
    }
}

/// Lookup namespaces in order: new scheme first, then legacy homes for
/// old-scheme content and images (recorded rule:
/// entry-key space and URL space stay distinct — DS resolves URLs to
/// these keys client-side before calling lookup).
const LOOKUP_ORDER: [Namespace; 4] = [
    Namespace::UserContent,
    Namespace::Articles,
    Namespace::ImagesFile,
    Namespace::Layout,
];

/// Open + validate an archive file that policy has already admitted
/// (regular file, under an allowed root, not a symlink). Reads header
/// fields the registry needs. Magic/version/offset validation is the
/// crate's (`InvalidHeader` and friends surface as errors here).
pub fn open_validated(canonical: &Path) -> Result<OpenedArchive> {
    let meta =
        std::fs::metadata(canonical).with_context(|| format!("stat {}", canonical.display()))?;
    if !meta.is_file() {
        anyhow::bail!("not a regular file: {}", canonical.display());
    }
    let zim = Zim::new(canonical).with_context(|| format!("parse {}", canonical.display()))?;
    let header = &zim.header;
    Ok(OpenedArchive {
        uuid: header.uuid.to_string(),
        version_major: header.version_major,
        version_minor: header.version_minor,
        article_count: header.article_count,
        cluster_count: header.cluster_count,
        file_size: meta.len(),
        path: canonical.to_path_buf(),
        zim: Some(zim),
    })
}

/// Header facts without retaining the mapping (cheap probes).
pub struct OpenedArchive {
    pub uuid: String,
    pub version_major: u16,
    pub version_minor: u16,
    pub article_count: u32,
    pub cluster_count: u32,
    pub file_size: u64,
    pub path: PathBuf,
    /// Retain the mapping only when the caller keeps the archive open.
    /// `None` + drop = unmapped.
    pub zim: Option<Zim>,
}

/// Text metadata map (`M/` entries that decode as UTF-8; binary values
/// such as illustrations are skipped — they are read as entries).
pub fn read_metadata(zim: &Zim) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let keys = zim.metadata_keys().unwrap_or_default();
    for key in keys {
        if let Ok(Some(content)) = zim.metadata(&key) {
            if let Ok(len) = content.len() {
                if len > 1_048_576 {
                    continue;
                }
            }
            if let Ok(bytes) = content.to_vec() {
                if let Ok(text) = String::from_utf8(bytes) {
                    out.insert(key, text);
                }
            }
        }
    }
    out
}

/// Whether the archive has any usable title listing, and which one.
/// New scheme exposes the article list; old scheme the entry list —
/// the adapter/suggest layer tries article first, then entry.
pub fn title_lists(zim: &Zim) -> (bool, bool) {
    let article = zim
        .article_list_by_title()
        .map(|list| list.map(|l| l.len().unwrap_or(0)).unwrap_or(0))
        .unwrap_or(0);
    let entry = zim
        .entry_list_by_title()
        .map(|list| list.map(|l| l.len().unwrap_or(0)).unwrap_or(0))
        .unwrap_or(0);
    (article > 0, entry > 0)
}

/// Ordered (title, url-pointer index, url) triples with the same
/// article-first/entry-fallback rule. Byte-ordered as the index defines
/// it — no case folding (DZ-31). Untitled entries are skipped: an empty
/// title is not a discovery result.
pub fn titles_in_order(zim: &Zim) -> Vec<(String, u32, String)> {
    let listing = zim
        .article_list_by_title()
        .ok()
        .flatten()
        .or_else(|| zim.entry_list_by_title().ok().flatten());
    let Some(listing) = listing else {
        return Vec::new();
    };
    let Ok(indices) = listing.to_vec() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(indices.len().min(1_000_000));
    for idx in indices {
        let Ok(entry) = zim.get_by_url_index(idx) else {
            continue;
        };
        if entry.title.is_empty() {
            continue;
        }
        out.push((entry.title, idx, entry.url));
        if out.len() >= 1_000_000 {
            break;
        }
    }
    out
}

/// Entry lookup across the namespace order. Returns the entry with its
/// url-pointer index.
pub fn lookup_entry(zim: &Zim, path: &str) -> Result<Option<(DirectoryEntry, u32)>, Fault> {
    for ns in LOOKUP_ORDER {
        match zim.find_by_path(ns, path)? {
            Some(idx) => {
                let entry = zim.get_by_url_index(idx)?;
                return Ok(Some((entry, idx)));
            }
            None => continue,
        }
    }
    Ok(None)
}

/// Follows a redirect chain (bounded, D25). Returns the terminal entry,
/// its index, and the traversed chain.
pub fn resolve_chain(
    zim: &Zim,
    entry: DirectoryEntry,
    index: u32,
) -> Result<(DirectoryEntry, u32, Vec<String>), Fault> {
    let mut chain = vec![entry.url.clone()];
    let mut current = (entry, index);
    for _ in 0..8 {
        let target = match &current.0.target {
            Some(zim_reader::Target::Redirect(idx)) => *idx,
            _ => return Ok((current.0, current.1, chain)),
        };
        let next = zim.get_by_url_index(target)?;
        chain.push(next.url.clone());
        current = (next, target);
    }
    Err(Fault::RedirectLoop)
}

/// Reads one entry's bytes. Redirects, link-targets and deleted entries
/// are the caller's to resolve first (content routes canonicalise), so
/// hitting one here is a caller bug, surfaced as unexpected.
pub fn read_entry_bytes(zim: &Zim, entry: &DirectoryEntry) -> Result<Vec<u8>, Fault> {
    match entry.mime_type {
        MimeType::Redirect => {
            return Err(Fault::Unexpected(anyhow!("entry is a redirect")));
        }
        MimeType::DeletedEntry => {
            return Err(Fault::Unexpected(anyhow!("entry is deleted")));
        }
        _ => {}
    }
    let Some(content) = zim.entry_content(entry)? else {
        return Err(Fault::Unexpected(anyhow!("entry has no content")));
    };
    let len = content.len()?;
    if len > MAX_SINGLE_READ_BYTES {
        return Err(Fault::TooLarge { bytes: len });
    }
    Ok(content.to_vec()?)
}

/// Cap for one decoded entry (DZ-23, D26). Larger entries fail with
/// `content_unavailable`/`limit_exceeded` instead of inflating the heap.
pub const MAX_SINGLE_READ_BYTES: usize = 32 * 1024 * 1024;

/// MIME string exactly as archived.
pub fn mime_of(entry: &DirectoryEntry) -> String {
    match &entry.mime_type {
        MimeType::Type(s) => s.clone(),
        MimeType::LinkTarget => "application/octet-stream".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests (blocking primitives on committed fixtures, no server)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/zim")
            .join(name)
    }

    #[test]
    fn opens_new_and_old_scheme() {
        for name in ["lit.zim", "nons-small.zim", "withns-small.zim"] {
            let opened = open_validated(&fixture(name)).expect(name);
            assert!(!opened.uuid.is_empty());
            assert!(opened.article_count > 0);
            assert!(opened.zim.is_some());
        }
        let old = open_validated(&fixture("withns-small.zim")).unwrap();
        assert_eq!((old.version_major, old.version_minor), (5, 0));
        let new = open_validated(&fixture("lit.zim")).unwrap();
        assert_eq!((new.version_major, new.version_minor), (6, 3));
    }

    #[test]
    fn corrupt_headers_fail_cleanly() {
        let err = open_validated(&fixture("invalid-header.zim"))
            .err()
            .expect("corrupt file must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("smaller than the header") || msg.contains("InvalidHeader"),
            "{msg}"
        );
    }

    #[test]
    fn metadata_titles_and_lookup() {
        let opened = open_validated(&fixture("lit.zim")).unwrap();
        let zim = opened.zim.unwrap();
        let meta = read_metadata(&zim);
        assert_eq!(meta.get("Title").map(String::as_str), Some("Lit Docs"));
        let (article, entry) = title_lists(&zim);
        assert!(article || entry);
        let titles = titles_in_order(&zim);
        assert!(!titles.is_empty());
        assert!(titles.iter().all(|(t, _, _)| !t.is_empty()));

        // First redirect found by scan (mirrors real usage: lookup,
        // then resolve).
        let mut redirect = None;
        for i in 0..zim.header.article_count.min(200) {
            if let Ok(entry) = zim.get_by_url_index(i) {
                if matches!(entry.mime_type, MimeType::Redirect) {
                    redirect = Some(entry);
                    break;
                }
            }
        }
        let entry = redirect.expect("fixture has a redirect");
        let entry_url = entry.url.clone();
        let (terminal, _, chain) = resolve_chain(&zim, entry, 0).unwrap();
        assert_eq!(chain.first(), Some(&entry_url));
        assert!(!terminal.url.is_empty());

        let bytes = read_entry_bytes(&zim, &terminal).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(mime_of(&terminal), "text/html");
    }

    #[test]
    fn missing_entries_yield_none() {
        let opened = open_validated(&fixture("lit.zim")).unwrap();
        let zim = opened.zim.unwrap();
        assert!(lookup_entry(&zim, "no-such-entry").unwrap().is_none());
    }

    /// Deterministic mutation sweep (fuzz-lite): flipping header and
    /// offset bytes must yield controlled errors, never a panic. This
    /// runs on stable (no libFuzzer here); `cargo-fuzz` remains the
    /// nightly follow-up (DZ-26).
    #[test]
    fn mutated_headers_never_panic() {
        let original = std::fs::read(fixture("lit.zim")).expect("fixture bytes");
        // Xorshift64*: deterministic without a rng dependency.
        let mut state: u64 = 0x12345678;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..200 {
            let mut mutated = original.clone();
            // Bias mutations at the header and pointer tables.
            let base = if round % 2 == 0 {
                (next() % 512) as usize
            } else {
                (next() % mutated.len() as u64) as usize
            };
            for k in 0..4 {
                let idx = (base + k * 7919) % mutated.len();
                mutated[idx] ^= 0xFF;
            }
            let dir =
                std::env::temp_dir().join(format!("dsterm-mut-{}-{}", std::process::id(), round));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("m.zim");
            std::fs::write(&path, &mutated).unwrap();
            let opened = open_validated(&path);
            std::fs::remove_dir_all(&dir).ok();
            // Either outcome is fine; panicking is not (fails the test).
            if let Ok(opened) = opened {
                if let Some(zim) = opened.zim {
                    let _ = title_lists(&zim);
                    let _ = lookup_entry(&zim, "index");
                }
            }
        }
    }
}

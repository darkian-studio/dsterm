//! Bounded LRU cache of parsed tree-sitter trees keyed by document id.

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use tree_sitter::Tree;

pub struct CachedDocument {
    pub version: i64,
    pub source: Vec<u8>,
    pub tree: Tree,
}

pub struct DocumentCache {
    inner: Mutex<LruCache<String, Arc<CachedDocument>>>,
}

impl DocumentCache {
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity).expect("cache capacity must be non-zero");
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<CachedDocument>> {
        let mut guard = self.inner.lock().expect("ast document cache lock poisoned");
        guard.get(id).cloned()
    }

    pub fn insert(&self, id: String, doc: CachedDocument) {
        let mut guard = self.inner.lock().expect("ast document cache lock poisoned");
        guard.put(id, Arc::new(doc));
    }
}

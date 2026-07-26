use lru::LruCache;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub data: Vec<u8>,
    pub content_type: String,
    pub status: u16,
    pub url: String,
    pub fetched_at: Instant,
    pub ttl: Duration,
}

impl CacheEntry {
    pub fn is_expired(&self) -> bool {
        self.fetched_at.elapsed() > self.ttl
    }
}

#[derive(Debug, Clone)]
pub struct CacheKey {
    pub operation: String,
    pub url: String,
    pub extra: String,
}

impl CacheKey {
    pub fn hash_key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.operation.hash(&mut hasher);
        self.url.hash(&mut hasher);
        self.extra.hash(&mut hasher);
        hasher.finish()
    }
}

pub struct ContentCache {
    inner: RwLock<LruCache<u64, CacheEntry>>,
}

impl ContentCache {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(LruCache::new(capacity)),
        })
    }

    pub async fn get(&self, key: &CacheKey) -> Option<CacheEntry> {
        let hash = key.hash_key();
        let mut cache = self.inner.write().await;
        if let Some(entry) = cache.get(&hash) {
            if entry.is_expired() {
                cache.pop(&hash);
                None
            } else {
                Some(entry.clone())
            }
        } else {
            None
        }
    }

    pub async fn insert(&self, key: &CacheKey, entry: CacheEntry) {
        let hash = key.hash_key();
        let mut cache = self.inner.write().await;
        cache.put(hash, entry);
    }

    pub async fn clear(&self) {
        let mut cache = self.inner.write().await;
        cache.clear();
    }

    pub async fn remove(&self, key: &CacheKey) -> bool {
        let hash = key.hash_key();
        let mut cache = self.inner.write().await;
        cache.pop(&hash).is_some()
    }
}

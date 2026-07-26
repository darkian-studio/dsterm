use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    #[serde(default)]
    pub css_selector: Option<String>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default = "default_max_fetch_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_fetch_timeout")]
    pub timeout_secs: u64,
}

fn default_max_fetch_bytes() -> usize {
    2_000_000
}
fn default_fetch_timeout() -> u64 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub html: String,
    pub markdown: String,
    pub title: Option<String>,
    pub metadata: HashMap<String, String>,
    pub content_ok: bool,
    pub from_cache: bool,
    pub bytes_fetched: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub focus: Option<String>,
}

fn default_search_limit() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub score: f64,
    #[serde(default)]
    pub engines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub query: String,
    pub total_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlRequest {
    pub root_url: String,
    #[serde(default = "default_crawl_depth")]
    pub max_depth: usize,
    #[serde(default = "default_crawl_pages")]
    pub max_pages: usize,
    #[serde(default = "default_crawl_chars")]
    pub max_total_chars: usize,
    #[serde(default = "default_crawl_timeout")]
    pub deadline_ms: u64,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub discover_only: bool,
    #[serde(default)]
    pub crawl_urls: Option<Vec<String>>,
}

fn default_crawl_depth() -> usize {
    2
}
fn default_crawl_pages() -> usize {
    10
}
fn default_crawl_chars() -> usize {
    400_000
}
fn default_crawl_timeout() -> u64 {
    120_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlPage {
    pub url: String,
    pub title: Option<String>,
    pub markdown: String,
    pub status: u16,
    pub content_ok: bool,
    pub page_type: String,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlResponse {
    pub pages: Vec<CrawlPage>,
    pub root_url: String,
    pub pages_crawled: usize,
    pub truncated_by_time: bool,
    pub truncated_by_pages: bool,
}

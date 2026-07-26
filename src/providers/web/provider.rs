use super::cache::ContentCache;
use super::crawler::CrawlService;
use super::fetch::HttpService;
use super::models::*;
use super::permissions::NetworkPermissions;
use super::reader::{self, PageType};
use super::search::SearchService;
use crate::providers::{Provider, ProviderRequest, ProviderResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

pub struct WebProvider {
    http: Arc<HttpService>,
    search: SearchService,
    crawl: CrawlService,
}

impl WebProvider {
    pub fn new() -> Self {
        let permissions = NetworkPermissions::default();
        let cache = ContentCache::new(256);
        let http = Arc::new(HttpService::new(permissions, cache));
        let search = SearchService::new();
        let crawl = CrawlService::new(http.clone());

        Self {
            http,
            search,
            crawl,
        }
    }

    pub fn with_permissions(permissions: NetworkPermissions) -> Self {
        let cache = ContentCache::new(256);
        let http = Arc::new(HttpService::new(permissions, cache));
        let search = SearchService::new();
        let crawl = CrawlService::new(http.clone());

        Self {
            http,
            search,
            crawl,
        }
    }

    async fn handle_fetch(
        &self,
        payload: serde_json::Value,
        budgets: &crate::providers::Budgets,
    ) -> ProviderResponse {
        let request: FetchRequest = match serde_json::from_value(payload) {
            Ok(r) => r,
            Err(e) => {
                return ProviderResponse {
                    success: false,
                    data: None,
                    error: Some(format!("invalid fetch request: {e}")),
                    metadata: HashMap::new(),
                };
            }
        };

        match self
            .http
            .fetch(
                &request.url,
                request.css_selector.as_deref(),
                request.focus.as_deref(),
                request.max_bytes.min(budgets.max_bytes),
            )
            .await
        {
            Ok(response) => {
                let mut markdown = response.markdown;
                if let Some(ref focus) = request.focus {
                    markdown = reader::apply_focus(&markdown, focus);
                }

                let mut meta = HashMap::new();
                meta.insert("status".into(), serde_json::json!(response.status));
                meta.insert("content_ok".into(), serde_json::json!(response.content_ok));
                meta.insert("from_cache".into(), serde_json::json!(response.from_cache));
                meta.insert("bytes".into(), serde_json::json!(response.bytes_fetched));

                ProviderResponse {
                    success: true,
                    data: Some(serde_json::json!({
                        "url": response.url,
                        "status": response.status,
                        "content_type": response.content_type,
                        "html": response.html,
                        "markdown": markdown,
                        "title": response.title,
                        "content_ok": response.content_ok,
                        "from_cache": response.from_cache,
                    })),
                    error: None,
                    metadata: meta,
                }
            }
            Err(e) => ProviderResponse {
                success: false,
                data: None,
                error: Some(e.to_string()),
                metadata: HashMap::new(),
            },
        }
    }

    async fn handle_extract(
        &self,
        payload: serde_json::Value,
        budgets: &crate::providers::Budgets,
    ) -> ProviderResponse {
        let request: FetchRequest = match serde_json::from_value(payload) {
            Ok(r) => r,
            Err(e) => {
                return ProviderResponse {
                    success: false,
                    data: None,
                    error: Some(format!("invalid extract request: {e}")),
                    metadata: HashMap::new(),
                };
            }
        };

        match self
            .http
            .fetch(
                &request.url,
                None,
                None,
                request.max_bytes.min(budgets.max_bytes),
            )
            .await
        {
            Ok(response) => {
                let page_type = reader::classify_page(&response.html);
                let markdown = match page_type {
                    PageType::Article => reader::extract_article(&response.html),
                    PageType::List => reader::extract_list(&response.html),
                    _ => response.markdown,
                };

                let final_markdown = if let Some(ref focus) = request.focus {
                    reader::apply_focus(&markdown, focus)
                } else {
                    markdown
                };

                let mut meta = HashMap::new();
                meta.insert("page_type".into(), serde_json::json!(page_type.as_str()));
                meta.insert("content_ok".into(), serde_json::json!(response.content_ok));

                ProviderResponse {
                    success: true,
                    data: Some(serde_json::json!({
                        "url": response.url,
                        "title": response.title,
                        "markdown": final_markdown,
                        "page_type": page_type.as_str(),
                        "content_ok": response.content_ok,
                    })),
                    error: None,
                    metadata: meta,
                }
            }
            Err(e) => ProviderResponse {
                success: false,
                data: None,
                error: Some(e.to_string()),
                metadata: HashMap::new(),
            },
        }
    }

    async fn handle_search(
        &self,
        payload: serde_json::Value,
        budgets: &crate::providers::Budgets,
    ) -> ProviderResponse {
        let request: SearchRequest = match serde_json::from_value(payload) {
            Ok(r) => r,
            Err(e) => {
                return ProviderResponse {
                    success: false,
                    data: None,
                    error: Some(format!("invalid search request: {e}")),
                    metadata: HashMap::new(),
                };
            }
        };

        let limited = SearchRequest {
            limit: request.limit.min(budgets.max_pages),
            ..request
        };

        let response = self.search.search(limited).await;

        let mut meta = HashMap::new();
        meta.insert(
            "total_results".into(),
            serde_json::json!(response.total_results),
        );

        ProviderResponse {
            success: true,
            data: Some(serde_json::json!({
                "results": response.results,
                "query": response.query,
                "total_results": response.total_results,
            })),
            error: None,
            metadata: meta,
        }
    }

    async fn handle_crawl(
        &self,
        payload: serde_json::Value,
        budgets: &crate::providers::Budgets,
    ) -> ProviderResponse {
        let request: CrawlRequest = match serde_json::from_value(payload) {
            Ok(r) => r,
            Err(e) => {
                return ProviderResponse {
                    success: false,
                    data: None,
                    error: Some(format!("invalid crawl request: {e}")),
                    metadata: HashMap::new(),
                };
            }
        };

        let limited = CrawlRequest {
            max_pages: request.max_pages.min(budgets.max_pages),
            max_total_chars: request.max_total_chars.min(budgets.max_bytes),
            ..request
        };

        let response = self.crawl.crawl(limited).await;

        let mut meta = HashMap::new();
        meta.insert(
            "pages_crawled".into(),
            serde_json::json!(response.pages_crawled),
        );
        meta.insert(
            "truncated_by_time".into(),
            serde_json::json!(response.truncated_by_time),
        );

        ProviderResponse {
            success: true,
            data: Some(serde_json::json!({
                "pages": response.pages,
                "root_url": response.root_url,
                "pages_crawled": response.pages_crawled,
                "truncated_by_time": response.truncated_by_time,
                "truncated_by_pages": response.truncated_by_pages,
            })),
            error: None,
            metadata: meta,
        }
    }
}

#[async_trait]
impl Provider for WebProvider {
    fn id(&self) -> &'static str {
        "web"
    }

    async fn execute(&self, request: ProviderRequest) -> ProviderResponse {
        match request.operation.as_str() {
            "fetch" => self.handle_fetch(request.payload, &request.budgets).await,
            "extract" => self.handle_extract(request.payload, &request.budgets).await,
            "search" => self.handle_search(request.payload, &request.budgets).await,
            "crawl" => self.handle_crawl(request.payload, &request.budgets).await,
            op => ProviderResponse {
                success: false,
                data: None,
                error: Some(format!(
                    "unknown web operation: {op}. supported: fetch, extract, search, crawl"
                )),
                metadata: HashMap::new(),
            },
        }
    }
}

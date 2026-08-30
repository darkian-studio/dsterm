use super::fetch::HttpService;
use super::models::{CrawlPage, CrawlRequest, CrawlResponse};
use super::reader::{self, PageType};
use reqwest::Url;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CRAWL_CONCURRENCY: usize = 5;
const PER_PAGE_TIMEOUT: Duration = Duration::from_secs(15);

type CrawlResult = Option<(CrawlPage, Vec<String>)>;
type PendingFuts = Vec<tokio::sync::oneshot::Receiver<CrawlResult>>;

struct CrawlUrl {
    url: String,
    depth: usize,
    score: f64,
}

impl Eq for CrawlUrl {}

impl PartialEq for CrawlUrl {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}

impl PartialOrd for CrawlUrl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CrawlUrl {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
    }
}
pub struct CrawlService {
    http: Arc<HttpService>,
}

impl CrawlService {
    pub fn new(http: Arc<HttpService>) -> Self {
        Self { http }
    }

    pub async fn crawl(&self, request: CrawlRequest) -> CrawlResponse {
        let deadline = Duration::from_millis(request.deadline_ms);
        let start = Instant::now();
        let mut visited: HashSet<String> = HashSet::new();
        let mut pages: Vec<CrawlPage> = Vec::new();
        let mut total_chars = 0usize;
        let mut truncated_by_time = false;
        let mut truncated_by_pages = false;
        let mut _fetch_errors = 0u32;

        if let Some(ref urls) = request.crawl_urls {
            let sem = Arc::new(tokio::sync::Semaphore::new(CRAWL_CONCURRENCY));
            let mut futs = Vec::new();

            for url in urls {
                if start.elapsed() >= deadline {
                    truncated_by_time = true;
                    break;
                }
                if pages.len() >= request.max_pages {
                    truncated_by_pages = true;
                    break;
                }
                let normalized = normalize_crawl_url(url);
                if !visited.insert(normalized) {
                    continue;
                }

                let http = self.http.clone();
                let url = url.clone();
                let focus = request.focus.clone();
                let permit = sem.clone().acquire_owned().await.unwrap();

                futs.push(tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        PER_PAGE_TIMEOUT,
                        http.fetch(&url, None, None, 2_000_000),
                    )
                    .await;
                    let _permit = permit;
                    match result {
                        Ok(Ok(response)) => Some(build_page(&response, 0, &focus)),
                        _ => None,
                    }
                }));
            }

            let results = futures::future::join_all(futs).await;
            for result in results {
                if let Ok(Some(page)) = result {
                    pages.push(page);
                }
            }

            return CrawlResponse {
                pages,
                root_url: request.root_url,
                pages_crawled: visited.len(),
                truncated_by_time,
                truncated_by_pages,
            };
        }

        if request.discover_only {
            let urls = self.discover_urls_iterative(&request.root_url, request.max_depth);
            for url in urls {
                pages.push(CrawlPage {
                    url,
                    title: None,
                    markdown: String::new(),
                    status: 0,
                    content_ok: false,
                    page_type: "discovered".to_string(),
                    depth: 0,
                });
            }
            return CrawlResponse {
                pages,
                root_url: request.root_url,
                pages_crawled: 0,
                truncated_by_time: false,
                truncated_by_pages: false,
            };
        }

        let root_domain = extract_domain(&request.root_url);
        let mut queue: BinaryHeap<CrawlUrl> = BinaryHeap::new();
        let mut discovered: HashSet<String> = HashSet::new();
        let sem = Arc::new(tokio::sync::Semaphore::new(CRAWL_CONCURRENCY));

        queue.push(CrawlUrl {
            url: request.root_url.clone(),
            depth: 0,
            score: 1.0,
        });
        discovered.insert(normalize_crawl_url(&request.root_url));

        let mut pending_futs: PendingFuts = Vec::new();

        while let Some(current) = queue.pop() {
            if start.elapsed() >= deadline {
                truncated_by_time = true;
                break;
            }
            if pages.len() >= request.max_pages {
                truncated_by_pages = true;
                break;
            }
            if total_chars >= request.max_total_chars {
                break;
            }
            if current.depth > request.max_depth {
                continue;
            }
            let normalized = normalize_crawl_url(&current.url);
            if !visited.insert(normalized) {
                continue;
            }

            let http = self.http.clone();
            let url = current.url.clone();
            let focus = request.focus.clone();
            let permit = sem.clone().acquire_owned().await.unwrap();

            let (tx, rx) = tokio::sync::oneshot::channel();
            pending_futs.push(rx);

            tokio::spawn(async move {
                let result =
                    tokio::time::timeout(PER_PAGE_TIMEOUT, http.fetch(&url, None, None, 2_000_000))
                        .await;
                let _permit = permit;

                let output = match result {
                    Ok(Ok(response)) => {
                        let page = build_page(&response, current.depth, &focus);
                        let mut links = Vec::new();
                        if page.content_ok && page.page_type != "js_shell" {
                            links = extract_links(&page.markdown, &url);
                        }
                        Some((page, links))
                    }
                    Ok(Err(_)) => None,
                    Err(_) => None,
                };

                let _ = tx.send(output);
            });

            while pending_futs.len() >= CRAWL_CONCURRENCY * 2 || queue.is_empty() {
                if let Some(rx) = pending_futs.first_mut() {
                    match rx.try_recv() {
                        Ok(Some((page, links))) => {
                            total_chars += page.markdown.len();

                            for link in &links {
                                let link_domain = extract_domain(link);
                                if link_domain != root_domain {
                                    continue;
                                }
                                let link_normalized = normalize_crawl_url(link);
                                if discovered.contains(&link_normalized) {
                                    continue;
                                }
                                discovered.insert(link_normalized);

                                let score = score_url(link, &request.focus);
                                queue.push(CrawlUrl {
                                    url: link.clone(),
                                    depth: current.depth + 1,
                                    score,
                                });
                            }

                            pages.push(page);
                            pending_futs.remove(0);
                        }
                        Ok(None) => {
                            _fetch_errors += 1;
                            pending_futs.remove(0);
                        }
                        Err(_) => {
                            pending_futs.remove(0);
                        }
                    }
                } else {
                    break;
                }

                if start.elapsed() >= deadline {
                    truncated_by_time = true;
                    break;
                }
                if pages.len() >= request.max_pages {
                    truncated_by_pages = true;
                    break;
                }
            }

            if start.elapsed() >= deadline {
                truncated_by_time = true;
                break;
            }
        }

        for rx in pending_futs {
            if let Ok(Some((page, links))) = rx.await {
                for link in &links {
                    let link_domain = extract_domain(link);
                    if link_domain != root_domain {
                        continue;
                    }
                    let link_normalized = normalize_crawl_url(link);
                    if !discovered.contains(&link_normalized) {
                        discovered.insert(link_normalized.clone());
                        queue.push(CrawlUrl {
                            url: link.clone(),
                            depth: 0,
                            score: score_url(link, &request.focus),
                        });
                    }
                }
                if pages.len() < request.max_pages {
                    pages.push(page);
                }
            }
        }

        CrawlResponse {
            pages,
            root_url: request.root_url,
            pages_crawled: visited.len(),
            truncated_by_time,
            truncated_by_pages,
        }
    }

    fn discover_urls_iterative(&self, root: &str, max_depth: usize) -> Vec<String> {
        let mut all_urls = Vec::new();
        let mut visited = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        let root_normalized = normalize_crawl_url(root);
        visited.insert(root_normalized);
        queue.push_back((root.to_string(), 0));

        while let Some((url, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }

            let root_domain = extract_domain(&url);
            all_urls.push(url.clone());

            let http = self.http.clone();
            let fetch_url = url.clone();
            let response = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    tokio::time::timeout(
                        PER_PAGE_TIMEOUT,
                        http.fetch(&fetch_url, None, None, 500_000),
                    )
                    .await
                })
            });

            if let Ok(Ok(response)) = response {
                let links = extract_links(&response.markdown, &url);
                for link in links {
                    let link_domain = extract_domain(&link);
                    if link_domain != root_domain {
                        continue;
                    }
                    let normalized = normalize_crawl_url(&link);
                    if visited.insert(normalized) {
                        queue.push_back((link, depth + 1));
                    }
                }
            }
        }

        all_urls
    }
}

fn build_page(
    response: &super::models::FetchResponse,
    depth: usize,
    focus: &Option<String>,
) -> CrawlPage {
    if !response.content_ok {
        return CrawlPage {
            url: response.url.clone(),
            title: response.title.clone(),
            markdown: String::new(),
            status: response.status,
            content_ok: false,
            page_type: PageType::Fallback.as_str().to_string(),
            depth,
        };
    }

    let page_type = reader::classify_page(&response.html);
    let mut markdown = match page_type {
        PageType::Article => reader::extract_article(&response.html),
        PageType::List => reader::extract_list(&response.html),
        _ => response.markdown.clone(),
    };

    if let Some(ref f) = focus {
        markdown = reader::apply_focus(&markdown, f);
    }

    CrawlPage {
        url: response.url.clone(),
        title: response.title.clone(),
        markdown,
        status: response.status,
        content_ok: response.content_ok,
        page_type: page_type.as_str().to_string(),
        depth,
    }
}

fn extract_links(markdown: &str, base_url: &str) -> Vec<String> {
    let mut links = Vec::new();
    let re = match regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)") {
        Ok(r) => r,
        Err(_) => return links,
    };

    for cap in re.captures_iter(markdown) {
        let href = &cap[2];
        if href.starts_with('#')
            || href.starts_with("javascript:")
            || href.starts_with("mailto:")
            || href.starts_with("data:")
        {
            continue;
        }
        let resolved = resolve_crawl_url(href, base_url);
        if resolved.starts_with("http") {
            links.push(resolved);
        }
    }

    links
}

fn resolve_crawl_url(href: &str, base: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with("//") {
        return format!("https:{}", href);
    }
    if !base.is_empty() {
        if let Ok(base_url) = Url::parse(base) {
            if let Ok(resolved) = base_url.join(href) {
                return resolved.to_string();
            }
        }
    }
    href.to_string()
}

fn score_url(url: &str, focus: &Option<String>) -> f64 {
    let mut score = 0.5;
    let lower = url.to_lowercase();

    let boost_patterns = [
        ("/docs", 0.3),
        ("/guide", 0.3),
        ("/api", 0.3),
        ("/reference", 0.3),
        ("/tutorial", 0.25),
        ("/example", 0.2),
        ("/blog", 0.15),
        ("/post", 0.15),
        ("/article", 0.15),
        ("/wiki", 0.2),
    ];

    for (pattern, boost) in &boost_patterns {
        if lower.contains(pattern) {
            score += boost;
        }
    }

    let penalize_patterns = [
        ("/login", -0.4),
        ("/signup", -0.4),
        ("/register", -0.4),
        ("/cart", -0.5),
        ("/checkout", -0.5),
        ("/payment", -0.4),
        ("/tag/", -0.2),
        ("/category/", -0.15),
        ("/author/", -0.15),
        ("/page/", -0.1),
        ("/search?", -0.3),
    ];

    for (pattern, penalty) in &penalize_patterns {
        if lower.contains(pattern) {
            score += penalty;
        }
    }

    if let Some(ref f) = focus {
        let focus_lower = f.to_lowercase();
        let focus_terms: Vec<&str> = focus_lower.split_whitespace().collect();
        let matching = focus_terms.iter().filter(|t| lower.contains(**t)).count();
        score += 0.1 * matching as f64;
    }

    let depth_penalty = lower.matches('/').count().saturating_sub(3) as f64 * 0.05;
    score -= depth_penalty;

    score.clamp(0.0, 2.0)
}

fn normalize_crawl_url(url: &str) -> String {
    super::fetch::normalize_url(url)
}

fn extract_domain(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_url_docs() {
        let score = score_url("https://docs.example.com/guide/intro", &None);
        assert!(score > 0.7);
    }

    #[test]
    fn test_score_url_cart_penalized() {
        let score = score_url("https://shop.example.com/cart", &None);
        assert!(score < 0.3);
    }

    #[test]
    fn test_score_url_focus_boost() {
        let score = score_url(
            "https://example.com/rust-guide",
            &Some("rust tutorial".into()),
        );
        assert!(score > 0.6);
    }

    #[test]
    fn test_extract_links_from_markdown() {
        let md = "Check [this](https://example.com/page) and [that](https://other.com)";
        let links = extract_links(md, "https://base.com");
        assert_eq!(links.len(), 2);
        assert!(links.contains(&"https://example.com/page".to_string()));
    }

    #[test]
    fn test_extract_links_skips_javascript() {
        let md = "[click](javascript:void(0)) and [real](https://example.com)";
        let links = extract_links(md, "https://base.com");
        assert_eq!(links.len(), 1);
    }
}

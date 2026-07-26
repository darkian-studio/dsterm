use super::models::{SearchRequest, SearchResponse, SearchResult};
use reqwest::Client;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use url::Url;

const PER_ENGINE_TIMEOUT: Duration = Duration::from_secs(8);
const ENGINE_RETRIES: u32 = 2;
const ENGINE_RETRY_DELAY: Duration = Duration::from_millis(800);

const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
];

#[derive(Debug, Clone)]
struct RawResult {
    title: String,
    url: String,
    snippet: String,
    engine: String,
    position: usize,
}

pub struct SearchService {
    clients: Vec<Client>,
    counter: std::sync::atomic::AtomicU64,
}

impl SearchService {
    pub fn new() -> Self {
        let clients: Vec<Client> = USER_AGENTS
            .iter()
            .map(|ua| {
                Client::builder()
                    .timeout(PER_ENGINE_TIMEOUT)
                    .connect_timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::limited(5))
                    .user_agent(*ua)
                    .build()
                    .expect("failed to build search client")
            })
            .collect();

        Self {
            clients,
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn client(&self) -> &Client {
        let idx = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        &self.clients[(idx as usize) % self.clients.len()]
    }

    pub async fn search(&self, request: SearchRequest) -> SearchResponse {
        let mut futs = Vec::new();

        futs.push(self.search_engine_retry("duckduckgo", &request.query, request.limit));
        futs.push(self.search_engine_retry("brave", &request.query, request.limit));
        futs.push(self.search_engine_retry("mojeek", &request.query, request.limit));
        futs.push(self.search_engine_retry("yahoo", &request.query, request.limit));
        futs.push(self.search_engine_retry("startpage", &request.query, request.limit));

        let results = futures::future::join_all(futs).await;

        let mut all_raw: Vec<RawResult> = results
            .into_iter()
            .filter_map(|r| r.ok())
            .flatten()
            .collect();

        let merged = merge_and_rank(&mut all_raw, &request.query, request.limit);

        let total = merged.len();
        SearchResponse {
            results: merged,
            query: request.query,
            total_results: total,
        }
    }

    async fn search_engine_retry(
        &self,
        engine: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let mut last_err = None;
        for attempt in 0..=ENGINE_RETRIES {
            let result = match engine {
                "duckduckgo" => self.search_duckduckgo(query, limit).await,
                "brave" => self.search_brave(query, limit).await,
                "mojeek" => self.search_mojek(query, limit).await,
                "yahoo" => self.search_yahoo(query, limit).await,
                "startpage" => self.search_startpage(query, limit).await,
                _ => return Err(last_err.unwrap_or_else(|| reqwest::Error::builder().build())),
            };

            match result {
                Ok(r) if !r.is_empty() => return Ok(r),
                Ok(_) => {
                    last_err = Some(reqwest::Error::builder().build());
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < ENGINE_RETRIES {
                        tokio::time::sleep(ENGINE_RETRY_DELAY).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| reqwest::Error::builder().build()))
    }

    async fn search_duckduckgo(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let resp = self.client().get(&url).send().await?;
        let html = resp.text().await?;

        let document = scraper::Html::parse_document(&html);

        let selectors = [".result__a", ".result-title a", "a.result__url"];

        let snippet_selectors = [".result__snippet", ".result-snippet", ".result__body"];

        let mut results = Vec::new();

        for sel_str in &selectors {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                for (i, element) in document.select(&sel).take(limit).enumerate() {
                    let title = element.text().collect::<String>().trim().to_string();
                    let href = element.attr("href").unwrap_or("").to_string();
                    let url = extract_ddg_url(&href);

                    let mut snippet = String::new();
                    for snip_sel in &snippet_selectors {
                        if let Ok(s) = scraper::Selector::parse(snip_sel) {
                            if let Some(parent) = element.parent() {
                                if let Some(grandparent) = parent.parent() {
                                    if let Some(snip) = grandparent.select(&s).next() {
                                        snippet =
                                            snip.text().collect::<String>().trim().to_string();
                                        if !snippet.is_empty() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !url.is_empty() && url.starts_with("http") && !title.is_empty() {
                        results.push(RawResult {
                            title,
                            url,
                            snippet,
                            engine: "duckduckgo".to_string(),
                            position: i,
                        });
                    }
                }
                if !results.is_empty() {
                    break;
                }
            }
        }

        Ok(results)
    }

    async fn search_brave(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let url = format!(
            "https://search.brave.com/search?q={}",
            urlencoding::encode(query)
        );

        let resp = self.client().get(&url).send().await?;
        let html = resp.text().await?;

        let document = scraper::Html::parse_document(&html);

        let title_selectors = [".snippet-title", "[data-testid='title']", "a.result-header"];
        let snippet_selectors = [
            ".snippet-description",
            ".snippet-content",
            "[data-testid='description']",
        ];

        let mut results = Vec::new();

        for sel_str in &title_selectors {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                for (i, element) in document.select(&sel).take(limit).enumerate() {
                    let title = element.text().collect::<String>().trim().to_string();

                    let url = if let Ok(a_sel) = scraper::Selector::parse("a") {
                        element
                            .select(&a_sel)
                            .next()
                            .and_then(|a| a.attr("href"))
                            .or_else(|| element.attr("href"))
                            .unwrap_or("")
                            .to_string()
                    } else {
                        element.attr("href").unwrap_or("").to_string()
                    };

                    let mut snippet = String::new();
                    for snip_sel in &snippet_selectors {
                        if let Ok(s) = scraper::Selector::parse(snip_sel) {
                            if let Some(parent) = element.parent() {
                                if let Some(snip) = parent.select(&s).next() {
                                    snippet = snip.text().collect::<String>().trim().to_string();
                                    if !snippet.is_empty() {
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    if !url.is_empty() && url.starts_with("http") && !title.is_empty() {
                        results.push(RawResult {
                            title,
                            url,
                            snippet,
                            engine: "brave".to_string(),
                            position: i,
                        });
                    }
                }
                if !results.is_empty() {
                    break;
                }
            }
        }

        Ok(results)
    }

    async fn search_mojek(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let url = format!(
            "https://www.mojeek.com/search?q={}",
            urlencoding::encode(query)
        );

        let resp = self.client().get(&url).send().await?;
        let html = resp.text().await?;

        let document = scraper::Html::parse_document(&html);

        let container_selectors = [".ob", ".results-standard", "li.ob"];
        let link_selectors = ["a", ".title a", "h3 a"];
        let snippet_selectors = [".s", ".snippet", ".description"];

        let mut results = Vec::new();

        for sel_str in &container_selectors {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                for (i, element) in document.select(&sel).take(limit).enumerate() {
                    let mut title = String::new();
                    let mut url = String::new();

                    for link_sel in &link_selectors {
                        if let Ok(s) = scraper::Selector::parse(link_sel) {
                            if let Some(a) = element.select(&s).next() {
                                title = a.text().collect::<String>().trim().to_string();
                                url = a.attr("href").unwrap_or("").to_string();
                                if !title.is_empty() && !url.is_empty() {
                                    break;
                                }
                            }
                        }
                    }

                    let mut snippet = String::new();
                    for snip_sel in &snippet_selectors {
                        if let Ok(s) = scraper::Selector::parse(snip_sel) {
                            if let Some(el) = element.select(&s).next() {
                                snippet = el.text().collect::<String>().trim().to_string();
                                if !snippet.is_empty() {
                                    break;
                                }
                            }
                        }
                    }

                    if !url.is_empty() && url.starts_with("http") && !title.is_empty() {
                        results.push(RawResult {
                            title,
                            url,
                            snippet,
                            engine: "mojeek".to_string(),
                            position: i,
                        });
                    }
                }
                if !results.is_empty() {
                    break;
                }
            }
        }

        Ok(results)
    }

    async fn search_yahoo(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let url = format!(
            "https://search.yahoo.com/search?p={}",
            urlencoding::encode(query)
        );

        let resp = self.client().get(&url).send().await?;
        let html = resp.text().await?;

        let document = scraper::Html::parse_document(&html);

        let title_selectors = [".algo-title", "h3.title a", ".compTitle a"];
        let snippet_selectors = [".compText", ".compText p", ".Abstract"];

        let mut results = Vec::new();

        for sel_str in &title_selectors {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                for (i, element) in document.select(&sel).take(limit).enumerate() {
                    let title = element.text().collect::<String>().trim().to_string();

                    let url = if let Ok(a_sel) = scraper::Selector::parse("a") {
                        element
                            .select(&a_sel)
                            .next()
                            .and_then(|a| a.attr("href"))
                            .or_else(|| element.attr("href"))
                            .unwrap_or("")
                            .to_string()
                    } else {
                        element.attr("href").unwrap_or("").to_string()
                    };

                    let mut snippet = String::new();
                    for snip_sel in &snippet_selectors {
                        if let Ok(s) = scraper::Selector::parse(snip_sel) {
                            if let Some(parent) = element.parent() {
                                if let Some(grandparent) = parent.parent() {
                                    if let Some(snip) = grandparent.select(&s).next() {
                                        snippet =
                                            snip.text().collect::<String>().trim().to_string();
                                        if !snippet.is_empty() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !url.is_empty() && url.starts_with("http") && !title.is_empty() {
                        results.push(RawResult {
                            title,
                            url,
                            snippet,
                            engine: "yahoo".to_string(),
                            position: i,
                        });
                    }
                }
                if !results.is_empty() {
                    break;
                }
            }
        }

        Ok(results)
    }

    async fn search_startpage(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RawResult>, reqwest::Error> {
        let url = format!(
            "https://www.startpage.com/do/dsearch?query={}&cat=web",
            urlencoding::encode(query)
        );

        let resp = self.client().get(&url).send().await?;
        let html = resp.text().await?;

        let document = scraper::Html::parse_document(&html);

        let title_selectors = [".w-gl__result-title", ".result-title", "h3.result-title"];
        let snippet_selectors = [".w-gl__description", ".result-snippet", ".description"];

        let mut results = Vec::new();

        for sel_str in &title_selectors {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                for (i, element) in document.select(&sel).take(limit).enumerate() {
                    let title = element.text().collect::<String>().trim().to_string();

                    let url = if let Ok(a_sel) = scraper::Selector::parse("a") {
                        element
                            .select(&a_sel)
                            .next()
                            .and_then(|a| a.attr("href"))
                            .or_else(|| element.attr("href"))
                            .unwrap_or("")
                            .to_string()
                    } else {
                        element.attr("href").unwrap_or("").to_string()
                    };

                    let mut snippet = String::new();
                    for snip_sel in &snippet_selectors {
                        if let Ok(s) = scraper::Selector::parse(snip_sel) {
                            if let Some(parent) = element.parent() {
                                if let Some(grandparent) = parent.parent() {
                                    if let Some(snip) = grandparent.select(&s).next() {
                                        snippet =
                                            snip.text().collect::<String>().trim().to_string();
                                        if !snippet.is_empty() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !url.is_empty() && url.starts_with("http") && !title.is_empty() {
                        results.push(RawResult {
                            title,
                            url,
                            snippet,
                            engine: "startpage".to_string(),
                            position: i,
                        });
                    }
                }
                if !results.is_empty() {
                    break;
                }
            }
        }

        Ok(results)
    }
}

fn extract_ddg_url(href: &str) -> String {
    if let Ok(url) = Url::parse(href) {
        if let Some(uddg) = url.query_pairs().find(|(k, _)| k == "uddg") {
            return uddg.to_string();
        }
    }
    if href.starts_with("http") {
        return href.to_string();
    }
    String::new()
}

fn merge_and_rank(raw: &mut Vec<RawResult>, query: &str, limit: usize) -> Vec<SearchResult> {
    let mut seen_urls: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<SearchResult> = Vec::new();

    raw.sort_by(|a, b| a.url.cmp(&b.url));

    for result in raw.iter() {
        let normalized = normalize_search_url(&result.url);
        let entry = seen_urls.entry(normalized).or_insert(merged.len());

        if *entry == merged.len() {
            merged.push(SearchResult {
                title: result.title.clone(),
                url: result.url.clone(),
                snippet: result.snippet.clone(),
                score: 0.0,
                engines: vec![result.engine.clone()],
            });
        } else {
            if merged[*entry].engines.contains(&result.engine) {
                continue;
            }
            merged[*entry].engines.push(result.engine.clone());
            if result.snippet.len() > merged[*entry].snippet.len() {
                merged[*entry].snippet = result.snippet.clone();
            }
        }
    }

    let query_lower = query.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

    for result in merged.iter_mut() {
        let engine_count = result.engines.len();
        let consensus_boost = 0.15 * (engine_count as f64 - 1.0).max(0.0);

        let title_lower = result.title.to_lowercase();
        let snippet_lower = result.snippet.to_lowercase();
        let title_relevance: f64 = query_terms
            .iter()
            .map(|term| {
                let mut score = 0.0;
                if title_lower.contains(term) {
                    score += 0.3;
                }
                if snippet_lower.contains(term) {
                    score += 0.1;
                }
                score
            })
            .sum();

        let position_bonus = if result.engines.len() > 1 { 0.1 } else { 0.0 };

        result.score = consensus_boost + title_relevance + position_bonus;
    }

    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen_domains: HashSet<String> = HashSet::new();
    let mut diversified = Vec::new();
    for result in merged {
        let domain = extract_domain(&result.url);
        if seen_domains.contains(&domain) && diversified.len() < 3 {
            continue;
        }
        seen_domains.insert(domain);
        diversified.push(result);
        if diversified.len() >= limit {
            break;
        }
    }

    diversified
}

fn normalize_search_url(url: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url) {
        let _ = parsed.set_query(None);
        let _ = parsed.set_fragment(None);
        let path = parsed.path().to_string();
        if path.len() > 1 && path.ends_with('/') {
            let _ = parsed.set_path(&path[..path.len() - 1]);
        }
        parsed.to_string()
    } else {
        url.to_string()
    }
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
    fn test_extract_ddg_url_with_uddg() {
        let href = "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=abc";
        assert_eq!(extract_ddg_url(href), "https://example.com");
    }

    #[test]
    fn test_extract_ddg_url_direct() {
        let href = "https://example.com/page";
        assert_eq!(extract_ddg_url(href), "https://example.com/page");
    }

    #[test]
    fn test_normalize_search_url() {
        let url = "https://example.com/page?utm_source=twitter&id=1";
        let normalized = normalize_search_url(url);
        assert_eq!(normalized, "https://example.com/page");
    }

    #[test]
    fn test_merge_deduplicates_urls() {
        let mut raw = vec![
            RawResult {
                title: "Title A".into(),
                url: "https://example.com/page".into(),
                snippet: "snippet a".into(),
                engine: "duckduckgo".into(),
                position: 0,
            },
            RawResult {
                title: "Title A2".into(),
                url: "https://example.com/page?ref=1".into(),
                snippet: "longer snippet here".into(),
                engine: "brave".into(),
                position: 0,
            },
        ];
        let merged = merge_and_rank(&mut raw, "example", 10);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].engines.len(), 2);
        assert_eq!(merged[0].snippet, "longer snippet here");
    }

    #[test]
    fn test_diversify_domains() {
        let mut raw: Vec<RawResult> = (0..10)
            .map(|i| RawResult {
                title: format!("Result {i}"),
                url: format!("https://same.com/page{i}"),
                snippet: format!("snippet {i}"),
                engine: "duckduckgo".into(),
                position: i,
            })
            .collect();
        raw.push(RawResult {
            title: "Different".into(),
            url: "https://different.com/page".into(),
            snippet: "different".into(),
            engine: "brave".into(),
            position: 0,
        });
        let merged = merge_and_rank(&mut raw, "test", 10);
        let domains: Vec<&str> = merged
            .iter()
            .map(|r| extract_domain(&r.url).as_str())
            .collect();
        assert!(domains.contains(&"different.com"));
    }
}

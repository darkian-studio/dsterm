use super::cache::{CacheEntry, CacheKey, ContentCache};
use super::models::FetchResponse;
use super::permissions::{NetworkPermissions, PermissionError};
use regex::Regex;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

type NodeRef<'a> = ego_tree::NodeRef<'a, scraper::Node>;

const MAX_RETRIES: u32 = 3;
const BASE_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RESPONSE_BYTES: usize = 10_000_000;

const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
];

const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "fbclid",
    "gclid",
    "mc_cid",
    "mc_eid",
    "_ga",
    "_gl",
    "yclid",
    "msclkid",
    "twclid",
    "li_fat_id",
    "igshid",
];

pub struct HttpService {
    clients: Vec<Client>,
    permissions: NetworkPermissions,
    cache: Arc<ContentCache>,
    counter: std::sync::atomic::AtomicU64,
}

impl HttpService {
    pub fn new(permissions: NetworkPermissions, cache: Arc<ContentCache>) -> Self {
        let clients: Vec<Client> = USER_AGENTS
            .iter()
            .map(|ua| {
                Client::builder()
                    .timeout(Duration::from_secs(30))
                    .connect_timeout(Duration::from_secs(10))
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .user_agent(*ua)
                    .build()
                    .expect("failed to build HTTP client")
            })
            .collect();

        Self {
            clients,
            permissions,
            cache,
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn client(&self) -> &Client {
        let idx = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        &self.clients[(idx as usize) % self.clients.len()]
    }

    pub async fn fetch(
        &self,
        url: &str,
        css_selector: Option<&str>,
        _focus: Option<&str>,
        max_bytes: usize,
    ) -> Result<FetchResponse, FetchError> {
        self.permissions.check(url)?;

        let cache_key = CacheKey {
            operation: "fetch".to_string(),
            url: normalize_url(url),
            extra: css_selector.unwrap_or("").to_string(),
        };

        if let Some(cached) = self.cache.get(&cache_key).await {
            let body = String::from_utf8_lossy(&cached.data).to_string();
            let markdown = html_to_markdown(&body);
            let title = extract_title(&body);
            return Ok(FetchResponse {
                url: cached.url,
                status: cached.status,
                content_type: cached.content_type,
                html: body,
                markdown,
                title,
                metadata: HashMap::new(),
                content_ok: true,
                from_cache: true,
                bytes_fetched: cached.data.len(),
            });
        }

        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            match self.attempt_fetch(url, max_bytes).await {
                Ok(response) => {
                    let cache_entry = CacheEntry {
                        data: response.html.as_bytes().to_vec(),
                        content_type: response.content_type.clone(),
                        status: response.status,
                        url: response.url.clone(),
                        fetched_at: Instant::now(),
                        ttl: Duration::from_secs(300),
                    };
                    self.cache.insert(&cache_key, cache_entry).await;
                    return Ok(response);
                }
                Err(FetchError::Retryable(status, retry_after)) => {
                    if attempt < MAX_RETRIES {
                        let delay =
                            retry_after.unwrap_or_else(|| BASE_RETRY_DELAY * 2u32.pow(attempt));
                        tokio::time::sleep(delay).await;
                        last_err = Some(FetchError::Retryable(status, retry_after));
                        continue;
                    }
                    last_err = Some(FetchError::HttpStatus { status });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(BASE_RETRY_DELAY * 2u32.pow(attempt)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or(FetchError::Network("all retries exhausted".into())))
    }

    async fn attempt_fetch(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<FetchResponse, FetchError> {
        let client = self.client();
        let response = client
            .get(url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Accept-Encoding", "gzip, deflate, br")
            .header("Cache-Control", "no-cache")
            .header("DNT", "1")
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;

        let status = response.status().as_u16();

        if status == 429 || status == 503 {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| Duration::from_secs(s));
            return Err(FetchError::Retryable(status, retry_after));
        }

        if status >= 400 {
            return Err(FetchError::HttpStatus { status });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string();

        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());

        if let Some(len) = content_length {
            if len > MAX_RESPONSE_BYTES {
                return Err(FetchError::Network(format!(
                    "response too large: {len} bytes (limit {MAX_RESPONSE_BYTES})"
                )));
            }
        }

        let effective_limit = max_bytes.min(MAX_RESPONSE_BYTES);

        let mut stream = response;
        let mut body_bytes: Vec<u8> = Vec::new();
        let mut byte_count = 0usize;

        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?
        {
            byte_count += chunk.len();
            if byte_count > effective_limit {
                body_bytes
                    .extend_from_slice(&chunk[..effective_limit.saturating_sub(body_bytes.len())]);
                break;
            }
            body_bytes.extend_from_slice(&chunk);
        }

        let truncated = byte_count > effective_limit;
        let _encoding = detect_encoding(&content_type, &body_bytes);
        let html = String::from_utf8_lossy(&body_bytes)
            .replace('\0', "")
            .to_owned();

        let markdown = if should_extract_markdown(&content_type) {
            html_to_markdown(&html)
        } else {
            html.clone()
        };

        let title = extract_title(&html);
        let content_ok = is_content_ok(status, &html);

        let mut metadata = HashMap::new();
        if truncated {
            metadata.insert("truncated".into(), "true".into());
        }

        Ok(FetchResponse {
            url: url.to_string(),
            status,
            content_type,
            html,
            markdown,
            title,
            metadata,
            content_ok,
            from_cache: false,
            bytes_fetched: byte_count,
        })
    }

    pub fn check_redirect(&self, original: &str, redirect: &str) -> Result<(), FetchError> {
        self.permissions.check(redirect).map_err(|e| {
            FetchError::Network(format!(
                "redirect from {original} to {redirect} blocked: {e}"
            ))
        })
    }
}

fn detect_encoding(content_type: &str, bytes: &[u8]) -> &'static str {
    if let Some(ct) = content_type
        .split(';')
        .find(|s| s.trim().starts_with("charset="))
    {
        let charset = ct.split('=').nth(1).unwrap_or("").trim().to_lowercase();
        match charset.as_str() {
            "utf-8" | "utf8" => "utf-8",
            "iso-8859-1" | "latin1" | "latin-1" => "iso-8859-1",
            "windows-1252" | "cp1252" => "windows-1252",
            "shift_jis" | "shift-jis" | "sjis" => "shift_jis",
            "euc-jp" => "euc-jp",
            "gb2312" | "gbk" | "gb18030" => "gb18030",
            "big5" => "big5",
            "euc-kr" | "euc_kr" => "euc-kr",
            _ => "utf-8",
        }
    } else if bytes.len() >= 3 && bytes[..3] == [0xEF, 0xBB, 0xBF] {
        "utf-8"
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        "utf-16le"
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        "utf-16be"
    } else {
        "utf-8"
    }
}

fn should_extract_markdown(content_type: &str) -> bool {
    content_type.contains("text/html")
        || content_type.contains("application/xhtml+xml")
        || content_type.contains("application/xml")
}

fn is_content_ok(status: u16, html: &str) -> bool {
    if status >= 400 {
        return false;
    }

    let lower = html.to_lowercase();

    let bot_patterns = [
        "just a moment",
        "checking your browser",
        "verify you are human",
        "attention required",
        "access denied",
        "blocked",
        "please wait while we verify",
        "browser check",
        "enable javascript to continue",
        "enable javascript and cookies to continue",
        "ray id:",
        "cloudflare",
        "incapsula",
        "sucuri",
        "akamai",
        "perimeterx",
        "datadome",
        "captcha",
        "recaptcha",
        "hcaptcha",
        "please complete the security check",
        "security check",
        "denied by administrator",
        "forbidden",
        "request blocked",
        "rate limit",
        "too many requests",
        "service unavailable",
        "temporarily unavailable",
    ];

    for pattern in &bot_patterns {
        if lower.contains(pattern) {
            if lower.contains("<form") || lower.contains("<input") {
                return false;
            }
            if lower.contains("cloudflare") || lower.contains("incapsula") {
                return false;
            }
        }
    }

    if lower.contains("<form") && lower.contains("captcha") {
        return false;
    }

    if lower.contains("<form") && lower.contains("challenge") {
        return false;
    }

    if html.len() < 200 && !lower.contains("<body") {
        return false;
    }

    let script_count = lower.matches("<script").count();
    let noscript = lower.contains("<noscript");
    let body_content = lower.matches("<p").count() + lower.matches("<div").count();
    if script_count > 5 && noscript && body_content < 3 {
        return false;
    }

    true
}

pub fn html_to_markdown(html: &str) -> String {
    let document = scraper::Html::parse_document(html);
    let root = document.root_element();

    let mut output = String::new();
    extract_node(&root, &mut output);

    let re_single = Regex::new(r"\n{3,}").unwrap();
    let result = re_single.replace_all(&output, "\n\n");
    result.trim().to_string()
}

fn extract_node(node: &NodeRef<'_>, output: &mut String) {
    match node.value() {
        scraper::Node::Element(el) => {
            let tag = el.name();
            match tag {
                "script" | "style" | "noscript" | "svg" | "head" | "meta" | "link" => return,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag.chars().last().unwrap() as usize - '0' as usize;
                    output.push_str(&"#".repeat(level));
                    output.push(' ');
                }
                "p" | "div" | "section" | "article" | "main" | "aside" | "footer" | "header" => {
                    output.push('\n');
                }
                "br" => {
                    output.push('\n');
                }
                "li" => {
                    output.push_str("- ");
                }
                "a" => {
                    if let Some(href) = el.attr("href") {
                        output.push('[');
                        for child in node.children() {
                            extract_node(&child, output);
                        }
                        output.push_str("](");
                        output.push_str(href);
                        output.push(']');
                        return;
                    }
                }
                "img" => {
                    if let Some(alt) = el.attr("alt") {
                        if !alt.is_empty() {
                            output.push_str(&format!("[image: {}]", alt));
                        }
                    }
                    return;
                }
                "pre" | "code" => {
                    output.push_str("\n```\n");
                }
                "table" => {
                    output.push('\n');
                    extract_table(node, output);
                    return;
                }
                "blockquote" => {
                    output.push_str("\n> ");
                }
                "hr" => {
                    output.push_str("\n---\n");
                }
                "dt" => {
                    output.push_str("**");
                }
                "dd" => {
                    output.push_str(": ");
                }
                _ => {}
            }

            for child in node.children() {
                extract_node(&child, output);
            }

            match tag {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => output.push('\n'),
                "p" | "div" | "section" | "article" | "main" | "aside" | "footer" | "header" => {
                    output.push('\n');
                }
                "li" => output.push('\n'),
                "pre" | "code" => output.push_str("\n```\n"),
                "dt" => output.push_str("**"),
                "dd" => output.push('\n'),
                "blockquote" => output.push('\n'),
                _ => {}
            }
        }
        scraper::Node::Text(text) => {
            let text = text.trim();
            if !text.is_empty() {
                output.push_str(text);
                output.push(' ');
            }
        }
        _ => {}
    }
}

fn extract_table(table_node: &NodeRef<'_>, output: &mut String) {
    let table_el = match scraper::ElementRef::wrap(*table_node) {
        Some(el) => el,
        None => return,
    };

    let tr_sel = match scraper::Selector::parse("tr") {
        Ok(s) => s,
        Err(_) => return,
    };
    let cell_sel = match scraper::Selector::parse("th, td") {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut rows: Vec<Vec<String>> = Vec::new();
    for tr in table_el.select(&tr_sel) {
        let cells: Vec<String> = tr
            .select(&cell_sel)
            .map(|cell| cell.text().collect::<String>().trim().to_string())
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }

    if rows.is_empty() {
        return;
    }

    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    for row in &rows {
        output.push_str("| ");
        for cell in row {
            output.push_str(cell);
            output.push_str(" | ");
        }
        for _ in row.len()..col_count {
            output.push_str(" | ");
        }
        output.push('\n');
    }

    output.push_str("|");
    for _ in 0..col_count {
        output.push_str("---|");
    }
    output.push('\n');
}

fn extract_title(html: &str) -> Option<String> {
    if let Some(start) = html.find("<title") {
        let after_tag = &html[start..];
        if let Some(open_end) = after_tag.find('>') {
            let content = &after_tag[open_end + 1..];
            if let Some(close) = content.find("</title") {
                let title = content[..close].trim();
                if !title.is_empty() {
                    return Some(
                        htmlescape::decode_html(title).unwrap_or_else(|_| title.to_string()),
                    );
                }
            }
        }
    }
    None
}

pub fn normalize_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };

    let host = parsed.host_str().unwrap_or("").to_lowercase();
    let _ = parsed.set_host(Some(&host));

    let params: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let k = key.to_lowercase();
            !TRACKING_PARAMS.iter().any(|tp| k == *tp)
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    parsed.query_pairs_mut().clear();
    for (k, v) in &params {
        parsed.query_pairs_mut().append_pair(k, v);
    }

    let path = parsed.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        let _ = parsed.set_path(&path[..path.len() - 1]);
    }

    parsed.to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("permission denied: {0}")]
    Permission(#[from] PermissionError),
    #[error("network error: {0}")]
    Network(String),
    #[error("HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("HTTP {0} (retryable, retry-after: {1:?})")]
    Retryable(u16, Option<Duration>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_content_ok_clean_page() {
        assert!(is_content_ok(
            200,
            "<html><body><p>Hello world</p></body></html>"
        ));
    }

    #[test]
    fn test_is_content_ok_cloudflare() {
        assert!(!is_content_ok(
            200,
            "<html><body>Just a moment... Cloudflare checking</body></html>"
        ));
    }

    #[test]
    fn test_is_content_ok_captcha() {
        assert!(!is_content_ok(
            200,
            "<html><body><form><input captcha></form></body></html>"
        ));
    }

    #[test]
    fn test_is_content_ok_status_403() {
        assert!(!is_content_ok(403, "<html><body>Forbidden</body></html>"));
    }

    #[test]
    fn test_is_content_ok_rate_limit() {
        assert!(!is_content_ok(
            200,
            "<html><body>rate limit exceeded, too many requests</body></html>"
        ));
    }

    #[test]
    fn test_is_content_ok_empty_small() {
        assert!(!is_content_ok(200, "hi"));
    }

    #[test]
    fn test_extract_title_from_html() {
        assert_eq!(
            extract_title("<html><head><title>My Title</title></head></html>"),
            Some("My Title".to_string())
        );
    }

    #[test]
    fn test_extract_title_encoded() {
        assert_eq!(
            extract_title("<html><head><title>Foo &amp; Bar</title></head></html>"),
            Some("Foo & Bar".to_string())
        );
    }

    #[test]
    fn test_normalize_url_strips_tracking() {
        let url = "https://example.com/page?utm_source=twitter&id=123";
        let normalized = normalize_url(url);
        assert!(!normalized.contains("utm_source"));
        assert!(normalized.contains("id=123"));
    }

    #[test]
    fn test_html_to_markdown_table() {
        let html = r#"
        <table>
            <tr><th>Name</th><th>Value</th></tr>
            <tr><td>A</td><td>1</td></tr>
            <tr><td>B</td><td>2</td></tr>
        </table>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("| Name | Value |"));
        assert!(md.contains("| A | 1 |"));
        assert!(md.contains("| B | 2 |"));
        assert!(md.contains("|---|"));
    }

    #[test]
    fn test_html_to_markdown_blockquote() {
        let html = "<blockquote>quoted text</blockquote>";
        let md = html_to_markdown(html);
        assert!(md.contains("> quoted text"));
    }
}

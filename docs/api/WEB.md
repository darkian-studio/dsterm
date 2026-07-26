# Web Provider API

Native web capabilities for DSTerm — fetch, extract, search, and crawl
websites without external dependencies. Built on `reqwest` (HTTP),
`scraper` (HTML parsing), and a custom bounded-BFS crawler.

The provider is **enabled by default** with sensible security defaults:
localhost, RFC1918, `file://`, and `data:` URIs are blocked. Configure
under `[web]` in `dsterm.toml`.

---

## Configuration

```toml
[web]
enabled = true          # default: true
allow_private = false   # allow localhost/RFC1918 targets
allow_file = false      # allow file:// URLs
allow_data_uri = false  # allow data: URIs
cache_size = 256        # LRU cache entries (default: 256)
```

---

## Security Model

All outbound requests are validated against `NetworkPermissions`:

| Blocked by default | Reason |
| ------------------ | ------ |
| `file://` | Local filesystem access |
| `data:` | Inline URI injection |
| `localhost`, `127.x.x.x`, `::1` | Loopback / local services |
| `10.x`, `172.16-31.x`, `192.168.x` | RFC1918 private networks |
| `fe80::`, `fc00::`, `fd00::` | IPv6 link-local / ULA |
| Explicit denied hosts | Configurable denylist |

Enable `allow_private = true` only when proxying to local services.

---

## POST /web/fetch

Fetch a URL and return raw HTML + markdown conversion.

### Request Body

```json
{
  "url": "https://example.com/article",
  "css_selector": "article",
  "focus": "rust async",
  "max_bytes": 2000000,
  "timeout_secs": 20
}
```

| Field | Type | Required | Default | Description |
| ----- | ---- | -------- | ------- | ----------- |
| `url` | string | Yes | — | Target URL. Must be public HTTP/HTTPS. |
| `css_selector` | string | No | null | CSS selector to narrow the fetched content. |
| `focus` | string | No | null | Focus query — extracts only blocks containing these terms. |
| `max_bytes` | number | No | 2000000 | Max response bytes to read. |
| `timeout_secs` | number | No | 20 | Request timeout in seconds. |

### Response (200 OK)

```json
{
  "url": "https://example.com/article",
  "status": 200,
  "content_type": "text/html; charset=utf-8",
  "html": "<article>...</article>",
  "markdown": "# Article Title\n\nBody text here...",
  "title": "Article Title",
  "content_ok": true,
  "from_cache": false
}
```

| Field | Type | Description |
| ----- | ---- | ----------- |
| `url` | string | Final URL after redirects. |
| `status` | number | HTTP status code. |
| `content_type` | string | Response Content-Type header. |
| `html` | string | Raw HTML body. |
| `markdown` | string | HTML converted to markdown (tables, links, headings preserved). |
| `title` | string or null | `<title>` tag content, decoded. |
| `content_ok` | bool | False if the page is a CAPTCHA wall, Cloudflare challenge, or error page. |
| `from_cache` | bool | True if served from the in-memory LRU cache. |

### Errors

| Status | Condition |
| ------ | --------- |
| 400 | Permission denied (private network, blocked host) |
| 400 | HTTP 4xx/5xx from upstream |
| 400 | Response exceeds `max_bytes` |
| 400 | All retries exhausted (429/503) |

---

## POST /web/extract

Fetch a URL and extract content using adaptive classification. The
server classifies the page as **article**, **list**, or **js_shell**
and applies the optimal extraction strategy.

### Request Body

Same as `/web/fetch`. The `css_selector` field is ignored (extraction
is content-adaptive).

### Response (200 OK)

```json
{
  "url": "https://docs.example.com/guide",
  "title": "Getting Started",
  "markdown": "# Getting Started\n\n1. Install...\n2. Configure...",
  "page_type": "article",
  "content_ok": true
}
```

| Field | Type | Description |
| ----- | ---- | ----------- |
| `page_type` | string | `article`, `list`, `js_shell`, or `fallback`. |
| `markdown` | string | Extracted content. For articles: paragraph text. For lists: `* [title](url)` links. |

### Page Type Classification

| Type | Heuristic |
| ---- | --------- |
| `article` | `<article>` tag + 3+ paragraphs, OR `<main>` + 5+ paragraphs + headings |
| `list` | 5+ `<li>` with 8+ links, OR tables with 5+ rows, OR `<dl>` with 5+ `<dt>` |
| `js_shell` | `<noscript>` + scripts-only, or "enable JavaScript" markers |
| `fallback` | Full body markdown extraction |

---

## POST /web/search

Keyless multi-engine web search. Queries 5 search engines in parallel
and merges results using consensus ranking.

### Request Body

```json
{
  "query": "rust async runtime comparison",
  "limit": 10,
  "focus": "tokio async-std"
}
```

| Field | Type | Required | Default | Description |
| ----- | ---- | -------- | ------- | ----------- |
| `query` | string | Yes | — | Search query. |
| `limit` | number | No | 10 | Max results to return. |
| `focus` | string | No | null | Optional focus to boost relevant results. |

### Response (200 OK)

```json
{
  "results": [
    {
      "title": "Async Rust: Tokio vs async-std",
      "url": "https://blog.example.com/async-comparison",
      "snippet": "A detailed comparison of the two major async runtimes...",
      "score": 0.85,
      "engines": ["duckduckgo", "brave", "mojeek"]
    }
  ],
  "query": "rust async runtime comparison",
  "total_results": 8
}
```

| Field | Type | Description |
| ----- | ---- | ----------- |
| `results[].title` | string | Page title. |
| `results[].url` | string | Page URL. |
| `results[].snippet` | string | Search result snippet/description. |
| `results[].score` | number | Consensus score (higher = more relevant). |
| `results[].engines` | string[] | Which engines returned this URL. |

### Ranking Algorithm

1. **Consensus boost:** +0.15 per additional engine that returned the same URL
2. **Title relevance:** +0.3 per query term found in title, +0.1 per term in snippet
3. **Multi-engine bonus:** +0.1 if result came from 2+ engines
4. **Domain diversity:** max 2 results from same domain in top positions

### Engines

| Engine | Method |
| ------ | ------ |
| DuckDuckGo | HTML scraping (`html.duckduckgo.com`) |
| Brave | HTML scraping (`search.brave.com`) |
| Mojeek | HTML scraping (`mojeek.com`) |
| Yahoo | HTML scraping (`search.yahoo.com`) |
| Startpage | HTML scraping (`startpage.com`) |

Each engine has 3 fallback CSS selector patterns and retries up to 2
times on failure. No API keys required.

---

## POST /web/crawl

Bounded best-first BFS crawl of a single domain. Returns extracted
content from multiple pages, prioritized by content relevance.

### Request Body

```json
{
  "root_url": "https://docs.example.com",
  "max_depth": 2,
  "max_pages": 10,
  "max_total_chars": 400000,
  "deadline_ms": 120000,
  "focus": "installation guide",
  "discover_only": false
}
```

| Field | Type | Required | Default | Description |
| ----- | ---- | -------- | ------- | ----------- |
| `root_url` | string | Yes | — | Starting URL. Crawl stays on same domain. |
| `max_depth` | number | No | 2 | Max link depth from root. |
| `max_pages` | number | No | 10 | Max pages to fetch. |
| `max_total_chars` | number | No | 400000 | Max total markdown characters across all pages. |
| `deadline_ms` | number | No | 120000 | Hard time limit in milliseconds. |
| `focus` | string | No | null | Boost pages whose URLs match these terms. |
| `discover_only` | bool | No | false | If true, only return discovered URLs without fetching content. |
| `crawl_urls` | string[] | No | null | Explicit URL list to fetch (bypasses BFS). |

### Response (200 OK)

```json
{
  "pages": [
    {
      "url": "https://docs.example.com/getting-started",
      "title": "Getting Started",
      "markdown": "# Getting Started\n\n...",
      "status": 200,
      "content_ok": true,
      "page_type": "article",
      "depth": 1
    }
  ],
  "root_url": "https://docs.example.com",
  "pages_crawled": 8,
  "truncated_by_time": false,
  "truncated_by_pages": false
}
```

| Field | Type | Description |
| ----- | ---- | ----------- |
| `pages` | array | Fetched and extracted pages, ordered by crawl priority. |
| `pages_crawled` | number | Total pages visited (including failures). |
| `truncated_by_time` | bool | True if deadline was reached. |
| `truncated_by_pages` | bool | True if max_pages was reached. |

### Crawl Behavior

- **Same-domain only** — links to other domains are ignored
- **Concurrent fetch** — up to 5 pages fetched in parallel (semaphore-bounded)
- **Per-page timeout** — 15 seconds per fetch; slow pages are skipped
- **Content-adaptive extraction** — each page classified and extracted independently
- **Priority queue** — URLs scored by content relevance (docs/guide/api boosted, login/cart penalized)
- **Focus-aware** — pages matching focus terms are prioritized in the queue

### URL Scoring

| Pattern | Score |
| ------- | ----- |
| `/docs`, `/guide`, `/api`, `/reference` | +0.3 |
| `/tutorial`, `/example` | +0.2–0.25 |
| `/blog`, `/post`, `/article` | +0.15 |
| `/login`, `/signup`, `/register` | -0.4 |
| `/cart`, `/checkout`, `/payment` | -0.4 to -0.5 |
| `/tag/`, `/category/`, `/author/` | -0.15 to -0.2 |
| Deep paths (>3 segments) | -0.05 per extra segment |

---

## Budgets

All endpoints respect per-request budgets via the `budgets` field:

```json
{
  "payload": { "url": "https://example.com" },
  "budgets": {
    "max_bytes": 2000000,
    "timeout_secs": 20,
    "max_pages": 5,
    "allow_browser": false
  }
}
```

| Budget | Applied to | Description |
| ------ | ---------- | ----------- |
| `max_bytes` | fetch, extract | Caps response body size. |
| `timeout_secs` | fetch, extract | Request timeout. |
| `max_pages` | search, crawl | Limits results/pages. |

---

## Caching

The fetch service uses an in-memory LRU cache (256 entries, 5-minute TTL):

- **Key:** `operation + normalized_url + selector`
- **Hit:** returns cached HTML/markdown without network request
- **Eviction:** LRU with TTL expiration on read

Cache is per-process and resets on server restart.

---

## Examples

### Fetch with CSS selector

```bash
curl -X POST http://localhost:8767/web/fetch \
  -H "Content-Type: application/json" \
  -d '{"url": "https://docs.rust-lang.org/book/", "css_selector": "main"}'
```

### Extract article content

```bash
curl -X POST http://localhost:8767/web/extract \
  -H "Content-Type: application/json" \
  -d '{"url": "https://blog.example.com/my-post", "focus": "rust ownership"}'
```

### Search

```bash
curl -X POST http://localhost:8767/web/search \
  -H "Content-Type: application/json" \
  -d '{"query": "tokio spawn vs thread::spawn", "limit": 5}'
```

### Crawl documentation site

```bash
curl -X POST http://localhost:8767/web/crawl \
  -H "Content-Type: application/json" \
  -d '{
    "root_url": "https://docs.example.com",
    "max_depth": 2,
    "max_pages": 5,
    "focus": "getting started"
  }'
```

### Discover URLs only

```bash
curl -X POST http://localhost:8767/web/crawl \
  -H "Content-Type: application/json" \
  -d '{"root_url": "https://docs.example.com", "discover_only": true, "max_depth": 1}'
```

---

## Errors

All endpoints return errors as:

```json
{
  "success": false,
  "error": "permission denied: private network (RFC1918) target 192.168.1.1 is not allowed"
}
```

Common errors:

| Error | Cause |
| ----- | ----- |
| `permission denied` | Target blocked by NetworkPermissions |
| `HTTP 429` | Rate limited by upstream (all retries exhausted) |
| `HTTP 4xx/5xx` | Upstream error after retries |
| `response too large` | Content-Length exceeds 10MB limit |
| `all retries exhausted` | Network failure on all attempts |
| `redirect blocked` | Redirect led to a blocked host/scheme |

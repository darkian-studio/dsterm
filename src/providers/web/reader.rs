use regex::Regex;

type NodeRef<'a> = ego_tree::NodeRef<'a, scraper::Node>;

#[derive(Debug, Clone, PartialEq)]
pub enum PageType {
    Article,
    List,
    JsShell,
    Fallback,
}

impl PageType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PageType::Article => "article",
            PageType::List => "list",
            PageType::JsShell => "js_shell",
            PageType::Fallback => "fallback",
        }
    }
}

pub fn classify_page(html: &str) -> PageType {
    let lower = html.to_lowercase();

    if is_js_shell(&lower) {
        return PageType::JsShell;
    }

    if is_list_page(&lower) {
        return PageType::List;
    }

    if is_article_page(&lower) {
        return PageType::Article;
    }

    PageType::Fallback
}

fn is_js_shell(lower: &str) -> bool {
    let has_noscript = lower.contains("<noscript");
    let script_count = lower.matches("<script").count();
    let p_count = lower.matches("<p").count();
    let div_count = lower.matches("<div").count();
    let body_content = p_count + div_count;

    let has_only_scripts = script_count > 3 && body_content < 5;

    let js_render_markers = [
        "javascript must be enabled",
        "enable javascript",
        "please enable javascript",
        "javascript is required",
        "you need to enable javascript",
        "please enable cookies and javascript",
        "this app requires javascript",
    ];
    let has_js_render = js_render_markers.iter().any(|m| lower.contains(m));

    (has_noscript && has_only_scripts) || has_js_render
}

fn is_list_page(lower: &str) -> bool {
    let link_count = lower.matches("<a ").count();
    let list_items = lower.matches("<li").count();

    if list_items >= 5 && link_count >= 8 {
        return true;
    }

    let has_table = lower.matches("<table").count() >= 1;
    let table_rows = lower.matches("<tr").count();
    if has_table && table_rows >= 5 {
        return true;
    }

    let dl_count = lower.matches("<dl").count();
    let dt_count = lower.matches("<dt").count();
    if dl_count >= 1 && dt_count >= 5 {
        return true;
    }

    false
}

fn is_article_page(lower: &str) -> bool {
    let has_article_tag = lower.contains("<article");
    let has_main_content = lower.contains("<main") || lower.contains("role=\"main\"");
    let paragraph_count = lower.matches("<p").count();
    let heading_count =
        lower.matches("<h1").count() + lower.matches("<h2").count() + lower.matches("<h3").count();

    if has_article_tag && paragraph_count >= 3 {
        return true;
    }

    if has_main_content && paragraph_count >= 5 && heading_count >= 1 {
        return true;
    }

    if paragraph_count >= 8 && heading_count >= 2 {
        return true;
    }

    let has_byline = lower.contains("author")
        || lower.contains("byline")
        || lower.contains("posted on")
        || lower.contains("published");
    if has_byline && paragraph_count >= 3 && heading_count >= 1 {
        return true;
    }

    false
}

pub fn extract_article(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    let article_selectors = [
        "article",
        "main",
        "[role='main']",
        ".content",
        ".post",
        ".article",
        ".entry-content",
        ".post-content",
        ".article-body",
        ".story-body",
        "#article-body",
        "#content",
    ];

    for sel_str in &article_selectors {
        if let Ok(selector) = scraper::Selector::parse(sel_str) {
            if let Some(article) = document.select(&selector).next() {
                let md = extract_element_markdown(&article);
                if md.len() > 100 {
                    return md;
                }
            }
        }
    }

    if let Ok(selector) = scraper::Selector::parse("body") {
        if let Some(body) = document.select(&selector).next() {
            return extract_element_markdown(&body);
        }
    }

    html_to_plain_markdown(html)
}

pub fn extract_list(html: &str) -> String {
    let document = scraper::Html::parse_document(html);
    let mut items = Vec::new();

    let link_selectors = ["a[href]", "a[href][rel='nofollow']", "a[href][target]"];

    for sel_str in &link_selectors {
        if let Ok(selector) = scraper::Selector::parse(sel_str) {
            for link in document.select(&selector) {
                let href = link.attr("href").unwrap_or("");
                let text = link.text().collect::<String>().trim().to_string();
                if !text.is_empty()
                    && !href.is_empty()
                    && !href.starts_with('#')
                    && !href.starts_with("javascript:")
                    && !href.starts_with("mailto:")
                    && text.len() > 2
                {
                    let abs_url = resolve_url(href, "");
                    items.push(format!("- [{}]({})", text, abs_url));
                }
            }
            if !items.is_empty() {
                break;
            }
        }
    }

    if items.is_empty() {
        html_to_plain_markdown(html)
    } else {
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(item.clone()));
        items.join("\n")
    }
}

fn extract_element_markdown(element: &scraper::ElementRef) -> String {
    let mut output = String::new();
    extract_node(element, &mut output);
    let re = Regex::new(r"\n{3,}").unwrap();
    re.replace_all(&output, "\n\n").trim().to_string()
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
                "br" => output.push('\n'),
                "li" => output.push_str("- "),
                "pre" | "code" => output.push_str("\n```\n"),
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
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();

    for child in table_node.children() {
        match child.value() {
            scraper::Node::Element(el) => match el.name() {
                "tr" => {
                    if !current_row.is_empty() {
                        rows.push(std::mem::take(&mut current_row));
                    }
                }
                "th" | "td" => {
                    let cell_text: String = if let Some(el) = scraper::ElementRef::wrap(child) {
                        el.text().collect::<String>().trim().to_string()
                    } else {
                        child.value().as_text().unwrap_or("").trim().to_string()
                    };
                    current_row.push(cell_text);
                }
                "thead" | "tbody" | "tfoot" => {
                    for inner in child.children() {
                        if let scraper::Node::Element(inner_el) = inner.value() {
                            if inner_el.name() == "tr" {
                                if !current_row.is_empty() {
                                    rows.push(std::mem::take(&mut current_row));
                                }
                                for cell in inner.children() {
                                    if let scraper::Node::Element(cell_el) = cell.value() {
                                        if cell_el.name() == "th" || cell_el.name() == "td" {
                                            let cell_text: String =
                                                if let Some(el) = scraper::ElementRef::wrap(cell) {
                                                    el.text().collect::<String>().trim().to_string()
                                                } else {
                                                    cell.value()
                                                        .as_text()
                                                        .unwrap_or("")
                                                        .trim()
                                                        .to_string()
                                                };
                                            current_row.push(cell_text);
                                        }
                                    }
                                }
                                if !current_row.is_empty() {
                                    rows.push(std::mem::take(&mut current_row));
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    if rows.is_empty() {
        return;
    }

    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    for row in &rows {
        let cells: Vec<&str> = row.iter().map(|s| s.as_str()).collect();
        output.push_str("| ");
        for cell in &cells {
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

fn html_to_plain_markdown(html: &str) -> String {
    let document = scraper::Html::parse_document(html);
    if let Ok(sel) = scraper::Selector::parse("body") {
        if let Some(body) = document.select(&sel).next() {
            return extract_element_markdown(&body);
        }
    }

    let re_tag = Regex::new(r"<[^>]+>").unwrap();
    let text = re_tag.replace_all(html, "");
    let re_space = Regex::new(r"\s+").unwrap();
    re_space.replace_all(&text, " ").trim().to_string()
}

fn resolve_url(href: &str, base: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with("//") {
        return format!("https:{}", href);
    }
    if href.starts_with("data:") || href.starts_with("javascript:") || href.starts_with("mailto:") {
        return href.to_string();
    }
    if !base.is_empty() {
        if let Ok(base_url) = reqwest::Url::parse(base) {
            if let Ok(resolved) = base_url.join(href) {
                return resolved.to_string();
            }
        }
    }
    if href.starts_with('/') {
        return format!("https:{}", href);
    }
    href.to_string()
}

pub fn apply_focus(markdown: &str, focus: &str) -> String {
    if focus.is_empty() {
        return markdown.to_string();
    }

    let focus_lower = focus.to_lowercase();
    let focus_terms: Vec<&str> = focus_lower.split_whitespace().collect();

    let blocks: Vec<&str> = markdown.split("\n\n").collect();
    let mut scored: Vec<(usize, &str)> = blocks
        .iter()
        .map(|block| {
            let block_lower = block.to_lowercase();
            let score = focus_terms
                .iter()
                .filter(|term| block_lower.contains(**term))
                .count();
            (score, *block)
        })
        .filter(|(score, _)| *score > 0)
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));

    if scored.is_empty() {
        return markdown.to_string();
    }

    let result: Vec<&str> = scored
        .into_iter()
        .take(30)
        .map(|(_, block)| block)
        .collect();
    result.join("\n\n")
}

pub fn extract_meta_description(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);

    let selectors = [
        "meta[name='description']",
        "meta[property='og:description']",
        "meta[name='twitter:description']",
    ];

    for sel_str in &selectors {
        if let Ok(sel) = scraper::Selector::parse(sel_str) {
            if let Some(meta) = document.select(&sel).next() {
                if let Some(content) = meta.attr("content") {
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_article() {
        let html = r#"
        <html><body>
        <article>
            <h1>Test Article</h1>
            <p>This is a paragraph about something important.</p>
            <p>This is another paragraph with more details.</p>
            <p>A third paragraph to meet the threshold.</p>
            <p>Fourth paragraph.</p>
            <p>Fifth paragraph.</p>
        </article>
        </body></html>
        "#;
        assert_eq!(classify_page(html), PageType::Article);
    }

    #[test]
    fn classify_js_shell() {
        let html = r#"
        <html><head><noscript>Enable JavaScript</noscript></head>
        <body>
            <script src="app.js"></script>
            <script src="vendor.js"></script>
            <script src="polyfill.js"></script>
            <script>require(['app'])</script>
            <div id="root"></div>
        </body></html>
        "#;
        assert_eq!(classify_page(html), PageType::JsShell);
    }

    #[test]
    fn classify_list() {
        let html = r#"
        <html><body>
        <ul>
            <li><a href="/1">Item 1</a></li>
            <li><a href="/2">Item 2</a></li>
            <li><a href="/3">Item 3</a></li>
            <li><a href="/4">Item 4</a></li>
            <li><a href="/5">Item 5</a></li>
            <li><a href="/6">Item 6</a></li>
            <li><a href="/7">Item 7</a></li>
            <li><a href="/8">Item 8</a></li>
            <li><a href="/9">Item 9</a></li>
        </ul>
        </body></html>
        "#;
        assert_eq!(classify_page(html), PageType::List);
    }

    #[test]
    fn focus_extracts_relevant_blocks() {
        let markdown = "Introduction paragraph.\n\nRust is a systems language.\n\nPython is used for data science.\n\nRust has memory safety.\n\nConclusion paragraph.";
        let result = apply_focus(markdown, "rust");
        assert!(result.contains("Rust is a systems language"));
        assert!(result.contains("Rust has memory safety"));
        assert!(!result.contains("Python"));
    }

    #[test]
    fn test_extract_table() {
        let html = r#"<table>
            <tr><th>Name</th><th>Age</th></tr>
            <tr><td>Alice</td><td>30</td></tr>
            <tr><td>Bob</td><td>25</td></tr>
        </table>"#;
        let document = scraper::Html::parse_document(html);
        let sel = scraper::Selector::parse("table").unwrap();
        if let Some(table) = document.select(&sel).next() {
            let mut output = String::new();
            extract_table(&*table, &mut output);
            assert!(output.contains("| Name | Age |"));
            assert!(output.contains("| Alice | 30 |"));
            assert!(output.contains("| Bob | 25 |"));
        }
    }

    #[test]
    fn test_resolve_url_relative() {
        assert_eq!(
            resolve_url("/page", "https://example.com"),
            "https://example.com/page"
        );
    }

    #[test]
    fn test_resolve_url_mailto() {
        assert_eq!(
            resolve_url("mailto:test@example.com", ""),
            "mailto:test@example.com"
        );
    }

    #[test]
    fn test_extract_meta_description() {
        let html = r#"<html><head>
            <meta name="description" content="A great article about Rust">
        </head><body></body></html>"#;
        assert_eq!(
            extract_meta_description(html),
            Some("A great article about Rust".to_string())
        );
    }
}

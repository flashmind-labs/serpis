//! `web_search` tool: client-side search with pluggable backends (Brave, Exa, Tavily, DuckDuckGo).
//! When the active model can search natively, the agent asks the provider instead and this tool
//! is not offered (see `Agent::native_web_search`).

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::*;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub num_results: usize,
    pub domain_filter: Vec<String>,
    pub recency: Option<String>,
}

/// A search backend. Implement this in a plugin to add providers.
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &str;
    fn available(&self) -> bool;
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>>;
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn client() -> &'static reqwest::Client {
    serpis_ai::stream::http_client()
}

async fn check(resp: reqwest::Response, who: &str) -> anyhow::Result<Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{who} search error {}: {}", status.as_u16(), serpis_ai::stream::truncate_chars(&body, 300));
    }
    Ok(serde_json::from_str(&body)?)
}

pub struct Brave;
impl SearchBackend for Brave {
    fn id(&self) -> &str {
        "brave"
    }
    fn available(&self) -> bool {
        env("BRAVE_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = env("BRAVE_API_KEY").ok_or_else(|| anyhow::anyhow!("BRAVE_API_KEY is not set"))?;
            let base = env("BRAVE_BASE_URL").unwrap_or_else(|| "https://api.search.brave.com/res/v1".into());
            let mut q = query.clone();
            for d in &opts.domain_filter {
                q.push_str(&if let Some(d) = d.strip_prefix('-') { format!(" -site:{d}") } else { format!(" site:{d}") });
            }
            let mut params = vec![("q", q), ("count", opts.num_results.to_string())];
            if let Some(r) = &opts.recency {
                params.push(("freshness", match r.as_str() { "day" => "pd", "week" => "pw", "month" => "pm", _ => "py" }.into()));
            }
            let resp = client()
                .get(format!("{base}/web/search"))
                .query(&params)
                .header("Accept", "application/json")
                .header("X-Subscription-Token", key)
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            let v = check(resp, "Brave").await?;
            Ok(v.pointer("/web/results")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|r| SearchResult {
                            title: r["title"].as_str().unwrap_or("").into(),
                            url: r["url"].as_str().unwrap_or("").into(),
                            snippet: strip_tags(r["description"].as_str().unwrap_or("")),
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
        .boxed()
    }
}

pub struct Exa;
impl SearchBackend for Exa {
    fn id(&self) -> &str {
        "exa"
    }
    fn available(&self) -> bool {
        env("EXA_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = env("EXA_API_KEY").ok_or_else(|| anyhow::anyhow!("EXA_API_KEY is not set"))?;
            let base = env("EXA_BASE_URL").unwrap_or_else(|| "https://api.exa.ai".into());
            let (include, exclude): (Vec<_>, Vec<_>) = opts.domain_filter.iter().partition(|d| !d.starts_with('-'));
            let mut body = json!({"query": query, "numResults": opts.num_results, "contents": {"highlights": true}});
            if !include.is_empty() {
                body["includeDomains"] = json!(include);
            }
            if !exclude.is_empty() {
                body["excludeDomains"] = json!(exclude.iter().map(|d| d.trim_start_matches('-')).collect::<Vec<_>>());
            }
            let resp = client()
                .post(format!("{base}/search"))
                .header("x-api-key", key)
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            let v = check(resp, "Exa").await?;
            Ok(v["results"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| SearchResult {
                            title: r["title"].as_str().unwrap_or("").into(),
                            url: r["url"].as_str().unwrap_or("").into(),
                            snippet: r["highlights"]
                                .as_array()
                                .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" … "))
                                .or_else(|| r["text"].as_str().map(|t| t.chars().take(400).collect()))
                                .unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
        .boxed()
    }
}

pub struct Tavily;
impl SearchBackend for Tavily {
    fn id(&self) -> &str {
        "tavily"
    }
    fn available(&self) -> bool {
        env("TAVILY_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = env("TAVILY_API_KEY").ok_or_else(|| anyhow::anyhow!("TAVILY_API_KEY is not set"))?;
            let base = env("TAVILY_BASE_URL").unwrap_or_else(|| "https://api.tavily.com".into());
            let (include, exclude): (Vec<_>, Vec<_>) = opts.domain_filter.iter().partition(|d| !d.starts_with('-'));
            let mut body = json!({"query": query, "max_results": opts.num_results});
            if !include.is_empty() {
                body["include_domains"] = json!(include);
            }
            if !exclude.is_empty() {
                body["exclude_domains"] = json!(exclude.iter().map(|d| d.trim_start_matches('-')).collect::<Vec<_>>());
            }
            if let Some(r) = &opts.recency {
                body["time_range"] = json!(r);
            }
            let resp = client()
                .post(format!("{base}/search"))
                .bearer_auth(key)
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            let v = check(resp, "Tavily").await?;
            Ok(v["results"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| SearchResult {
                            title: r["title"].as_str().unwrap_or("").into(),
                            url: r["url"].as_str().unwrap_or("").into(),
                            snippet: r["content"].as_str().unwrap_or("").into(),
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
        .boxed()
    }
}

/// Keyless fallback that scrapes DuckDuckGo's HTML endpoint.
pub struct DuckDuckGo;
impl SearchBackend for DuckDuckGo {
    fn id(&self) -> &str {
        "duckduckgo"
    }
    fn available(&self) -> bool {
        true
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let resp = client()
                .get("https://html.duckduckgo.com/html/")
                .query(&[("q", &query)])
                .header("Accept", "text/html")
                .header("User-Agent", "Mozilla/5.0 (compatible; serpis/0.1)")
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            let status = resp.status().as_u16();
            let html = resp.text().await?;
            if status == 202 || html.contains("anomaly-modal__box") || html.contains("challenge-form") {
                anyhow::bail!("DuckDuckGo blocked the request with a bot check (HTTP {status}, rate limited); use another search provider");
            }
            if !(200..300).contains(&status) {
                anyhow::bail!("DuckDuckGo search error {status}");
            }
            Ok(parse_ddg(&html, &opts))
        }
        .boxed()
    }
}

fn strip_tags(s: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    html_escape::decode_html_entities(&re.replace_all(s, "")).trim().to_string()
}

fn host_matches(url: &str, domain: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .is_some_and(|h| h == domain || h.ends_with(&format!(".{domain}")))
}

fn passes_filter(url: &str, filters: &[String]) -> bool {
    let allowed: Vec<&str> = filters.iter().filter(|d| !d.starts_with('-')).map(String::as_str).collect();
    let blocked: Vec<&str> = filters.iter().filter_map(|d| d.strip_prefix('-')).collect();
    (allowed.is_empty() || allowed.iter().any(|d| host_matches(url, d))) && !blocked.iter().any(|d| host_matches(url, d))
}

fn parse_ddg(html: &str, opts: &SearchOptions) -> Vec<SearchResult> {
    let block_re = regex::Regex::new(r#"(?s)<div class="result results_links[^"]*"(.*?)<div class="clear">"#).unwrap();
    let a_re = regex::Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let snip_re = regex::Regex::new(r#"(?s)class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();
    let mut out = Vec::new();
    for block in block_re.captures_iter(html) {
        let body = &block[1];
        if body.contains("result--ad") {
            continue;
        }
        let Some(a) = a_re.captures(body) else { continue };
        let href = html_escape::decode_html_entities(&a[1]).into_owned();
        let link = url::Url::parse("https://html.duckduckgo.com/html/").unwrap().join(&href).ok();
        let Some(dest) = link.and_then(|l| {
            l.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned()).or_else(|| Some(l.to_string()))
        }) else {
            continue;
        };
        if !(dest.starts_with("http://") || dest.starts_with("https://")) || !passes_filter(&dest, &opts.domain_filter) {
            continue;
        }
        out.push(SearchResult {
            title: strip_tags(&a[2]),
            url: dest,
            snippet: snip_re.captures(body).map(|s| strip_tags(&s[1])).unwrap_or_default(),
        });
        if out.len() >= opts.num_results {
            break;
        }
    }
    out
}

pub struct WebSearchTool {
    pub backends: Vec<Arc<dyn SearchBackend>>,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        WebSearchTool {
            backends: vec![Arc::new(Brave), Arc::new(Exa), Arc::new(Tavily), Arc::new(DuckDuckGo)],
        }
    }
}

impl WebSearchTool {
    fn pick(&self, requested: Option<&str>) -> anyhow::Result<Arc<dyn SearchBackend>> {
        let requested = requested.map(str::to_string).or_else(|| env("SERPIS_WEB_SEARCH_PROVIDER"));
        if let Some(id) = requested.filter(|r| r != "auto") {
            return self
                .backends
                .iter()
                .find(|b| b.id() == id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Unknown search provider: {id}"));
        }
        self.backends
            .iter()
            .find(|b| b.available())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No web search provider is available"))
    }
}

impl AgentTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> String {
        "Search the web. Returns titles, URLs and snippets for each query. Use several focused queries for broad research. Use domainFilter to restrict (\"example.com\") or exclude (\"-example.com\") sites.".into()
    }
    fn parameters(&self) -> Value {
        let ids: Vec<&str> = self.backends.iter().map(|b| b.id()).collect();
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "Several queries to run in parallel (instead of query)"},
                "numResults": {"type": "number", "description": "Results per query (default: 5, max: 20)"},
                "recencyFilter": {"type": "string", "enum": ["day", "week", "month", "year"], "description": "Only recent results"},
                "domainFilter": {"type": "array", "items": {"type": "string"}, "description": "Domains to include, or exclude with a leading '-'"},
                "provider": {"type": "string", "enum": ids, "description": "Search backend (default: first configured)"}
            }
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Search the web for current information".into())
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        let backend = self.pick(arg_str(&args, "provider"));
        async move {
            let backend = backend?;
            let mut queries: Vec<String> = args
                .get("queries")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            if let Some(q) = arg_str(&args, "query") {
                queries.insert(0, q.to_string());
            }
            queries.retain(|q| !q.trim().is_empty());
            if queries.is_empty() {
                anyhow::bail!("Provide query or queries");
            }
            let opts = SearchOptions {
                num_results: arg_u64(&args, "numResults").unwrap_or(5).clamp(1, 20) as usize,
                domain_filter: args
                    .get("domainFilter")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_lowercase()).collect())
                    .unwrap_or_default(),
                recency: arg_str(&args, "recencyFilter").map(str::to_string),
            };
            let futs = queries.iter().map(|q| backend.search(q.clone(), opts.clone()));
            let results = tokio::select! {
                _ = ctx.cancel.cancelled() => anyhow::bail!("Operation aborted"),
                r = futures::future::join_all(futs) => r,
            };
            let mut text = String::new();
            let mut structured = Vec::new();
            let mut all_failed = true;
            for (q, r) in queries.iter().zip(results) {
                text.push_str(&format!("## {q}\n"));
                match r {
                    Ok(list) => {
                        all_failed = false;
                        if list.is_empty() {
                            text.push_str("(no results)\n");
                        }
                        for (i, r) in list.iter().enumerate() {
                            text.push_str(&format!("{}. {}\n   {}\n", i + 1, r.title, r.url));
                            if !r.snippet.is_empty() {
                                text.push_str(&format!("   {}\n", r.snippet.replace('\n', " ")));
                            }
                        }
                        structured.push(json!({"query": q, "provider": backend.id(), "results": list.iter().map(|r| json!({"title": r.title, "url": r.url, "snippet": r.snippet})).collect::<Vec<_>>()}));
                    }
                    Err(e) => {
                        text.push_str(&format!("Error: {e:#}\n"));
                        structured.push(json!({"query": q, "provider": backend.id(), "error": format!("{e:#}")}));
                    }
                }
                text.push('\n');
            }
            let result = ToolResult::text(text.trim_end().to_string()).with_details(json!({"queries": structured, "provider": backend.id()}));
            Ok(if all_failed { ToolResult { is_error: true, ..result } } else { result })
        }
        .boxed()
    }
}

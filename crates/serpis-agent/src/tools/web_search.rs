//! `web_search` tool: client-side search with pluggable backends (Brave, Exa, Tavily, Firecrawl,
//! DuckDuckGo). A provider that fails every query hands them to the next one configured; when all
//! of them fail, the result says so (`details.unavailable`), and the agent loop can stop the run
//! there (`AgentConfig::web_search_required`). When the active model can search natively, the
//! agent asks the provider instead and this tool is not offered (see `Agent::native_web_search`).

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use serpis_ai::stream::{ProviderError, retry_delay};

use super::*;

const SEARCH_RETRIES: u32 = 2;

async fn search_with_retry(backend: Arc<dyn SearchBackend>, query: String, opts: SearchOptions) -> anyhow::Result<Vec<SearchResult>> {
    let mut attempt = 0;
    loop {
        let result = backend.search(query.clone(), opts.clone()).await;
        let Err(e) = &result else { return result };
        match retry_delay(e, attempt).filter(|_| attempt < SEARCH_RETRIES) {
            Some(delay) => tokio::time::sleep(delay.min(Duration::from_secs(2))).await,
            None => return result,
        }
        attempt += 1;
    }
}

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
    fn parallel(&self) -> bool {
        true
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>>;
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

static KEYS: std::sync::OnceLock<serpis_ai::Registry> = std::sync::OnceLock::new();

pub fn use_registry(registry: serpis_ai::Registry) {
    let _ = KEYS.set(registry);
}

fn api_key(provider: &str, var: &str) -> Option<String> {
    KEYS.get().and_then(|r| r.api_key(provider)).filter(|k| !k.trim().is_empty()).or_else(|| env(var))
}

fn missing(provider: &str, var: &str) -> anyhow::Error {
    anyhow::anyhow!("No {provider} key: add \"{provider}\" to auth.json or set {var}")
}

fn client() -> &'static reqwest::Client {
    serpis_ai::stream::http_client()
}

async fn check(resp: reqwest::Response, who: &str) -> anyhow::Result<Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = format!("{who} search error {}: {}", status.as_u16(), serpis_ai::stream::truncate_chars(&body, 300));
        return Err(ProviderError { status: status.as_u16(), message, retry_after: None }.into());
    }
    Ok(serde_json::from_str(&body)?)
}

pub struct Brave;
impl SearchBackend for Brave {
    fn id(&self) -> &str {
        "brave"
    }
    fn available(&self) -> bool {
        api_key("brave", "BRAVE_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = api_key("brave", "BRAVE_API_KEY").ok_or_else(|| missing("brave", "BRAVE_API_KEY"))?;
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
        api_key("exa", "EXA_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = api_key("exa", "EXA_API_KEY").ok_or_else(|| missing("exa", "EXA_API_KEY"))?;
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
        api_key("tavily", "TAVILY_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = api_key("tavily", "TAVILY_API_KEY").ok_or_else(|| missing("tavily", "TAVILY_API_KEY"))?;
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

pub struct Firecrawl;
impl SearchBackend for Firecrawl {
    fn id(&self) -> &str {
        "firecrawl"
    }
    fn available(&self) -> bool {
        api_key("firecrawl", "FIRECRAWL_API_KEY").is_some()
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let key = api_key("firecrawl", "FIRECRAWL_API_KEY").ok_or_else(|| missing("firecrawl", "FIRECRAWL_API_KEY"))?;
            let base = env("FIRECRAWL_BASE_URL").unwrap_or_else(|| "https://api.firecrawl.dev".into());
            let resp = client()
                .post(format!("{base}/v2/search"))
                .bearer_auth(key)
                .json(&firecrawl_body(&query, &opts))
                .timeout(Duration::from_secs(60))
                .send()
                .await?;
            Ok(firecrawl_results(&check(resp, "Firecrawl").await?, &opts))
        }
        .boxed()
    }
}

fn firecrawl_body(query: &str, opts: &SearchOptions) -> Value {
    let (include, exclude): (Vec<_>, Vec<_>) = opts.domain_filter.iter().partition(|d| !d.starts_with('-'));
    let mut body = json!({"query": query, "limit": opts.num_results, "sources": [{"type": "web"}]});
    if !include.is_empty() {
        body["includeDomains"] = json!(include);
    } else if !exclude.is_empty() {
        body["excludeDomains"] = json!(exclude.iter().map(|d| d.trim_start_matches('-')).collect::<Vec<_>>());
    }
    if let Some(r) = &opts.recency {
        body["tbs"] = json!(match r.as_str() { "day" => "qdr:d", "week" => "qdr:w", "month" => "qdr:m", _ => "qdr:y" });
    }
    body
}

fn firecrawl_results(v: &Value, opts: &SearchOptions) -> Vec<SearchResult> {
    let web = v.pointer("/data/web").or_else(|| v.get("data").filter(|d| d.is_array()));
    web.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| SearchResult {
                    title: r["title"].as_str().unwrap_or("").into(),
                    url: r["url"].as_str().unwrap_or("").into(),
                    snippet: r["description"].as_str().unwrap_or("").into(),
                })
                .filter(|r| !r.url.is_empty() && passes_filter(&r.url, &opts.domain_filter))
                .take(opts.num_results)
                .collect()
        })
        .unwrap_or_default()
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
    fn parallel(&self) -> bool {
        false
    }
    fn search(&self, query: String, opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async move {
            let base = env("DUCKDUCKGO_BASE_URL").unwrap_or_else(|| "https://html.duckduckgo.com".into());
            let resp = client()
                .get(format!("{base}/html/"))
                .query(&[("q", &query)])
                .header("Accept", "text/html")
                .header("User-Agent", "Mozilla/5.0 (compatible; serpis/0.1)")
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            let status = resp.status().as_u16();
            let html = resp.text().await?;
            if status == 202 || html.contains("anomaly-modal__box") || html.contains("challenge-form") {
                let message = format!("DuckDuckGo blocked the request with a bot check (HTTP {status}, rate limited); use another search provider");
                return Err(ProviderError { status: 429, message, retry_after: None }.into());
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
            backends: vec![Arc::new(Brave), Arc::new(Exa), Arc::new(Tavily), Arc::new(Firecrawl), Arc::new(DuckDuckGo)],
        }
    }
}

impl WebSearchTool {
    /// The providers to try, in order: the one asked for (else `SERPIS_WEB_SEARCH_PROVIDER`)
    /// first, then the other configured ones.
    fn candidates(&self, requested: Option<&str>) -> Vec<Arc<dyn SearchBackend>> {
        let requested = requested.map(str::to_string).or_else(|| env("SERPIS_WEB_SEARCH_PROVIDER"));
        let mut out: Vec<_> = self.backends.iter().filter(|b| b.available()).cloned().collect();
        if let Some(i) = requested.and_then(|id| out.iter().position(|b| b.id() == id)) {
            let b = out.remove(i);
            out.insert(0, b);
        }
        out
    }
}

/// Every query, on one provider (one at a time when it can't take them at once).
async fn search_all(backend: &Arc<dyn SearchBackend>, queries: &[String], opts: &SearchOptions) -> Vec<anyhow::Result<Vec<SearchResult>>> {
    let futs = queries.iter().map(|q| search_with_retry(backend.clone(), q.clone(), opts.clone()));
    if backend.parallel() {
        return futures::future::join_all(futs).await;
    }
    let mut out = Vec::new();
    for f in futs {
        out.push(f.await);
    }
    out
}

impl AgentTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> String {
        "Search the web. Returns titles, URLs and snippets for each query. Use several focused queries for broad research. Use domainFilter to restrict (\"example.com\") or exclude (\"-example.com\") sites.".into()
    }
    fn parameters(&self) -> Value {
        let ids: Vec<&str> = self.backends.iter().filter(|b| b.available()).map(|b| b.id()).collect();
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "Several queries (instead of query)"},
                "numResults": {"type": "number", "description": "Results per query (default: 5, max: 20)"},
                "recencyFilter": {"type": "string", "enum": ["day", "week", "month", "year"], "description": "Only recent results"},
                "domainFilter": {"type": "array", "items": {"type": "string"}, "description": "Domains to include, or exclude with a leading '-'"},
                "provider": {"type": "string", "enum": ids, "description": "Search backend to try first (default: first configured; the others are tried if it fails)"}
            }
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Search the web for current information".into())
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        let backends = self.candidates(arg_str(&args, "provider"));
        async move {
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
            // Each provider's first error, for the ones that failed every query.
            let mut failed: Vec<(String, String)> = Vec::new();
            for backend in backends {
                let results = tokio::select! {
                    _ = ctx.cancel.cancelled() => anyhow::bail!("Operation aborted"),
                    r = search_all(&backend, &queries, &opts) => r,
                };
                if let Some(Err(e)) = results.iter().find(|r| r.is_err())
                    && results.iter().all(Result::is_err)
                {
                    failed.push((backend.id().to_string(), format!("{e:#}")));
                    continue;
                }
                return Ok(render(&backend, &queries, results, &failed));
            }
            let mut text = String::from("Web search is unavailable: every provider failed.\n");
            if failed.is_empty() {
                text.push_str("No web search provider is configured.\n");
            }
            for (id, e) in &failed {
                text.push_str(&format!("- {id}: {e}\n"));
            }
            let failures: Vec<Value> = failed.iter().map(|(id, e)| json!({"provider": id, "error": e})).collect();
            Ok(ToolResult::error(text.trim_end().to_string()).with_details(json!({"unavailable": true, "failures": failures})))
        }
        .boxed()
    }
}

/// The results from the provider that answered, noting the ones that failed before it.
fn render(backend: &Arc<dyn SearchBackend>, queries: &[String], results: Vec<anyhow::Result<Vec<SearchResult>>>, failed: &[(String, String)]) -> ToolResult {
    let mut text = String::new();
    if !failed.is_empty() {
        let ids: Vec<&str> = failed.iter().map(|(id, _)| id.as_str()).collect();
        text.push_str(&format!("({} failed; results from {})\n\n", ids.join(", "), backend.id()));
    }
    let mut structured = Vec::new();
    for (q, r) in queries.iter().zip(results) {
        text.push_str(&format!("## {q}\n"));
        match r {
            Ok(list) => {
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
    let failures: Vec<Value> = failed.iter().map(|(id, e)| json!({"provider": id, "error": e})).collect();
    ToolResult::text(text.trim_end().to_string()).with_details(json!({"queries": structured, "provider": backend.id(), "failures": failures}))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Stats {
        failures: AtomicUsize,
        calls: AtomicUsize,
        running: AtomicUsize,
        most_at_once: AtomicUsize,
    }

    struct Fake {
        id: &'static str,
        available: bool,
        parallel: bool,
        stats: Arc<Stats>,
    }

    impl SearchBackend for Fake {
        fn id(&self) -> &str {
            self.id
        }
        fn available(&self) -> bool {
            self.available
        }
        fn parallel(&self) -> bool {
            self.parallel
        }
        fn search(&self, query: String, _opts: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
            let s = self.stats.clone();
            s.calls.fetch_add(1, Ordering::SeqCst);
            let fail = s.failures.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok();
            let now = s.running.fetch_add(1, Ordering::SeqCst) + 1;
            s.most_at_once.fetch_max(now, Ordering::SeqCst);
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                s.running.fetch_sub(1, Ordering::SeqCst);
                if fail {
                    return Err(ProviderError { status: 429, message: "slow down".into(), retry_after: Some(Duration::from_millis(10)) }.into());
                }
                Ok(vec![SearchResult { title: format!("about {query}"), url: "https://example.com/".into(), snippet: String::new() }])
            }
            .boxed()
        }
    }

    fn fake(id: &'static str, available: bool, parallel: bool, failures: usize) -> (Arc<dyn SearchBackend>, Arc<Stats>) {
        let stats = Arc::new(Stats { failures: AtomicUsize::new(failures), ..Default::default() });
        (Arc::new(Fake { id, available, parallel, stats: stats.clone() }), stats)
    }

    async fn run(backends: Vec<Arc<dyn SearchBackend>>, args: Value) -> ToolResult {
        WebSearchTool { backends }.execute("1".into(), args, ToolContext::new(".")).await.unwrap()
    }

    #[test]
    fn search_keys_come_from_auth_json_before_the_environment() {
        let dir = std::env::temp_dir().join(format!("serpis-search-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), r#"{"firecrawl": {"type": "api_key", "key": "!printf fc-from-a-command"}, "exa": {"type": "api_key", "key": "exa-literal"}}"#).unwrap();
        use_registry(serpis_ai::Registry::new(serpis_ai::Paths { agent_dir: dir.clone() }));
        assert_eq!(api_key("firecrawl", "SERPIS_TEST_UNSET_FIRECRAWL"), Some("fc-from-a-command".into()));
        assert_eq!(api_key("exa", "SERPIS_TEST_UNSET_EXA"), Some("exa-literal".into()));
        assert_eq!(api_key("brave", "SERPIS_TEST_UNSET_BRAVE"), None);
        assert!(Firecrawl.available() && !Brave.available() || env("BRAVE_API_KEY").is_some());
        assert!(missing("brave", "BRAVE_API_KEY").to_string().contains("auth.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn firecrawl_asks_for_web_results_and_reads_them() {
        let opts = SearchOptions { num_results: 2, domain_filter: vec!["-pinterest.com".into()], recency: Some("week".into()) };
        assert_eq!(firecrawl_body("rust", &opts), json!({"query": "rust", "limit": 2, "sources": [{"type": "web"}], "excludeDomains": ["pinterest.com"], "tbs": "qdr:w"}));
        let only = SearchOptions { num_results: 5, domain_filter: vec!["docs.rs".into(), "-x.com".into()], recency: None };
        assert_eq!(firecrawl_body("q", &only)["includeDomains"], json!(["docs.rs"]));
        assert!(firecrawl_body("q", &only).get("excludeDomains").is_none());
        let reply = json!({"success": true, "data": {"web": [
            {"url": "https://www.rust-lang.org/", "title": "Rust", "description": "A language"},
            {"url": "https://pinterest.com/rust", "title": "Pins", "description": ""},
            {"url": "https://doc.rust-lang.org/book/", "title": "The Book", "description": "Learn Rust"},
            {"url": "https://crates.io/", "title": "Crates", "description": ""}
        ]}});
        let got: Vec<_> = firecrawl_results(&reply, &opts).into_iter().map(|r| (r.title, r.url, r.snippet)).collect();
        assert_eq!(got, [("Rust".into(), "https://www.rust-lang.org/".into(), "A language".into()), ("The Book".into(), "https://doc.rust-lang.org/book/".into(), "Learn Rust".into())]);
    }

    #[tokio::test]
    async fn a_rate_limited_search_is_tried_again() {
        let (b, s) = fake("duckduckgo", true, false, 2);
        let r = run(vec![b], json!({"query": "q"})).await;
        assert!(!r.is_error, "{}", r.details);
        assert_eq!(s.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_search_gives_up_after_its_retries() {
        let (b, s) = fake("duckduckgo", true, false, 5);
        let r = run(vec![b], json!({"query": "q"})).await;
        assert!(r.is_error);
        assert_eq!(r.details["unavailable"], true);
        assert_eq!(s.calls.load(Ordering::SeqCst), 1 + SEARCH_RETRIES as usize);
    }

    #[tokio::test]
    async fn a_failing_provider_hands_the_search_to_the_next() {
        let (brave, _) = fake("brave", true, true, 9);
        let (exa, _) = fake("exa", true, true, 0);
        let r = run(vec![brave, exa], json!({"query": "q"})).await;
        assert!(!r.is_error, "{}", r.details);
        assert_eq!(r.details["provider"], "exa");
        assert_eq!(r.details["failures"][0]["provider"], "brave");
    }

    #[tokio::test]
    async fn every_provider_failing_says_search_is_unavailable() {
        let (brave, _) = fake("brave", true, true, 9);
        let (ddg, _) = fake("duckduckgo", true, false, 9);
        let r = run(vec![brave, ddg], json!({"query": "q"})).await;
        assert!(r.is_error);
        assert_eq!(r.details["unavailable"], true);
        assert_eq!(r.details["failures"].as_array().unwrap().len(), 2);
        let r = run(vec![], json!({"query": "q"})).await;
        assert_eq!(r.details["unavailable"], true);
    }

    #[tokio::test]
    async fn a_backend_that_cannot_take_parallel_queries_gets_them_one_at_a_time() {
        let (b, serial) = fake("duckduckgo", true, false, 0);
        run(vec![b], json!({"queries": ["a", "b", "c"]})).await;
        assert_eq!(serial.most_at_once.load(Ordering::SeqCst), 1);
        let (b, parallel) = fake("exa", true, true, 0);
        run(vec![b], json!({"queries": ["a", "b", "c"]})).await;
        assert_eq!(parallel.most_at_once.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn only_configured_providers_are_offered_and_others_fall_back() {
        let (brave, brave_stats) = fake("brave", false, true, 0);
        let (ddg, _) = fake("duckduckgo", true, false, 0);
        let t = WebSearchTool { backends: vec![brave, ddg] };
        assert_eq!(t.parameters()["properties"]["provider"]["enum"], json!(["duckduckgo"]));
        let r = t.execute("1".into(), json!({"query": "q", "provider": "brave"}), ToolContext::new(".")).await.unwrap();
        assert!(!r.is_error);
        assert_eq!(r.details["provider"], "duckduckgo");
        assert_eq!(brave_stats.calls.load(Ordering::SeqCst), 0);
    }
}

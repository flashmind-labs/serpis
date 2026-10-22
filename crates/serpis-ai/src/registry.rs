//! Providers, model catalog, credentials.
//!
//! Sources, in increasing precedence:
//! 1. The pi.dev catalog for built-in providers (cached in `models-store.json`).
//! 2. `models.json` custom providers and overrides (same format as pi).
//! Credentials: `auth.json` → provider environment variables → `models.json` `apiKey`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{Model, now_ms};

pub const CATALOG_BASE_URL: &str = "https://pi.dev";
const CATALOG_REFRESH_MS: u64 = 6 * 60 * 60 * 1000;

/// A built-in provider: where its models come from and which environment variables hold its key.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub env_keys: &'static [&'static str],
}

/// Built-in providers (pi-ai `KnownProvider`) with their API key variables (pi-ai `env-api-keys.ts`).
pub const BUILTIN_PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo { id: "anthropic", name: "Anthropic", env_keys: &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"] },
    ProviderInfo { id: "openai", name: "OpenAI", env_keys: &["OPENAI_API_KEY"] },
    ProviderInfo { id: "google", name: "Google", env_keys: &["GEMINI_API_KEY"] },
    ProviderInfo { id: "openrouter", name: "OpenRouter", env_keys: &["OPENROUTER_API_KEY"] },
    ProviderInfo { id: "xai", name: "xAI", env_keys: &["XAI_API_KEY"] },
    ProviderInfo { id: "groq", name: "Groq", env_keys: &["GROQ_API_KEY"] },
    ProviderInfo { id: "cerebras", name: "Cerebras", env_keys: &["CEREBRAS_API_KEY"] },
    ProviderInfo { id: "deepseek", name: "DeepSeek", env_keys: &["DEEPSEEK_API_KEY"] },
    ProviderInfo { id: "mistral", name: "Mistral", env_keys: &["MISTRAL_API_KEY"] },
    ProviderInfo { id: "zai", name: "Z.ai", env_keys: &["ZAI_API_KEY"] },
    ProviderInfo { id: "zai-coding-cn", name: "Z.ai Coding (CN)", env_keys: &["ZAI_CODING_CN_API_KEY"] },
    ProviderInfo { id: "moonshotai", name: "Moonshot AI", env_keys: &["MOONSHOT_API_KEY"] },
    ProviderInfo { id: "moonshotai-cn", name: "Moonshot AI (CN)", env_keys: &["MOONSHOT_API_KEY"] },
    ProviderInfo { id: "minimax", name: "MiniMax", env_keys: &["MINIMAX_API_KEY"] },
    ProviderInfo { id: "minimax-cn", name: "MiniMax (CN)", env_keys: &["MINIMAX_CN_API_KEY"] },
    ProviderInfo { id: "huggingface", name: "Hugging Face", env_keys: &["HF_TOKEN"] },
    ProviderInfo { id: "fireworks", name: "Fireworks", env_keys: &["FIREWORKS_API_KEY"] },
    ProviderInfo { id: "together", name: "Together", env_keys: &["TOGETHER_API_KEY"] },
    ProviderInfo { id: "baseten", name: "Baseten", env_keys: &["BASETEN_API_KEY"] },
    ProviderInfo { id: "nvidia", name: "NVIDIA", env_keys: &["NVIDIA_API_KEY"] },
    ProviderInfo { id: "vercel-ai-gateway", name: "Vercel AI Gateway", env_keys: &["AI_GATEWAY_API_KEY"] },
    ProviderInfo { id: "opencode", name: "OpenCode", env_keys: &["OPENCODE_API_KEY"] },
    ProviderInfo { id: "opencode-go", name: "OpenCode Go", env_keys: &["OPENCODE_API_KEY"] },
    ProviderInfo { id: "kimi-coding", name: "Kimi Coding", env_keys: &["KIMI_API_KEY"] },
    ProviderInfo { id: "meta", name: "Meta", env_keys: &["META_API_KEY"] },
    ProviderInfo { id: "ant-ling", name: "Ant Ling", env_keys: &["ANT_LING_API_KEY"] },
    ProviderInfo { id: "xiaomi", name: "Xiaomi", env_keys: &["XIAOMI_API_KEY"] },
    ProviderInfo { id: "qwen-token-plan", name: "Qwen Token Plan", env_keys: &["QWEN_TOKEN_PLAN_API_KEY"] },
    ProviderInfo { id: "github-copilot", name: "GitHub Copilot", env_keys: &["COPILOT_GITHUB_TOKEN"] },
    ProviderInfo { id: "openai-codex", name: "OpenAI Codex", env_keys: &[] },
    ProviderInfo { id: "azure", name: "Azure OpenAI", env_keys: &["AZURE_OPENAI_API_KEY"] },
    ProviderInfo { id: "google-vertex", name: "Google Vertex", env_keys: &["GOOGLE_CLOUD_API_KEY"] },
    ProviderInfo { id: "amazon-bedrock", name: "Amazon Bedrock", env_keys: &["AWS_BEARER_TOKEN_BEDROCK"] },
    ProviderInfo { id: "cloudflare-workers-ai", name: "Cloudflare Workers AI", env_keys: &["CLOUDFLARE_API_KEY"] },
];

pub fn builtin_provider(id: &str) -> Option<&'static ProviderInfo> {
    BUILTIN_PROVIDERS.iter().find(|p| p.id == id)
}

/// `models.json` provider entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub compat: Option<Map<String, Value>>,
    #[serde(default)]
    pub models: Option<Vec<Map<String, Value>>>,
    #[serde(default)]
    pub model_overrides: Option<HashMap<String, Map<String, Value>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsFile {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    ApiKey {
        key: String,
    },
    Oauth {
        access: String,
        #[serde(default)]
        refresh: Option<String>,
        #[serde(default)]
        expires: Option<u64>,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreEntry {
    models: Vec<Model>,
    #[serde(rename = "checkedAt", default)]
    checked_at: u64,
}

/// Config directory layout shared with the agent crate.
#[derive(Debug, Clone)]
pub struct Paths {
    pub agent_dir: PathBuf,
}

impl Paths {
    /// `$SERPIS_AGENT_DIR`, else `~/.serpis/agent`.
    pub fn default_paths() -> Self {
        let agent_dir = std::env::var_os("SERPIS_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".serpis").join("agent"));
        Paths { agent_dir }
    }
    pub fn models_json(&self) -> PathBuf {
        self.agent_dir.join("models.json")
    }
    pub fn auth_json(&self) -> PathBuf {
        self.agent_dir.join("auth.json")
    }
    pub fn models_store(&self) -> PathBuf {
        self.agent_dir.join("models-store.json")
    }
    /// pi's directory, used as a fallback source for `models.json` and `auth.json`.
    pub fn pi_agent_dir() -> PathBuf {
        dirs::home_dir().unwrap_or_default().join(".pi").join("agent")
    }
}

#[derive(Default)]
struct State {
    catalog: BTreeMap<String, StoreEntry>,
    models_file: ModelsFile,
    auth: BTreeMap<String, Credential>,
    errors: Vec<String>,
}

/// Thread-safe model registry. Cheap to clone.
#[derive(Clone)]
pub struct Registry {
    paths: Paths,
    state: Arc<RwLock<State>>,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

impl Registry {
    pub fn new(paths: Paths) -> Self {
        let r = Registry {
            paths,
            state: Arc::new(RwLock::new(State::default())),
        };
        r.reload();
        r
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Re-read `models.json`, `auth.json` and the catalog cache from disk.
    pub fn reload(&self) {
        let mut errors = Vec::new();
        let pi_dir = Paths::pi_agent_dir();
        let pick = |own: PathBuf, file: &str| if own.exists() { own } else { pi_dir.join(file) };

        let models_file: ModelsFile = read_json(&pick(self.paths.models_json(), "models.json"))
            .unwrap_or_else(|e| {
                errors.push(format!("{e:#}"));
                None
            })
            .unwrap_or_default();
        let auth: BTreeMap<String, Credential> = read_json(&pick(self.paths.auth_json(), "auth.json"))
            .unwrap_or_else(|e| {
                errors.push(format!("{e:#}"));
                None
            })
            .unwrap_or_default();
        let mut catalog: BTreeMap<String, StoreEntry> = read_json(&self.paths.models_store())
            .unwrap_or_else(|e| {
                errors.push(format!("{e:#}"));
                None
            })
            .unwrap_or_default();
        // Seed from pi's store when ours lacks a provider (e.g. OpenRouter's live list).
        if let Ok(Some(pi_store)) = read_json::<BTreeMap<String, StoreEntry>>(&pi_dir.join("models-store.json")) {
            for (k, v) in pi_store {
                catalog.entry(k).or_insert(v);
            }
        }
        let mut st = self.state.write().unwrap();
        st.models_file = models_file;
        st.auth = auth;
        st.catalog = catalog;
        st.errors = errors;
    }

    pub fn load_errors(&self) -> Vec<String> {
        self.state.read().unwrap().errors.clone()
    }

    /// Providers worth showing: built-ins with credentials, plus every `models.json` provider.
    pub fn configured_providers(&self) -> Vec<String> {
        let st = self.state.read().unwrap();
        let mut out: Vec<String> = BUILTIN_PROVIDERS
            .iter()
            .filter(|p| st.auth.contains_key(p.id) || p.env_keys.iter().any(|k| env_nonempty(k)))
            .map(|p| p.id.to_string())
            .collect();
        for (id, cfg) in &st.models_file.providers {
            if !out.contains(id) && (cfg.models.is_some() || builtin_provider(id).is_none()) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Whether `provider`'s credential is an OAuth login (its tokens belong to one machine:
    /// refreshing them on another would log this one out).
    pub fn is_oauth(&self, provider: &str) -> bool {
        matches!(self.state.read().unwrap().auth.get(provider), Some(Credential::Oauth { .. }))
    }

    /// The models.json in effect: this agent directory's, else pi's.
    pub fn models_file_path(&self) -> PathBuf {
        let own = self.paths.models_json();
        if own.exists() { own } else { Paths::pi_agent_dir().join("models.json") }
    }

    /// The auth.json in effect: this agent directory's, else pi's.
    pub fn auth_file_path(&self) -> PathBuf {
        let own = self.paths.auth_json();
        if own.exists() { own } else { Paths::pi_agent_dir().join("auth.json") }
    }

    /// Fetch catalogs for configured built-in providers whose cache is stale.
    pub async fn refresh_catalogs(&self, force: bool) -> Vec<String> {
        let mut errors = Vec::new();
        let providers: Vec<String> = self
            .configured_providers()
            .into_iter()
            .filter(|p| builtin_provider(p).is_some())
            .collect();
        for provider in providers {
            let stale = {
                let st = self.state.read().unwrap();
                st.catalog
                    .get(&provider)
                    .is_none_or(|e| now_ms().saturating_sub(e.checked_at) > CATALOG_REFRESH_MS)
            };
            if !stale && !force {
                continue;
            }
            match fetch_catalog(&provider).await {
                Ok(models) => {
                    let mut st = self.state.write().unwrap();
                    st.catalog.insert(
                        provider.clone(),
                        StoreEntry {
                            models,
                            checked_at: now_ms(),
                        },
                    );
                }
                Err(e) => errors.push(format!("{provider}: {e:#}")),
            }
        }
        self.save_store();
        errors
    }

    fn save_store(&self) {
        let st = self.state.read().unwrap();
        if let Ok(json) = serde_json::to_string(&st.catalog) {
            let _ = std::fs::create_dir_all(&self.paths.agent_dir);
            let _ = std::fs::write(self.paths.models_store(), json);
        }
    }

    /// All chat models after merging catalog entries with `models.json`.
    pub fn models(&self) -> Vec<Model> {
        let st = self.state.read().unwrap();
        let mut out: Vec<Model> = Vec::new();
        let mut covered = std::collections::HashSet::new();

        for (provider, entry) in &st.catalog {
            let cfg = st.models_file.providers.get(provider);
            for m in &entry.models {
                if m.extra.get("type").and_then(Value::as_str).is_some_and(|t| t != "chat") {
                    continue;
                }
                let mut m = m.clone();
                m.provider = provider.clone();
                if let Some(cfg) = cfg {
                    apply_provider_overrides(&mut m, cfg);
                }
                out.push(m);
            }
            covered.insert(provider.clone());
        }

        for (provider, cfg) in &st.models_file.providers {
            let Some(models) = &cfg.models else { continue };
            for raw in models {
                let mut obj = raw.clone();
                obj.entry("api").or_insert_with(|| {
                    Value::String(cfg.api.clone().unwrap_or_else(|| "openai-completions".into()))
                });
                obj.insert("provider".into(), Value::String(provider.clone()));
                if let Some(base) = &cfg.base_url {
                    obj.entry("baseUrl").or_insert_with(|| Value::String(base.clone()));
                }
                match serde_json::from_value::<Model>(Value::Object(obj)) {
                    Ok(mut m) => {
                        if m.name.is_empty() {
                            m.name = m.id.clone();
                        }
                        apply_provider_overrides(&mut m, cfg);
                        out.retain(|x| !(x.provider == m.provider && x.id == m.id));
                        out.push(m);
                    }
                    Err(e) => tracing::warn!("models.json {provider}: {e}"),
                }
            }
        }
        out
    }

    pub fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.models().into_iter().find(|m| m.provider == provider && m.id == id)
    }

    /// Resolve the API key for a provider (auth.json → env → models.json).
    pub fn api_key(&self, provider: &str) -> Option<String> {
        let st = self.state.read().unwrap();
        if let Some(cred) = st.auth.get(provider) {
            match cred {
                Credential::ApiKey { key } => return resolve_config_value(key),
                Credential::Oauth { access, .. } => return Some(access.clone()),
            }
        }
        if let Some(info) = builtin_provider(provider) {
            for k in info.env_keys {
                if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
                    return Some(v);
                }
            }
        }
        st.models_file
            .providers
            .get(provider)
            .and_then(|c| c.api_key.as_deref())
            .and_then(resolve_config_value)
    }

    /// Provider-level headers from `models.json`, values resolved like API keys.
    pub fn provider_headers(&self, provider: &str) -> HashMap<String, String> {
        let st = self.state.read().unwrap();
        st.models_file
            .providers
            .get(provider)
            .and_then(|c| c.headers.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| resolve_config_value(&v).map(|v| (k, v)))
            .collect()
    }

    /// Store an API key in auth.json.
    pub fn set_api_key(&self, provider: &str, key: &str) -> Result<()> {
        {
            let mut st = self.state.write().unwrap();
            st.auth.insert(provider.into(), Credential::ApiKey { key: key.into() });
            std::fs::create_dir_all(&self.paths.agent_dir)?;
            std::fs::write(self.paths.auth_json(), serde_json::to_string_pretty(&st.auth)?)?;
        }
        Ok(())
    }
}

fn env_nonempty(k: &str) -> bool {
    std::env::var(k).is_ok_and(|v| !v.is_empty())
}

fn merge_object(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (k, v) in patch {
        match (target.get_mut(k), v) {
            (Some(Value::Object(t)), Value::Object(p)) => merge_object(t, p),
            _ => {
                target.insert(k.clone(), v.clone());
            }
        }
    }
}

fn apply_provider_overrides(m: &mut Model, cfg: &ProviderConfig) {
    if let Some(base) = &cfg.base_url {
        if m.base_url.is_empty() {
            m.base_url = base.clone();
        }
    }
    if let Some(compat) = &cfg.compat {
        let target = m.compat.get_or_insert_with(Map::new);
        merge_object(target, compat);
    }
    if let Some(h) = &cfg.headers {
        let target = m.headers.get_or_insert_with(HashMap::new);
        for (k, v) in h {
            if let Some(v) = resolve_config_value(v) {
                target.insert(k.clone(), v);
            }
        }
    }
    if let Some(ov) = cfg.model_overrides.as_ref().and_then(|o| o.get(&m.id)) {
        if let Ok(Value::Object(mut current)) = serde_json::to_value(&*m) {
            merge_object(&mut current, ov);
            if let Ok(updated) = serde_json::from_value(Value::Object(current)) {
                *m = updated;
            }
        }
    }
}

async fn fetch_catalog(provider: &str) -> Result<Vec<Model>> {
    let url = format!("{CATALOG_BASE_URL}/api/models/providers/{provider}?types=chat");
    let resp = crate::stream::http_client()
        .get(&url)
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?;
    let v: Value = resp.json().await?;
    let list = match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("models") {
            Some(Value::Array(a)) => a,
            _ => anyhow::bail!("invalid catalog"),
        },
        _ => anyhow::bail!("invalid catalog"),
    };
    Ok(list
        .into_iter()
        .filter_map(|m| serde_json::from_value::<Model>(m).ok())
        .collect())
}

/// Resolve a config value: `!cmd` runs a shell command, `$VAR`/`${VAR}` expand, else literal.
/// A bare upper-case name that is set in the environment is treated as a variable name (pi compat).
pub fn resolve_config_value(raw: &str) -> Option<String> {
    if let Some(cmd) = raw.strip_prefix('!') {
        static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<String>>>> =
            std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(v) = cache.lock().unwrap().get(cmd) {
            return v.clone();
        }
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        cache.lock().unwrap().insert(cmd.to_string(), out.clone());
        return out;
    }
    let is_env_name = !raw.is_empty()
        && raw.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && raw.chars().next().is_some_and(|c| !c.is_ascii_digit());
    if is_env_name {
        if let Ok(v) = std::env::var(raw) {
            return Some(v);
        }
    }
    if !raw.contains('$') {
        return Some(raw.to_string());
    }
    let mut out = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('$') | Some('!') => {
                out.push(chars[i + 1]);
                i += 2;
            }
            Some('{') => {
                if let Some(end) = chars[i + 2..].iter().position(|c| *c == '}') {
                    let name: String = chars[i + 2..i + 2 + end].iter().collect();
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                    i += end + 3;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                let name: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .collect();
                if name.is_empty() {
                    out.push('$');
                    i += 1;
                } else {
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                    i += 1 + name.chars().count();
                }
            }
        }
    }
    Some(out)
}

//! The model catalog: models.dev data, the on-disk cache and per-provider fallbacks.
//!
//! `models.dev` is the catalog (`https://models.dev/api.json`). Yoke maps its own
//! provider ids onto models.dev ids (`opencode` → `opencode-go`, `copilot` →
//! `github-copilot`, and so on), keeps only models that support tool calls, and
//! caches the reduced result in `<home>/models.json` for a day. A fetch failure
//! leaves the stale cache in place; with no cache at all, each provider falls
//! back to a short built-in list that includes its default model.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::providers;

pub const CATALOG_URL: &str = "https://models.dev/api.json";
/// How long a cached catalog stays fresh, in seconds.
pub const CACHE_TTL: u64 = 24 * 60 * 60;

/// One selectable model, reduced to what the picker and status line need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
}

impl Model {
    fn new(id: &str, name: &str, context: Option<usize>) -> Self {
        Self {
            id: id.to_string(),
            name: if name.is_empty() {
                id.to_string()
            } else {
                name.to_string()
            },
            context,
        }
    }

    /// `name` with the id in parentheses when they differ, for the picker.
    pub fn label(&self) -> String {
        if self.name == self.id {
            self.id.clone()
        } else {
            format!("{} ({})", self.name, self.id)
        }
    }
}

/// The cached catalog: yoke provider id → models that support tool calls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Unix seconds when the data was fetched; `0` means "never".
    #[serde(default)]
    pub fetched_at: u64,
    #[serde(default)]
    pub providers: BTreeMap<String, Vec<Model>>,
}

impl Catalog {
    pub fn models(&self, provider: &str) -> Option<&[Model]> {
        self.providers.get(provider).map(Vec::as_slice)
    }

    pub fn context_window(&self, provider: &str, model: &str) -> Option<usize> {
        self.providers
            .get(provider)?
            .iter()
            .find(|entry| entry.id == model)
            .and_then(|entry| entry.context)
    }
}

pub fn cache_path(home: &Path) -> PathBuf {
    home.join("models.json")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

pub fn is_fresh(catalog: Option<&Catalog>, now: u64) -> bool {
    match catalog {
        Some(catalog) => {
            catalog.fetched_at > 0 && now.saturating_sub(catalog.fetched_at) < CACHE_TTL
        }
        None => false,
    }
}

/// Read the cached catalog. A missing or malformed file is simply no catalog.
pub fn load_cache(home: &Path) -> Option<Catalog> {
    let bytes = fs::read(cache_path(home)).ok()?;
    let catalog: Catalog = serde_json::from_slice(&bytes).ok()?;
    (!catalog.providers.is_empty()).then_some(catalog)
}

pub fn save_cache(home: &Path, catalog: &Catalog) -> Result<()> {
    fs::create_dir_all(home)?;
    let path = cache_path(home);
    let body = serde_json::to_vec_pretty(catalog)?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

/// Parse models.dev's `api.json` into models.dev provider id → tool-calling models.
pub fn parse_api(body: &str) -> Result<BTreeMap<String, Vec<Model>>> {
    let value: serde_json::Value = serde_json::from_str(body).context("parse models.dev")?;
    let object = value
        .as_object()
        .context("models.dev: expected an object")?;
    let mut providers = BTreeMap::new();
    for (id, provider) in object {
        let Some(models) = provider.get("models").and_then(|models| models.as_object()) else {
            continue;
        };
        let mut list = Vec::new();
        for (key, model) in models {
            if model.get("tool_call").and_then(|flag| flag.as_bool()) != Some(true) {
                continue;
            }
            let model_id = model.get("id").and_then(|id| id.as_str()).unwrap_or(key);
            let name = model
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or(model_id);
            let context = model
                .get("limit")
                .and_then(|limit| limit.get("context"))
                .and_then(|context| context.as_u64())
                .map(|context| context as usize);
            list.push(Model::new(model_id, name, context));
        }
        if !list.is_empty() {
            list.sort_by(|a, b| a.id.cmp(&b.id));
            providers.insert(id.clone(), list);
        }
    }
    Ok(providers)
}

/// Yoke provider id → models.dev provider id.
pub fn models_dev_id(provider: &str) -> Option<&'static str> {
    match provider {
        "opencode" => Some("opencode-go"),
        "groq" => Some("groq"),
        "openai" => Some("openai"),
        // The Responses dialect serves the same account's models; codex is a subset.
        "codex" => Some("openai"),
        "claude" => Some("anthropic"),
        "copilot" => Some("github-copilot"),
        "grok" => Some("xai"),
        _ => None,
    }
}

/// Project models.dev providers onto Yoke's ids, keeping tool-calling models.
pub fn map_providers(api: &BTreeMap<String, Vec<Model>>) -> BTreeMap<String, Vec<Model>> {
    let mut providers = BTreeMap::new();
    for provider in ["opencode", "groq", "openai", "claude", "copilot", "grok"] {
        let Some(source) = models_dev_id(provider) else {
            continue;
        };
        if let Some(models) = api.get(source) {
            providers.insert(provider.to_string(), models.clone());
        }
    }
    if let Some(models) = api.get("openai") {
        let mut codex: Vec<Model> = models
            .iter()
            .filter(|model| model.id.to_ascii_lowercase().contains("codex"))
            .cloned()
            .collect();
        if codex.is_empty() {
            codex = models
                .iter()
                .filter(|model| model.id.starts_with("gpt-5"))
                .cloned()
                .collect();
        }
        if !codex.is_empty() {
            providers.insert("codex".to_string(), codex);
        }
    }
    providers
}

/// A small built-in list for when there is no cache: each provider's default
/// model first, then a few current ids that speak its dialect.
pub fn fallback(provider: &str) -> Vec<Model> {
    let mut ids: Vec<&str> = match provider {
        "codex" => vec![
            providers::default_model("codex"),
            "gpt-5.3-codex",
            "gpt-5.2-codex",
            "gpt-5.1-codex",
        ],
        "grok" => vec!["grok-4.7", "grok-4.6", "grok-4.5"],
        "claude" => vec!["claude-sonnet-4-5", "claude-opus-4-5", "claude-haiku-4-5"],
        "copilot" => vec!["gpt-5.4", "claude-sonnet-4.6", "gemini-3.6-flash"],
        "opencode" => vec![
            providers::default_model("opencode"),
            "deepseek-v4-pro",
            "kimi-k2.6",
        ],
        "openai" => vec![
            providers::default_model("openai"),
            "gpt-5.4",
            "gpt-5.3-codex",
        ],
        "groq" => vec![providers::default_model("groq"), "qwen/qwen3.8-27b"],
        "compat" => Vec::new(),
        "mock" => vec![providers::default_model(provider)],
        _ => Vec::new(),
    };
    ids.retain(|id| !id.is_empty());
    ids.dedup();
    ids.into_iter().map(|id| Model::new(id, id, None)).collect()
}

/// Providers `providers::make` can build today. The picker skips the others
/// (grok, claude, copilot) until they are implemented.
pub fn buildable(provider: &str) -> bool {
    matches!(
        provider,
        "codex" | "opencode" | "openai" | "groq" | "compat" | "mock"
    )
}

/// The models to show for a provider: the catalog's list or the fallback, with
/// `current` added (and marked separately by the picker) when it is missing.
pub fn models_for(catalog: Option<&Catalog>, provider: &str, current: Option<&str>) -> Vec<Model> {
    let mut models = catalog
        .and_then(|catalog| catalog.models(provider))
        .map(<[Model]>::to_vec)
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| fallback(provider));
    if let Some(current) = current.filter(|current| !current.is_empty()) {
        if !models.iter().any(|model| model.id == current) {
            models.insert(0, Model::new(current, current, None));
        }
    }
    models
}

/// Env var names and values for a user-configured OpenAI-compatible server.
pub fn compat_env() -> (Option<String>, Option<String>) {
    (
        std::env::var(providers::compat::ENV_BASE_URL).ok(),
        std::env::var(providers::compat::ENV_API_KEY).ok(),
    )
}

/// Whether `yoke status` would show this provider as usable.
pub fn connected(
    info: &providers::ProviderInfo,
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> bool {
    match info.id {
        "codex" => crate::auth::codex_logged_in(home),
        "compat" => crate::auth::stored_base_url(home, info.id)
            .or_else(|| env(providers::compat::ENV_BASE_URL))
            .is_some_and(|base| !base.trim().is_empty()),
        "mock" => env("YOKE_DEV").as_deref() == Some("1"),
        _ => {
            let stored = crate::auth::stored_key(home, info.id);
            let env_key = info.env.and_then(env);
            crate::auth::ApiKeyAuth::new(stored, env_key).is_some()
        }
    }
}

/// Fetch models.dev, map it, add a live compat server's models, and cache it.
pub async fn fetch(
    home: &Path,
    compat_base: Option<String>,
    compat_key: Option<String>,
) -> Result<Catalog> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("models.dev: cannot build the HTTP client")?;
    let body = client
        .get(CATALOG_URL)
        .header("user-agent", concat!("yoke/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .context("models.dev: request failed")?
        .error_for_status()
        .context("models.dev: bad response")?
        .text()
        .await
        .context("models.dev: cannot read body")?;
    let api = parse_api(&body)?;
    let mut providers = map_providers(&api);

    if let Some(base) = compat_base.filter(|base| !base.trim().is_empty()) {
        match compat_models(&base, compat_key.as_deref()).await {
            Ok(models) => {
                providers.insert("compat".to_string(), models);
            }
            Err(_) => {
                // Keep the previous compat list when only its live fetch failed.
                if let Some(previous) =
                    load_cache(home).and_then(|catalog| catalog.providers.get("compat").cloned())
                {
                    providers.insert("compat".to_string(), previous);
                }
            }
        }
    }

    let catalog = Catalog {
        fetched_at: now(),
        providers,
    };
    save_cache(home, &catalog)?;
    Ok(catalog)
}

/// `GET <base>/models` for a user-configured OpenAI-compatible server.
async fn compat_models(base: &str, api_key: Option<&str>) -> Result<Vec<Model>> {
    let ids = providers::compat::models(base, api_key).await?;
    let mut models: Vec<Model> = ids
        .into_iter()
        .filter(|id| !id.is_empty())
        .map(|id| Model::new(&id, &id, None))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

/// Update `<home>/config.toml` only when it already exists; a missing file stays
/// missing until the config loader lands.
pub fn persist_config(home: &Path, provider: &str, model: &str) -> Result<()> {
    let path = home.join("config.toml");
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    fs::write(&path, update_config_text(&text, provider, model))
        .with_context(|| format!("write {}", path.display()))
}

/// Replace `provider`/`model` lines in a tiny TOML file, appending them when absent.
pub fn update_config_text(text: &str, provider: &str, model: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (key, value) in [("provider", provider), ("model", model)] {
        let replacement = format!("{key} = \"{value}\"");
        let found = lines.iter().position(|line| {
            let line = line.trim_start();
            line.starts_with(key)
                && line[key.len()..].trim_start().starts_with('=')
                && !line.starts_with('#')
        });
        match found {
            Some(index) => lines[index] = replacement,
            None => lines.push(replacement),
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;

    const FIXTURE: &str = r#"{
        "groq": {
            "name": "Groq",
            "models": {
                "openai/gpt-oss-120b": {
                    "id": "openai/gpt-oss-120b",
                    "name": "GPT OSS 120B",
                    "tool_call": true,
                    "limit": {"context": 131072, "output": 32768}
                },
                "whisper": {
                    "id": "whisper",
                    "name": "Whisper",
                    "tool_call": false,
                    "limit": {"context": 448}
                }
            }
        },
        "opencode-go": {
            "models": {
                "deepseek-v4.1-flash": {"id": "deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash", "tool_call": true, "limit": {"context": 1000000}}
            }
        },
        "openai": {
            "models": {
                "gpt-5.3-codex": {"id": "gpt-5.3-codex", "name": "GPT-5.3 Codex", "tool_call": true, "limit": {"context": 400000}},
                "gpt-6.1-terra": {"id": "gpt-6.1-terra", "name": "GPT-6.1 Terra", "tool_call": true, "limit": {"context": 1050000}},
                "gpt-image": {"id": "gpt-image", "name": "Image", "tool_call": false}
            }
        },
        "anthropic": {
            "models": {
                "claude-sonnet-4-5": {"id": "claude-sonnet-4-5", "name": "Claude Sonnet 4.5", "tool_call": true, "limit": {"context": 200000}}
            }
        },
        "github-copilot": {
            "models": {
                "gpt-5.4": {"id": "gpt-5.4", "name": "GPT-5.4", "tool_call": true, "limit": {"context": 400000}}
            }
        },
        "xai": {
            "models": {
                "grok-4.7": {"id": "grok-4.7", "name": "Grok 4.7", "tool_call": true, "limit": {"context": 500000}}
            }
        }
    }"#;

    #[test]
    fn parser_keeps_only_tool_calling_models_with_context_windows() {
        let api = parse_api(FIXTURE).unwrap();
        let groq = &api["groq"];
        assert_eq!(groq.len(), 1);
        assert_eq!(groq[0].id, "openai/gpt-oss-120b");
        assert_eq!(groq[0].name, "GPT OSS 120B");
        assert_eq!(groq[0].context, Some(131072));
        assert!(api["openai"].iter().all(|model| model.id != "gpt-image"));
        assert_eq!(parse_api("{}").unwrap(), BTreeMap::new());
    }

    #[test]
    fn mapping_covers_every_catalog_provider_and_filters_codex() {
        let catalog = map_providers(&parse_api(FIXTURE).unwrap());
        assert_eq!(
            models_dev_id("opencode").unwrap(),
            "opencode-go",
            "opencode maps to its models.dev id"
        );
        assert_eq!(models_dev_id("compat"), None);
        assert_eq!(catalog["groq"].len(), 1);
        assert_eq!(catalog["opencode"][0].id, "deepseek-v4.1-flash");
        assert_eq!(catalog["claude"][0].id, "claude-sonnet-4-5");
        assert_eq!(catalog["copilot"][0].id, "gpt-5.4");
        assert_eq!(catalog["grok"][0].id, "grok-4.7");
        assert_eq!(
            catalog["codex"]
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.3-codex"],
            "codex is the codex subset of openai"
        );
        for provider in ["codex", "opencode", "openai", "groq", "compat", "mock"] {
            assert!(buildable(provider), "{provider} should be listed");
        }
        for provider in ["grok", "claude", "copilot"] {
            assert!(!buildable(provider), "{provider} is not implemented yet");
        }
    }

    #[test]
    fn codex_falls_back_to_gpt_5_models_when_no_codex_id_exists() {
        let api = parse_api(
            r#"{"openai":{"models":{
                "gpt-5.4":{"id":"gpt-5.4","name":"GPT-5.4","tool_call":true},
                "gpt-4o":{"id":"gpt-4o","name":"GPT-4o","tool_call":true}
            }}}"#,
        )
        .unwrap();
        let catalog = map_providers(&api);
        assert_eq!(
            catalog["codex"]
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.4"]
        );
    }

    #[test]
    fn fallbacks_include_each_providers_default_model() {
        for provider in [
            "codex", "grok", "claude", "copilot", "opencode", "openai", "groq", "mock",
        ] {
            let models = fallback(provider);
            assert!(!models.is_empty(), "{provider} has no fallback");
            let default = providers::default_model(provider);
            if default != "default" && !default.is_empty() {
                assert_eq!(
                    models[0].id, default,
                    "{provider} should list its default first"
                );
            }
        }
        assert!(
            fallback("compat").is_empty(),
            "compat has no default model; its live list supplies ids"
        );
    }

    #[test]
    fn the_current_model_is_added_when_the_catalog_lacks_it() {
        let catalog = Catalog {
            fetched_at: 1,
            providers: BTreeMap::from([(
                "groq".to_string(),
                vec![Model::new(
                    "openai/gpt-oss-120b",
                    "GPT OSS 120B",
                    Some(131072),
                )],
            )]),
        };
        let models = models_for(Some(&catalog), "groq", Some("local-test"));
        assert_eq!(models[0].id, "local-test");
        assert_eq!(models.len(), 2);
        let models = models_for(None, "groq", Some(providers::default_model("groq")));
        assert_eq!(models[0].id, providers::default_model("groq"));
    }

    #[test]
    fn cache_round_trips_through_a_temp_home_and_expires_after_a_day() {
        let home = TempDir::new("models-cache");
        let catalog = Catalog {
            fetched_at: 1_000,
            providers: BTreeMap::from([("groq".to_string(), vec![Model::new("m", "M", Some(10))])]),
        };
        save_cache(home.path(), &catalog).unwrap();
        assert_eq!(load_cache(home.path()).unwrap(), catalog);
        assert!(is_fresh(Some(&catalog), 1_000 + CACHE_TTL - 1));
        assert!(!is_fresh(Some(&catalog), 1_000 + CACHE_TTL));
        assert!(!is_fresh(None, 1_000));
        assert_eq!(
            catalog.context_window("groq", "m"),
            Some(10),
            "the status line reads the same catalog"
        );
        assert_eq!(catalog.context_window("groq", "other"), None);
        assert!(load_cache(&home.path().join("missing")).is_none());
    }

    #[test]
    fn connected_matches_the_status_check_with_injected_credentials() {
        use providers::PROVIDERS;
        let home = TempDir::new("models-connected");
        let groq = PROVIDERS.iter().find(|info| info.id == "groq").unwrap();
        let claude = PROVIDERS.iter().find(|info| info.id == "claude").unwrap();
        assert!(!connected(groq, home.path(), |_| None));
        assert!(!connected(claude, home.path(), |_| None));
        assert!(connected(groq, home.path(), |name| (name
            == "GROQ_API_KEY")
            .then(|| "key".into())));
        crate::auth::set_api_key(home.path(), "groq", "stored").unwrap();
        assert!(connected(groq, home.path(), |_| None));
        let compat = PROVIDERS.iter().find(|info| info.id == "compat").unwrap();
        assert!(!connected(compat, home.path(), |_| None));
        assert!(connected(compat, home.path(), |name| {
            (name == providers::compat::ENV_BASE_URL).then(|| "http://localhost:11434/v1".into())
        }));
        crate::auth::set_endpoint(home.path(), "compat", "http://localhost:8000/v1", None).unwrap();
        assert!(connected(compat, home.path(), |_| None));
    }

    #[test]
    fn config_is_only_updated_with_provider_and_model_lines() {
        let text = "# my config\nprovider = \"groq\"\nmodel = \"old\"\nother = 1\n";
        assert_eq!(
            update_config_text(text, "openai", "gpt-5.4"),
            "# my config\nprovider = \"openai\"\nmodel = \"gpt-5.4\"\nother = 1\n"
        );
        assert_eq!(
            update_config_text("", "groq", "m"),
            "provider = \"groq\"\nmodel = \"m\"\n"
        );
    }
}

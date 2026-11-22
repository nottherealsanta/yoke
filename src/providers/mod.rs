//! The provider trait, the `PROVIDERS` table and `make(id, model)`.

pub mod chat;
pub mod codex;
pub mod compat;
mod groq;
pub mod mock;
mod openai;
mod opencode;
pub mod responses;
pub mod sse;

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::types::{Request, StreamEvent};

pub use crate::auth::Auth;

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    /// The exact JSON body that will be sent. Mode 3 shows this verbatim.
    fn body(&self, req: &Request) -> Value;
    /// Stream the response as normalized events.
    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>>;
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// The environment variable that holds this provider's API key, if any.
    pub env: Option<&'static str>,
}

/// Every provider Yoke knows about, in the order the CLI lists them.
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "codex",
        label: "ChatGPT (Codex)",
        env: None,
    },
    ProviderInfo {
        id: "grok",
        label: "xAI (SuperGrok)",
        env: None,
    },
    ProviderInfo {
        id: "claude",
        label: "Claude",
        env: None,
    },
    ProviderInfo {
        id: "copilot",
        label: "GitHub Copilot",
        env: None,
    },
    ProviderInfo {
        id: "opencode",
        label: "OpenCode Go",
        env: Some("OPENCODE_API_KEY"),
    },
    ProviderInfo {
        id: "openai",
        label: "OpenAI",
        env: Some("OPENAI_API_KEY"),
    },
    ProviderInfo {
        id: "groq",
        label: "Groq",
        env: Some(groq::ENV),
    },
    ProviderInfo {
        id: "compat",
        label: compat::LABEL,
        env: Some(compat::ENV_API_KEY),
    },
];

/// The model used when `--model` is not given.
pub fn default_model(id: &str) -> &'static str {
    match id {
        "groq" => groq::DEFAULT_MODEL,
        "opencode" => opencode::DEFAULT_MODEL,
        "compat" => "",
        "openai" => openai::DEFAULT_MODEL,
        "codex" => codex::DEFAULT_MODEL,
        "mock" => "mock",
        _ => "default",
    }
}

/// Build a provider. The mock is reachable only with `YOKE_DEV=1`; `dev` and
/// `env` are passed in so tests need not touch the environment.
pub fn make(
    id: &str,
    model: &str,
    dev: Option<&str>,
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Box<dyn Provider>> {
    if id == "mock" {
        if dev == Some("1") {
            return Ok(Box::new(mock::MockProvider::dev()));
        }
        anyhow::bail!("provider `mock` requires YOKE_DEV=1");
    }
    if id == "groq" || id == "opencode" || id == "openai" {
        let env_name = match id {
            "groq" => groq::ENV,
            "opencode" => opencode::ENV,
            _ => openai::ENV,
        };
        let auth = crate::auth::ApiKeyAuth::new(crate::auth::stored_key(home, id), env(env_name))
            .ok_or_else(|| {
            anyhow::anyhow!("{id} is not signed in: run `yoke login {id}` or set {env_name}")
        })?;
        let auth: Box<dyn Auth> = Box::new(auth);
        return Ok(match id {
            "groq" => Box::new(groq::new(auth)),
            "opencode" => Box::new(opencode::new(auth)),
            _ => Box::new(openai::new(auth)),
        });
    }
    if id == "compat" {
        if model.trim().is_empty() {
            anyhow::bail!("compat needs a model: pass --model <id> from `GET <base>/models`");
        }
        let base = crate::auth::stored_base_url(home, id)
            .or_else(|| env(compat::ENV_BASE_URL))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "compat is not configured: run `yoke login compat` or set {}",
                    compat::ENV_BASE_URL
                )
            })?;
        let auth = crate::auth::ApiKeyAuth::new(
            crate::auth::stored_key(home, id),
            env(compat::ENV_API_KEY),
        );
        let auth: Option<Box<dyn Auth>> = auth.map(|auth| Box::new(auth) as Box<dyn Auth>);
        return Ok(Box::new(compat::new(&base, auth)?));
    }
    if id == "codex" {
        if !crate::auth::codex_logged_in(home) {
            anyhow::bail!("codex is not signed in: run `yoke login codex`");
        }
        return Ok(Box::new(codex::new(home)));
    }
    if PROVIDERS.iter().any(|provider| provider.id == id) {
        anyhow::bail!("provider `{id}` is not implemented yet");
    }
    anyhow::bail!("unknown provider: {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_all_eight_providers() {
        assert_eq!(PROVIDERS.len(), 8);
        for id in [
            "codex", "grok", "claude", "copilot", "opencode", "openai", "groq", "compat",
        ] {
            assert!(PROVIDERS.iter().any(|provider| provider.id == id), "{id}");
        }
    }

    #[test]
    fn mock_needs_dev() {
        let home = Path::new("/nonexistent");
        assert!(make("mock", "m", None, home, |_| None).is_err());
        assert_eq!(
            make("mock", "m", Some("1"), home, |_| None).unwrap().id(),
            "mock"
        );
    }

    #[test]
    fn real_providers_are_not_implemented() {
        for provider in PROVIDERS {
            if ["groq", "opencode", "openai", "compat", "codex"].contains(&provider.id) {
                continue;
            }
            let error = make(provider.id, "m", None, Path::new("/nonexistent"), |_| None)
                .map(|_| ())
                .unwrap_err();
            assert!(error.to_string().contains("not implemented"), "{error}");
        }
        assert!(make("nope", "m", Some("1"), Path::new("/nonexistent"), |_| None).is_err());
    }

    #[test]
    fn groq_needs_a_key_and_accepts_one_from_the_environment() {
        let home = Path::new("/nonexistent");
        let error = make("groq", "m", None, home, |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"), "{error}");
        assert_eq!(
            make("groq", "m", None, home, |name| {
                (name == "GROQ_API_KEY").then(|| "key".to_string())
            })
            .unwrap()
            .id(),
            "groq"
        );
    }

    #[test]
    fn opencode_needs_a_key_and_uses_its_default_model() {
        let home = Path::new("/nonexistent");
        let error = make("opencode", "m", None, home, |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"), "{error}");
        assert_eq!(default_model("opencode"), opencode::DEFAULT_MODEL);
        assert_eq!(
            make("opencode", "m", None, home, |name| {
                (name == "OPENCODE_API_KEY").then(|| "key".to_string())
            })
            .unwrap()
            .id(),
            "opencode"
        );
    }

    #[test]
    fn openai_needs_a_key_and_uses_its_default_model() {
        let home = Path::new("/nonexistent");
        let error = make("openai", "m", None, home, |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"), "{error}");
        assert_eq!(default_model("openai"), openai::DEFAULT_MODEL);
        assert_eq!(
            make("openai", "m", None, home, |name| {
                (name == openai::ENV).then(|| "key".to_string())
            })
            .unwrap()
            .id(),
            "openai"
        );
    }

    #[test]
    fn codex_needs_a_login_and_uses_its_default_model() {
        let home = Path::new("/nonexistent");
        let error = make("codex", "m", None, home, |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"), "{error}");
        assert_eq!(default_model("codex"), codex::DEFAULT_MODEL);
    }

    #[test]
    fn compat_needs_a_model_and_a_base_url_but_no_key() {
        use crate::tools::testutil::TempDir;

        let home = TempDir::new("compat");
        assert_eq!(default_model("compat"), "");

        let error = make("compat", "", None, home.path(), |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("needs a model"), "{error}");

        let error = make("compat", "model", None, home.path(), |_| None)
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("not configured"), "{error}");

        let provider = make("compat", "model", None, home.path(), |name| {
            (name == compat::ENV_BASE_URL).then(|| "localhost:8000/v1".to_string())
        })
        .unwrap();
        assert_eq!(provider.id(), "compat");
    }

    #[test]
    fn compat_prefers_stored_values_over_the_environment() {
        use crate::tools::testutil::TempDir;

        let home = TempDir::new("compat");
        crate::auth::set_endpoint(
            home.path(),
            "compat",
            "http://stored:8080/v1",
            Some("stored-key"),
        )
        .unwrap();
        assert_eq!(
            crate::auth::stored_base_url(home.path(), "compat").as_deref(),
            Some("http://stored:8080/v1")
        );
        let provider = make("compat", "model", None, home.path(), |name| match name {
            compat::ENV_BASE_URL => Some("two words".to_string()),
            compat::ENV_API_KEY => Some("env-key".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(provider.id(), "compat");
    }
}

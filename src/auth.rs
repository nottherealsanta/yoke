//! Credential store and the auth trait.
//!
//! `auth.json` holds provider credentials in the user's Yoke home. It is
//! written atomically with mode 0600. Keys are never printed or logged.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait Auth: Send + Sync {
    /// Headers for one provider request; refreshes tokens when needed.
    async fn headers(&self) -> Result<Vec<(String, String)>>;
}

pub fn auth_path(home: &Path) -> PathBuf {
    home.join("auth.json")
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    residency: Option<String>,
}

fn redacted(value: &Option<String>) -> Option<&'static str> {
    value.as_ref().map(|_| "<redacted>")
}

impl std::fmt::Debug for Record {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Record")
            .field("api_key", &redacted(&self.api_key))
            .field("base_url", &self.base_url)
            .field("refresh_token", &redacted(&self.refresh_token))
            .field("access_token", &redacted(&self.access_token))
            .field("id_token", &redacted(&self.id_token))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("residency", &self.residency)
            .finish()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    providers: BTreeMap<String, Record>,
}

fn load(home: &Path) -> Result<Store> {
    let path = auth_path(home);
    match fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn save(home: &Path, store: &Store) -> Result<()> {
    fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let path = auth_path(home);
    let unique = format!(
        "auth.json.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    );
    let temp = home.join(unique);
    let bytes = serde_json::to_vec_pretty(store)?;
    write_private(&temp, &bytes).with_context(|| format!("write {}", temp.display()))?;
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("rename to {}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The stored API key for a provider, if any.
pub fn stored_key(home: &Path, provider: &str) -> Option<String> {
    let store = load(home).ok()?;
    store
        .providers
        .get(provider)?
        .api_key
        .clone()
        .filter(|key| !key.trim().is_empty())
}

/// Store (or replace) a provider's API key.
pub fn set_api_key(home: &Path, provider: &str, key: &str) -> Result<()> {
    let mut store = load(home)?;
    store.providers.entry(provider.into()).or_default().api_key = Some(key.trim().into());
    save(home, &store)
}

/// The stored base URL for a user-configured provider, if any.
pub fn stored_base_url(home: &Path, provider: &str) -> Option<String> {
    let store = load(home).ok()?;
    store
        .providers
        .get(provider)?
        .base_url
        .clone()
        .filter(|url| !url.trim().is_empty())
}

/// Store a provider's base URL and optional API key in one write. `None` clears
/// the key, so logging in again without one removes the old credentials.
pub fn set_endpoint(
    home: &Path,
    provider: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<()> {
    let mut store = load(home)?;
    let record = store.providers.entry(provider.into()).or_default();
    record.base_url = Some(base_url.trim().into());
    record.api_key = api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    save(home, &store)
}

/// Remove a provider's stored credentials. Returns whether anything was there.
pub fn remove(home: &Path, provider: &str) -> Result<bool> {
    let mut store = load(home)?;
    let removed = store.providers.remove(provider).is_some();
    if removed {
        save(home, &store)?;
    }
    Ok(removed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Stored,
    Env,
}

/// Bearer auth from a stored API key, or from the environment when nothing is
/// stored. The environment value is passed in so tests need not touch it.
pub struct ApiKeyAuth {
    key: String,
    source: KeySource,
}

impl ApiKeyAuth {
    pub fn new(stored: Option<String>, env: Option<String>) -> Option<Self> {
        let stored = stored.filter(|key| !key.trim().is_empty());
        let env = env.filter(|key| !key.trim().is_empty());
        match (stored, env) {
            (Some(key), _) => Some(Self {
                key: key.trim().into(),
                source: KeySource::Stored,
            }),
            (None, Some(key)) => Some(Self {
                key: key.trim().into(),
                source: KeySource::Env,
            }),
            (None, None) => None,
        }
    }

    pub fn source(&self) -> KeySource {
        self.source
    }
}

impl std::fmt::Debug for ApiKeyAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiKeyAuth")
            .field("key", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

#[async_trait]
impl Auth for ApiKeyAuth {
    async fn headers(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![(
            "authorization".into(),
            format!("Bearer {}", self.key),
        )])
    }
}

/// The stored ChatGPT (Codex) credentials.
#[derive(Clone, Default)]
pub struct CodexTokens {
    pub refresh_token: Option<String>,
    pub access_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_at: Option<u64>,
    pub account_id: Option<String>,
    pub residency: Option<String>,
}

impl std::fmt::Debug for CodexTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexTokens")
            .field("refresh_token", &redacted(&self.refresh_token))
            .field("access_token", &redacted(&self.access_token))
            .field("id_token", &redacted(&self.id_token))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("residency", &self.residency)
            .finish()
    }
}

impl CodexTokens {
    fn from_record(record: &Record) -> Self {
        Self {
            refresh_token: record.refresh_token.clone(),
            access_token: record.access_token.clone(),
            id_token: record.id_token.clone(),
            expires_at: record.expires_at,
            account_id: record.account_id.clone(),
            residency: record.residency.clone(),
        }
    }

    /// Build credentials from an `/oauth/token` response. Access tokens are
    /// valid for at most a day, whatever the server claims.
    pub(crate) fn from_response(value: &serde_json::Value) -> Result<Self> {
        let text = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let access_token = text("access_token")
            .ok_or_else(|| anyhow::anyhow!("codex: token response had no access token"))?;
        let id_token = text("id_token");
        let (account_id, residency) = routing_metadata(id_token.as_deref(), Some(&access_token));
        let lifetime = value
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(3600)
            .min(86_400);
        Ok(Self {
            refresh_token: text("refresh_token"),
            access_token: Some(access_token),
            id_token,
            expires_at: Some(now_seconds().saturating_add(lifetime)),
            account_id,
            residency,
        })
    }
}

pub(crate) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn read_codex(home: &Path) -> Result<Option<CodexTokens>> {
    let store = load(home)?;
    Ok(store.providers.get("codex").map(CodexTokens::from_record))
}

/// The stored ChatGPT credentials, if any.
pub fn codex_tokens(home: &Path) -> Option<CodexTokens> {
    read_codex(home).ok().flatten()
}

/// Codex is signed in when a refresh token is stored.
pub fn codex_logged_in(home: &Path) -> bool {
    codex_tokens(home)
        .and_then(|tokens| tokens.refresh_token)
        .is_some_and(|token| !token.trim().is_empty())
}

/// Store (or replace) the ChatGPT credentials, preserving other providers.
pub fn save_codex(home: &Path, tokens: &CodexTokens) -> Result<()> {
    let mut store = load(home)?;
    let record = store.providers.entry("codex".into()).or_default();
    record.refresh_token = tokens.refresh_token.clone();
    record.access_token = tokens.access_token.clone();
    record.id_token = tokens.id_token.clone();
    record.expires_at = tokens.expires_at;
    record.account_id = tokens.account_id.clone();
    record.residency = tokens.residency.clone();
    if record.refresh_token.is_none() && record.access_token.is_none() && record.api_key.is_none() {
        store.providers.remove("codex");
    }
    save(home, &store)
}

fn claims(token: Option<&str>) -> Option<serde_json::Value> {
    let token = token.filter(|token| token.len() <= 16_384)?;
    let payload = token.split('.').nth(1)?;
    let bytes = crate::providers::codex::b64url_decode(payload)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.is_object().then_some(value)
}

/// The ChatGPT account id and compute residency from JWT claims. These are
/// routing metadata only and are never trusted for authorization.
pub fn routing_metadata(
    id_token: Option<&str>,
    access_token: Option<&str>,
) -> (Option<String>, Option<String>) {
    let auth = "https://api.openai.com/auth";
    let text = |value: Option<&serde_json::Value>, key: &str| {
        value
            .and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };

    let mut account = None;
    for token in [id_token, access_token] {
        let Some(data) = claims(token) else {
            continue;
        };
        account = text(Some(&data), "chatgpt_account_id")
            .or_else(|| text(data.get(auth), "chatgpt_account_id"))
            .or_else(|| {
                data.get("organizations")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|organizations| organizations.first())
                    .and_then(|organization| organization.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        if account.is_some() {
            break;
        }
    }

    let access = claims(access_token);
    let residency = text(
        access.as_ref().and_then(|data| data.get(auth)),
        "chatgpt_compute_residency",
    )
    .or_else(|| text(access.as_ref(), "chatgpt_compute_residency"))
    .filter(|residency| residency != "no_constraint");
    (account, residency)
}

/// ChatGPT OAuth headers, with a refresh when the access token is missing or
/// about to expire. The clock is injectable so the refresh rule is testable.
#[allow(dead_code)] // the codex provider wires this in at M7
pub struct CodexAuth {
    home: PathBuf,
    clock: fn() -> u64,
}

impl std::fmt::Debug for CodexAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexAuth")
            .field("home", &self.home)
            .field("tokens", &"<redacted>")
            .finish()
    }
}

#[allow(dead_code)]
impl CodexAuth {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            clock: now_seconds,
        }
    }

    pub fn with_clock(home: &Path, clock: fn() -> u64) -> Self {
        Self {
            home: home.to_path_buf(),
            clock,
        }
    }

    async fn refresh(&self, stored: &CodexTokens) -> Result<CodexTokens> {
        let refresh_token = stored
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow::anyhow!("codex: not signed in; run `yoke login codex`"))?;
        let response = crate::providers::codex::token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await?;
        let fresh = CodexTokens::from_response(&response)?;
        let tokens = CodexTokens {
            refresh_token: fresh.refresh_token.or_else(|| stored.refresh_token.clone()),
            access_token: fresh.access_token,
            id_token: fresh.id_token,
            expires_at: fresh.expires_at,
            account_id: fresh.account_id.or_else(|| stored.account_id.clone()),
            residency: fresh.residency,
        };
        save_codex(&self.home, &tokens)?;
        Ok(tokens)
    }
}

/// A missing access token or one that expires within a minute needs a refresh.
#[allow(dead_code)] // used by CodexAuth and its tests
pub fn needs_refresh(now: u64, expires_at: Option<u64>) -> bool {
    expires_at.is_none_or(|expires_at| expires_at <= now.saturating_add(60))
}

#[async_trait]
impl Auth for CodexAuth {
    async fn headers(&self) -> Result<Vec<(String, String)>> {
        let mut tokens = read_codex(&self.home)?
            .ok_or_else(|| anyhow::anyhow!("codex: not signed in; run `yoke login codex`"))?;
        if tokens.access_token.is_none() || needs_refresh((self.clock)(), tokens.expires_at) {
            tokens = self.refresh(&tokens).await?;
        }
        let access = tokens
            .access_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("codex: token response had no access token"))?;
        let mut headers = vec![
            ("authorization".to_string(), format!("Bearer {access}")),
            ("originator".to_string(), "yoke".to_string()),
            (
                "user-agent".to_string(),
                format!("yoke/{}", env!("CARGO_PKG_VERSION")),
            ),
        ];
        if let Some(account) = tokens.account_id {
            headers.push(("chatgpt-account-id".to_string(), account));
        }
        if let Some(residency) = tokens.residency {
            headers.push(("x-openai-internal-codex-residency".to_string(), residency));
        }
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;

    #[tokio::test]
    async fn stored_key_wins_over_env_and_headers_are_bearer() {
        let home = TempDir::new("auth");
        set_api_key(home.path(), "groq", "stored-key").unwrap();

        let auth =
            ApiKeyAuth::new(stored_key(home.path(), "groq"), Some("env-key".into())).unwrap();
        assert_eq!(auth.source(), KeySource::Stored);
        assert_eq!(
            auth.headers().await.unwrap(),
            [("authorization".to_string(), "Bearer stored-key".to_string())]
        );
    }

    #[tokio::test]
    async fn env_is_used_when_nothing_is_stored() {
        let auth = ApiKeyAuth::new(None, Some("env-key".into())).unwrap();
        assert_eq!(auth.source(), KeySource::Env);
        assert_eq!(
            auth.headers().await.unwrap()[0].1,
            "Bearer env-key".to_string()
        );
        assert!(ApiKeyAuth::new(None, Some("  ".into())).is_none());
        assert!(ApiKeyAuth::new(None, None).is_none());
    }

    #[test]
    fn file_is_private_and_has_no_temp_left() {
        let home = TempDir::new("auth");
        set_api_key(home.path(), "groq", "secret").unwrap();

        let path = auth_path(home.path());
        assert_eq!(stored_key(home.path(), "groq").as_deref(), Some("secret"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("secret"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let names: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["auth.json"]);
    }

    #[test]
    fn set_and_remove_round_trip() {
        let home = TempDir::new("auth");
        set_api_key(home.path(), "groq", " one ").unwrap();
        set_api_key(home.path(), "openai", "two").unwrap();
        assert_eq!(stored_key(home.path(), "groq").as_deref(), Some("one"));
        assert!(remove(home.path(), "groq").unwrap());
        assert!(!remove(home.path(), "groq").unwrap());
        assert_eq!(stored_key(home.path(), "groq"), None);
        assert_eq!(stored_key(home.path(), "openai").as_deref(), Some("two"));
    }

    #[test]
    fn an_endpoint_round_trips_with_an_optional_key() {
        let home = TempDir::new("auth");
        set_endpoint(
            home.path(),
            "compat",
            " http://localhost:8000/v1 ",
            Some(" key "),
        )
        .unwrap();
        assert_eq!(
            stored_base_url(home.path(), "compat").as_deref(),
            Some("http://localhost:8000/v1")
        );
        assert_eq!(stored_key(home.path(), "compat").as_deref(), Some("key"));

        set_endpoint(home.path(), "compat", "http://localhost:9000/v1", None).unwrap();
        assert_eq!(
            stored_base_url(home.path(), "compat").as_deref(),
            Some("http://localhost:9000/v1")
        );
        assert_eq!(stored_key(home.path(), "compat"), None);
        assert_eq!(stored_base_url(home.path(), "groq"), None);
    }

    #[test]
    fn a_missing_file_is_empty_but_a_corrupt_one_is_an_error() {
        let home = TempDir::new("auth");
        assert_eq!(stored_key(home.path(), "groq"), None);
        assert!(load(home.path()).is_ok());

        std::fs::write(auth_path(home.path()), "not json").unwrap();
        assert!(load(home.path()).is_err());
        assert_eq!(stored_key(home.path(), "groq"), None);
        assert!(set_api_key(home.path(), "groq", "key").is_err());
    }

    fn token(payload: serde_json::Value) -> String {
        let payload = crate::providers::codex::b64url(payload.to_string().as_bytes());
        format!("e30.{payload}.signature")
    }

    #[test]
    fn jwt_claims_supply_account_and_residency() {
        let id = token(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc-1"},
        }));
        let access = token(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_compute_residency": "us"},
        }));
        assert_eq!(
            routing_metadata(Some(&id), Some(&access)),
            (Some("acc-1".into()), Some("us".into()))
        );

        let top = token(serde_json::json!({
            "chatgpt_account_id": "top",
            "chatgpt_compute_residency": "no_constraint",
        }));
        assert_eq!(
            routing_metadata(None, Some(&top)),
            (Some("top".into()), None)
        );

        let org = token(serde_json::json!({
            "organizations": [{"id": "org-1"}],
        }));
        assert_eq!(
            routing_metadata(Some(&org), None),
            (Some("org-1".into()), None)
        );
        assert_eq!(routing_metadata(Some("not-a-jwt"), None), (None, None));
        assert_eq!(
            routing_metadata(Some(&"x".repeat(16_385)), None),
            (None, None)
        );
    }

    #[tokio::test]
    async fn codex_headers_include_optional_routing_fields() {
        let home = TempDir::new("auth");
        save_codex(
            home.path(),
            &CodexTokens {
                refresh_token: Some("refresh".into()),
                access_token: Some("access".into()),
                expires_at: Some(2_000),
                account_id: Some("acc".into()),
                residency: Some("us".into()),
                ..CodexTokens::default()
            },
        )
        .unwrap();
        let auth = CodexAuth::with_clock(home.path(), || 1_000);
        assert_eq!(
            auth.headers().await.unwrap(),
            [
                ("authorization", "Bearer access"),
                ("originator", "yoke"),
                ("user-agent", concat!("yoke/", env!("CARGO_PKG_VERSION"))),
                ("chatgpt-account-id", "acc"),
                ("x-openai-internal-codex-residency", "us"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string()))
        );
    }

    #[tokio::test]
    async fn codex_headers_omit_unknown_routing_fields() {
        let home = TempDir::new("auth");
        save_codex(
            home.path(),
            &CodexTokens {
                access_token: Some("access".into()),
                expires_at: Some(2_000),
                ..CodexTokens::default()
            },
        )
        .unwrap();
        let headers = CodexAuth::with_clock(home.path(), || 1_000)
            .headers()
            .await
            .unwrap();
        assert_eq!(headers.len(), 3);

        save_codex(home.path(), &CodexTokens::default()).unwrap();
        let error = CodexAuth::with_clock(home.path(), || 1_000)
            .headers()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not signed in"), "{error}");
    }

    #[test]
    fn refresh_is_needed_missing_or_within_a_minute() {
        assert!(needs_refresh(1_000, None));
        assert!(needs_refresh(1_000, Some(1_060)));
        assert!(!needs_refresh(1_000, Some(1_061)));
    }

    #[test]
    fn codex_login_state_and_secrets() {
        let home = TempDir::new("auth");
        assert!(!codex_logged_in(home.path()));
        set_api_key(home.path(), "groq", "key").unwrap();
        save_codex(
            home.path(),
            &CodexTokens {
                refresh_token: Some("secret-refresh".into()),
                access_token: Some("secret-access".into()),
                ..CodexTokens::default()
            },
        )
        .unwrap();
        assert!(codex_logged_in(home.path()));
        assert_eq!(stored_key(home.path(), "groq").as_deref(), Some("key"));
        let tokens = codex_tokens(home.path()).unwrap();
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("secret-access"), "{debug}");
        assert!(!debug.contains("secret-refresh"), "{debug}");

        assert!(remove(home.path(), "codex").unwrap());
        assert!(!codex_logged_in(home.path()));
    }

    #[test]
    fn api_key_records_still_load_without_codex_fields() {
        let home = TempDir::new("auth");
        std::fs::write(
            auth_path(home.path()),
            r#"{"providers":{"groq":{"api_key":"old"}}}"#,
        )
        .unwrap();
        assert_eq!(stored_key(home.path(), "groq").as_deref(), Some("old"));
        assert!(!codex_logged_in(home.path()));
    }
}

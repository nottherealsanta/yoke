//! A user-configured OpenAI-compatible server: the chat dialect with a base URL
//! and an optional API key.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use super::chat::{self, ChatQuirks};
use super::Auth;

pub const ENV_BASE_URL: &str = "OPENAI_COMPAT_BASE_URL";
pub const ENV_API_KEY: &str = "OPENAI_COMPAT_API_KEY";
pub const LABEL: &str = "OpenAI-compatible";

/// `<base>/chat/completions`; the base is used exactly as the user gives it, so
/// it must already include any `/v1`.
pub fn endpoint(base: &str) -> Result<String> {
    Ok(format!("{}/chat/completions", normalize_base_url(base)?))
}

pub fn models_url(base: &str) -> Result<String> {
    Ok(format!("{}/models", normalize_base_url(base)?))
}

/// Trim, drop trailing slashes, and assume `http://` when no scheme is given.
pub fn normalize_base_url(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("compat: the base URL is empty");
    }
    if value.chars().any(char::is_whitespace) {
        anyhow::bail!("compat: that does not look like a base URL");
    }
    let mut base = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    while base.ends_with('/') {
        base.pop();
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        anyhow::bail!("compat: only http and https base URLs are supported");
    }
    Ok(base)
}

/// No extra chat quirks: no session header and no reasoning replay.
pub fn quirks() -> ChatQuirks {
    ChatQuirks::default()
}

/// Used when the server needs no key; the request then has no Authorization.
pub struct NoAuth;

#[async_trait]
impl Auth for NoAuth {
    async fn headers(&self) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
}

pub fn new(base: &str, auth: Option<Box<dyn Auth>>) -> Result<chat::ChatProvider> {
    Ok(chat::ChatProvider::new(
        "compat",
        endpoint(base)?,
        auth.unwrap_or_else(|| Box::new(NoAuth)),
        quirks(),
    ))
}

/// The model ids the server lists at `GET <base>/models`.
pub async fn models(base: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    let url = models_url(base)?;
    let mut request = reqwest::Client::new().get(url.as_str());
    if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("compat: cannot reach {url}"))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("compat: HTTP {status} from {url}");
    }
    parse_models(&response.text().await?)
}

/// Pull model ids out of an OpenAI-style `/models` response.
pub fn parse_models(body: &str) -> Result<Vec<String>> {
    let value: Value = serde_json::from_str(body).context("compat: malformed /models response")?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ApiKeyAuth;
    use crate::providers::Provider;
    use crate::types::{Message, Part, Request, Role};
    use serde_json::json;

    fn request() -> Request {
        Request {
            model: "local-model".into(),
            instructions: "Be brief.".into(),
            messages: vec![
                Message {
                    role: Role::User,
                    parts: vec![Part::Text {
                        text: "What is in a.txt?".into(),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![Part::ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        args: json!({"path": "a.txt"}),
                    }],
                },
                Message {
                    role: Role::Tool,
                    parts: vec![Part::ToolResult {
                        id: "call_1".into(),
                        output: "1\thello".into(),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![json!({"name": "read", "parameters": {"type": "object"}})],
            effort: Some("low".into()),
            conversation_id: Some("conv-123".into()),
        }
    }

    #[test]
    fn base_urls_are_normalised_and_keep_the_users_v1() {
        assert_eq!(
            normalize_base_url("  https://api.example.com/v1/  ").unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("127.0.0.1:8080/v1").unwrap(),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            endpoint("http://localhost:8000/v1").unwrap(),
            "http://localhost:8000/v1/chat/completions"
        );
        assert_eq!(
            models_url("http://localhost:8000/v1//").unwrap(),
            "http://localhost:8000/v1/models"
        );
    }

    #[test]
    fn a_base_url_must_be_an_http_address() {
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("two words").is_err());
        assert!(normalize_base_url("ftp://example.com/v1").is_err());
        assert!(normalize_base_url("http://").is_err());
    }

    #[test]
    fn body_is_exact_chat_json_without_quirks() {
        assert_eq!(
            new("http://localhost:8000/v1", None)
                .unwrap()
                .body(&request()),
            json!({
                "model": "local-model",
                "messages": [
                    {"role": "system", "content": "Be brief."},
                    {"role": "user", "content": "What is in a.txt?"},
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "read", "arguments": "{\"path\":\"a.txt\"}"},
                        }],
                    },
                    {"role": "tool", "tool_call_id": "call_1", "content": "1\thello"},
                ],
                "stream": true,
                "stream_options": {"include_usage": true},
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "read",
                        "parameters": {"type": "object"},
                    },
                }],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
            })
        );
    }

    #[tokio::test]
    async fn authorization_is_sent_only_when_a_key_is_set() {
        let transport = vec![
            (
                "user-agent".to_string(),
                concat!("yoke/", env!("CARGO_PKG_VERSION")).to_string(),
            ),
            ("content-type".to_string(), "application/json".to_string()),
            ("accept".to_string(), "text/event-stream".to_string()),
        ];
        assert_eq!(
            chat::request_headers(&quirks(), None, NoAuth.headers().await.unwrap()),
            transport
        );

        let auth = ApiKeyAuth::new(None, Some("secret".into())).unwrap();
        let headers = chat::request_headers(&quirks(), None, auth.headers().await.unwrap());
        assert_eq!(
            headers.last().unwrap(),
            &("authorization".to_string(), "Bearer secret".to_string())
        );
    }

    #[test]
    fn model_list_sample_yields_ids() {
        let sample = r#"{
            "object": "list",
            "data": [
                {"id": "gpt-oss-120b", "object": "model", "created": 1735689600, "owned_by": "local"},
                {"id": "qwen2.5-coder-7b", "object": "model", "created": 1735689600, "owned_by": "local"}
            ]
        }"#;
        assert_eq!(
            parse_models(sample).unwrap(),
            ["gpt-oss-120b".to_string(), "qwen2.5-coder-7b".to_string()]
        );
        assert_eq!(
            parse_models(r#"{"object":"list","data":[]}"#).unwrap(),
            Vec::<String>::new()
        );
        assert!(parse_models("not json").is_err());
    }
}

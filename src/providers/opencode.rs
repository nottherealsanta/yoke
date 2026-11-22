//! OpenCode Go: the Chat Completions dialect with a Bearer API key and the
//! `x-opencode-session` routing header.

use super::chat::{self, ChatQuirks};
use super::Auth;

pub const ENDPOINT: &str = "https://opencode.ai/zen/go/v1/chat/completions";
pub const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";
pub const ENV: &str = "OPENCODE_API_KEY";

pub fn quirks() -> ChatQuirks {
    ChatQuirks {
        session_header: Some("x-opencode-session"),
        reasoning_replay: Some("reasoning_content"),
        ..ChatQuirks::default()
    }
}

pub fn new(auth: Box<dyn Auth>) -> chat::ChatProvider {
    chat::ChatProvider::new("opencode", ENDPOINT, auth, quirks())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use crate::types::{Message, Part, Request, Role};
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    struct NoAuth;

    #[async_trait]
    impl Auth for NoAuth {
        async fn headers(&self) -> Result<Vec<(String, String)>> {
            Ok(Vec::new())
        }
    }

    fn request() -> Request {
        Request {
            model: DEFAULT_MODEL.into(),
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
                    parts: vec![
                        Part::Reasoning {
                            text: "Need to read".into(),
                            raw: None,
                        },
                        Part::ToolCall {
                            id: "call_1".into(),
                            name: "read".into(),
                            args: json!({"path": "a.txt"}),
                        },
                    ],
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
            effort: None,
            conversation_id: Some("conv-123".into()),
        }
    }

    #[test]
    fn body_is_exact_chat_json_with_replayed_reasoning() {
        assert_eq!(
            new(Box::new(NoAuth)).body(&request()),
            json!({
                "model": "deepseek-v4.1-flash",
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
                        "reasoning_content": "Need to read",
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

    #[test]
    fn exact_request_headers_include_the_session() {
        let headers = chat::request_headers(
            &quirks(),
            Some("conv-123"),
            vec![("authorization".into(), "Bearer test".into())],
        );
        assert_eq!(
            headers,
            vec![
                (
                    "user-agent".to_string(),
                    concat!("yoke/", env!("CARGO_PKG_VERSION")).to_string()
                ),
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "text/event-stream".to_string()),
                ("authorization".to_string(), "Bearer test".to_string()),
                ("x-opencode-session".to_string(), "conv-123".to_string()),
            ]
        );
    }

    #[test]
    fn session_header_is_cut_to_64_chars_and_omitted_without_an_id() {
        let long = "x".repeat(100);
        let headers = chat::request_headers(&quirks(), Some(&long), Vec::new());
        assert_eq!(headers.last().unwrap().0, "x-opencode-session");
        assert_eq!(headers.last().unwrap().1.len(), 64);
        assert!(!chat::request_headers(&quirks(), None, Vec::new())
            .iter()
            .any(|(name, _)| name == "x-opencode-session"));
    }
}

//! Groq: the Chat Completions dialect with a Bearer API key.

use super::chat::{self, ChatQuirks};
use super::Auth;

pub const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_MODEL: &str = "openai/gpt-oss-120b";
pub const ENV: &str = "GROQ_API_KEY";

pub fn new(auth: Box<dyn Auth>) -> chat::ChatProvider {
    chat::ChatProvider::new(
        "groq",
        ENDPOINT,
        auth,
        ChatQuirks {
            reasoning_effort: true,
            ..ChatQuirks::default()
        },
    )
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

    #[test]
    fn body_is_the_chat_dialect_and_model_is_kept() {
        let req = Request {
            model: DEFAULT_MODEL.into(),
            instructions: "Be brief.".into(),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text { text: "hi".into() }],
            }],
            tools: vec![json!({"name": "read", "parameters": {"type": "object"}})],
            effort: Some("low".into()),
            conversation_id: None,
        };
        let value = new(Box::new(NoAuth)).body(&req);
        assert_eq!(value["model"], DEFAULT_MODEL);
        assert_eq!(value["stream"], true);
        assert_eq!(value["parallel_tool_calls"], false);
        assert_eq!(value["reasoning_effort"], "low");
    }
}

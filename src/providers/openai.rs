//! OpenAI: the Responses dialect with a Bearer API key.

use super::responses::{self, ResponsesQuirks};
use super::Auth;

pub const ENDPOINT: &str = "https://api.openai.com/v1/responses";
pub const DEFAULT_MODEL: &str = "gpt-6.1-terra";
pub const ENV: &str = "OPENAI_API_KEY";

pub fn quirks() -> ResponsesQuirks {
    ResponsesQuirks {
        encrypted_reasoning: true,
        prompt_cache_key: true,
        ..ResponsesQuirks::default()
    }
}

pub fn new(auth: Box<dyn Auth>) -> responses::ResponsesProvider {
    responses::ResponsesProvider::new("openai", ENDPOINT, auth, quirks())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use crate::types::{Message, Part, Request, Role};
    use serde_json::json;

    fn auth() -> Box<dyn Auth> {
        Box::new(crate::auth::ApiKeyAuth::new(None, Some("key".into())).unwrap())
    }

    fn request() -> Request {
        Request {
            model: DEFAULT_MODEL.into(),
            instructions: "Be brief.".into(),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text { text: "hi".into() }],
            }],
            tools: vec![json!({"name": "read", "parameters": {"type": "object"}})],
            effort: None,
            conversation_id: Some("conv-123".into()),
        }
    }

    #[test]
    fn body_is_the_responses_dialect_with_openai_quirks() {
        let value = new(auth()).body(&request());
        assert_eq!(value["model"], DEFAULT_MODEL);
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(value["prompt_cache_key"], "conv-123");
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["parallel_tool_calls"], false);
    }

    #[test]
    fn headers_are_transport_then_bearer() {
        assert_eq!(
            responses::request_headers(
                &quirks(),
                Some("conv-123"),
                vec![("authorization".into(), "Bearer key".into())],
            ),
            vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "text/event-stream".to_string()),
                ("authorization".to_string(), "Bearer key".to_string()),
            ]
        );
    }
}

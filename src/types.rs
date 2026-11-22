//! Conversation, request and stream event types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One message in the conversation: a role and its parts, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// Everything a message can hold. `ToolCall` args are the raw JSON arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        /// The provider's own reasoning item, replayed verbatim by the Responses
        /// dialect so its `encrypted_content` survives. Absent in older sessions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<Value>,
    },
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    Image {
        media_type: String,
        data: String,
    },
}

/// One model request: the full visible context, resent every step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub instructions: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Value>,
    pub effort: Option<String>,
    pub conversation_id: Option<String>,
}

/// Normalized provider output, forwarded to the UI as it streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    PartDone {
        part: Part,
    },
    Usage(Usage),
    Done {
        stop: String,
    },
    /// The exact response body, for mode 3.
    Wire(Value),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value);
    }

    #[test]
    fn every_type_round_trips() {
        for part in [
            Part::Text { text: "hi".into() },
            Part::Reasoning {
                text: "think".into(),
                raw: Some(json!({"type": "reasoning", "id": "rs_1"})),
            },
            Part::Reasoning {
                text: "old".into(),
                raw: None,
            },
            Part::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                args: json!({"path": "a.rs"}),
            },
            Part::ToolResult {
                id: "c1".into(),
                output: "ok".into(),
                is_error: false,
            },
            Part::Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
            },
        ] {
            round_trip(&part);
        }

        for event in [
            StreamEvent::TextDelta { text: "a".into() },
            StreamEvent::ReasoningDelta { text: "r".into() },
            StreamEvent::PartDone {
                part: Part::Text { text: "b".into() },
            },
            StreamEvent::Usage(Usage {
                input_tokens: 3,
                output_tokens: 4,
            }),
            StreamEvent::Done {
                stop: "end_turn".into(),
            },
            StreamEvent::Wire(json!({"id": "x"})),
        ] {
            round_trip(&event);
        }

        round_trip(&Message {
            role: Role::Assistant,
            parts: vec![Part::Text { text: "hi".into() }],
        });

        let old: Part = serde_json::from_str(r#"{"type":"reasoning","text":"old"}"#).unwrap();
        assert_eq!(
            old,
            Part::Reasoning {
                text: "old".into(),
                raw: None
            }
        );

        round_trip(&Request {
            model: "m".into(),
            instructions: "be brief".into(),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text { text: "hi".into() }],
            }],
            tools: vec![json!({"name": "read"})],
            effort: Some("low".into()),
            conversation_id: None,
        });
    }
}

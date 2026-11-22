//! The OpenAI Responses dialect, shared by `openai` and `codex`.
//!
//! `body` builds the exact request JSON and `stream` posts it, turning the SSE
//! stream into normalized events. Providers supply their URL, auth and quirks.
//!
//! Requests are always `store: false` and streamed, so the whole history is
//! resent each time. Text and function calls are rebuilt from the conversation
//! parts; a reasoning item is replayed verbatim because only its provider's own
//! item carries `encrypted_content`.

use std::collections::{HashSet, VecDeque};

use anyhow::{Context, Result};
use futures::stream::{BoxStream, Stream, StreamExt};
use serde_json::{json, Value};

use super::sse;
use super::{Auth, Provider};
use crate::types::{Part, Request, Role, StreamEvent, Usage};

pub const CACHE_KEY_MAX: usize = 64;
const ERROR_CHARS: usize = 4_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct ResponsesQuirks {
    /// Ask for `reasoning.encrypted_content`, and replay reasoning items that carry it.
    pub encrypted_reasoning: bool,
    /// Send the conversation id as `prompt_cache_key`.
    pub prompt_cache_key: bool,
    /// Header that carries the conversation id, cut to 64 characters.
    pub session_header: Option<&'static str>,
}

/// The exact JSON body for one request.
pub fn body(req: &Request, quirks: &ResponsesQuirks) -> Value {
    let mut body = json!({
        "model": req.model,
        "instructions": req.instructions,
        "input": input(req, quirks),
        "tools": tools(req),
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": reasoning(req),
        "store": false,
        "stream": true,
    });
    if quirks.encrypted_reasoning {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if quirks.prompt_cache_key {
        if let Some(id) = req.conversation_id.as_deref().filter(|id| !id.is_empty()) {
            body["prompt_cache_key"] = json!(id.chars().take(CACHE_KEY_MAX).collect::<String>());
        }
    }
    body
}

fn tools(req: &Request) -> Vec<Value> {
    req.tools
        .iter()
        .map(|tool| {
            let mut value = json!({
                "type": "function",
                "name": tool.get("name").cloned().unwrap_or(Value::Null),
                "parameters": tool
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
                "strict": false,
            });
            if let Some(description) = tool.get("description") {
                value["description"] = description.clone();
            }
            value
        })
        .collect()
}

fn reasoning(req: &Request) -> Value {
    let mut value = json!({"summary": "auto"});
    if let Some(effort) = &req.effort {
        value["effort"] = json!(effort);
    }
    value
}

fn input(req: &Request, quirks: &ResponsesQuirks) -> Vec<Value> {
    let mut items = Vec::new();
    for message in &req.messages {
        let start = items.len();
        for part in &message.parts {
            match part {
                Part::Text { text } => items.push(if message.role == Role::Assistant {
                    json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    })
                } else {
                    json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    })
                }),
                Part::Image { media_type, data } => {
                    let image = json!({
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data}"),
                    });
                    let appended = items.len() > start
                        && items
                            .last()
                            .and_then(|item| item.get("role"))
                            .and_then(Value::as_str)
                            == Some("user");
                    if appended {
                        if let Some(content) = items
                            .last_mut()
                            .and_then(|item| item.get_mut("content"))
                            .and_then(Value::as_array_mut)
                        {
                            content.push(image);
                        }
                    } else {
                        items.push(json!({"type": "message", "role": "user", "content": [image]}));
                    }
                }
                Part::Reasoning { raw, .. } => {
                    if let Some(raw) = raw.as_ref().filter(|_| quirks.encrypted_reasoning) {
                        if raw
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .is_some_and(|content| !content.is_empty())
                        {
                            items.push(raw.clone());
                        }
                    }
                }
                Part::ToolCall { id, name, args } => items.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments(args),
                })),
                Part::ToolResult { id, output, .. } => items.push(json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": output,
                })),
            }
        }
    }
    items
}

fn arguments(args: &Value) -> String {
    match args {
        // A chat parser fallback keeps malformed-but-raw arguments as a string.
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".into()),
    }
}

/// Every request header, in a stable order: transport, auth, then the session.
pub fn request_headers(
    quirks: &ResponsesQuirks,
    conversation_id: Option<&str>,
    auth: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
    ];
    headers.extend(auth);
    if let (Some(name), Some(id)) = (
        quirks.session_header,
        conversation_id.filter(|id| !id.is_empty()),
    ) {
        headers.push((name.into(), id.chars().take(CACHE_KEY_MAX).collect()));
    }
    headers
}

/// A provider that speaks only the Responses dialect, built from a few quirks.
pub struct ResponsesProvider {
    id: &'static str,
    url: &'static str,
    auth: Box<dyn Auth>,
    quirks: ResponsesQuirks,
}

impl ResponsesProvider {
    pub fn new(
        id: &'static str,
        url: &'static str,
        auth: Box<dyn Auth>,
        quirks: ResponsesQuirks,
    ) -> Self {
        Self {
            id,
            url,
            auth,
            quirks,
        }
    }
}

#[async_trait::async_trait]
impl Provider for ResponsesProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn body(&self, req: &Request) -> Value {
        body(req, &self.quirks)
    }

    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let auth = self.auth.headers().await?;
        let headers = request_headers(&self.quirks, req.conversation_id.as_deref(), auth);
        stream(self.id, self.url, headers, req, &self.quirks).await
    }
}

/// POST the body and stream normalized events, starting with the exact body.
pub async fn stream(
    provider: &'static str,
    url: &str,
    headers: Vec<(String, String)>,
    req: &Request,
    quirks: &ResponsesQuirks,
) -> Result<BoxStream<'static, Result<StreamEvent>>> {
    let body = body(req, quirks);
    let mut request = reqwest::Client::new()
        .post(url)
        .body(serde_json::to_vec(&body)?);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("{provider}: request failed"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("{provider}: HTTP {status}: {text}");
    }

    let wire = futures::stream::once(async move { Ok(StreamEvent::Wire(body)) });
    Ok(Box::pin(wire.chain(parse_response(provider, response))))
}

struct Input {
    response: reqwest::Response,
    parser: StreamParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
}

fn parse_response(
    provider: &'static str,
    response: reqwest::Response,
) -> impl Stream<Item = Result<StreamEvent>> + Send {
    let input = Input {
        response,
        parser: StreamParser::new(provider),
        pending: VecDeque::new(),
        done: false,
    };
    futures::stream::try_unfold(input, |mut input| async move {
        loop {
            if let Some(event) = input.pending.pop_front() {
                return Ok(Some((event, input)));
            }
            if input.done {
                return Ok(None);
            }
            match input.response.chunk().await {
                Ok(Some(chunk)) => {
                    for event in input.parser.feed(&chunk)? {
                        input.pending.push_back(event);
                    }
                }
                Ok(None) => {
                    for event in input.parser.finish()? {
                        input.pending.push_back(event);
                    }
                    input.done = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
    })
}

/// Stateful SSE-to-event parser; chunks may arrive at any byte boundary.
#[derive(Debug)]
pub struct StreamParser {
    sse: sse::Parser,
    provider: String,
    /// Message items whose text already arrived as deltas.
    text_items: HashSet<String>,
    summary_parts: usize,
    saw_tool_call: bool,
    done: bool,
}

impl StreamParser {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            sse: sse::Parser::default(),
            provider: provider.into(),
            text_items: HashSet::new(),
            summary_parts: 0,
            saw_tool_call: false,
            done: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<StreamEvent>> {
        let mut events = Vec::new();
        for event in self.sse.feed(chunk) {
            self.event(event.event.as_deref(), &event.data, &mut events)?;
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        let mut events = Vec::new();
        for event in self.sse.finish() {
            self.event(event.event.as_deref(), &event.data, &mut events)?;
        }
        if !self.done {
            self.done(&mut events);
        }
        Ok(events)
    }

    fn event(
        &mut self,
        name: Option<&str>,
        data: &str,
        events: &mut Vec<StreamEvent>,
    ) -> Result<()> {
        if data.trim() == "[DONE]" {
            if !self.done {
                self.done(events);
            }
            return Ok(());
        }
        let value: Value = serde_json::from_str(data)
            .context("responses: malformed stream chunk from provider")?;
        self.data(name, &value, events)
    }

    fn data(
        &mut self,
        name: Option<&str>,
        value: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<()> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(name)
            .unwrap_or("");
        match kind {
            "error" | "response.failed" => {
                let error = value
                    .get("error")
                    .or_else(|| {
                        value
                            .get("response")
                            .and_then(|response| response.get("error"))
                    })
                    .unwrap_or(&Value::Null);
                let code = error
                    .get("code")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("type").and_then(Value::as_str))
                    .unwrap_or(kind);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(ERROR_CHARS)
                    .collect::<String>();
                anyhow::bail!("{}: stream error {code}: {message}", self.provider);
            }
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => self.saw_tool_call = true,
                    Some("reasoning") => self.summary_parts = 0,
                    _ => {}
                }
            }
            "response.reasoning_summary_part.added" => {
                self.summary_parts += 1;
                if self.summary_parts > 1 {
                    events.push(StreamEvent::ReasoningDelta {
                        text: "\n\n".into(),
                    });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    if !text.is_empty() {
                        events.push(StreamEvent::ReasoningDelta { text: text.into() });
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(id) = value.get("item_id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        self.text_items.insert(id.into());
                    }
                }
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    if !text.is_empty() {
                        events.push(StreamEvent::TextDelta { text: text.into() });
                    }
                }
            }
            // The finished item carries the complete arguments; Yoke has no
            // live argument-delta event, so these are not normalized.
            "response.function_call_arguments.delta" => {}
            "response.output_item.done" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                        if !self.text_items.contains(id) {
                            let text = join(item.get("content"), Some("output_text"));
                            if !text.is_empty() {
                                events.push(StreamEvent::PartDone {
                                    part: Part::Text { text },
                                });
                            }
                        }
                    }
                    Some("reasoning") => {
                        let text = if item
                            .get("summary")
                            .and_then(Value::as_array)
                            .is_some_and(|summary| !summary.is_empty())
                        {
                            join(item.get("summary"), None)
                        } else {
                            join(item.get("content"), None)
                        };
                        events.push(StreamEvent::PartDone {
                            part: Part::Reasoning {
                                text,
                                raw: Some(item.clone()),
                            },
                        });
                    }
                    Some("function_call") => {
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                        let args = parse_arguments(item.get("arguments"));
                        events.push(StreamEvent::PartDone {
                            part: Part::ToolCall {
                                id: id.into(),
                                name: name.into(),
                                args,
                            },
                        });
                    }
                    _ => {}
                }
            }
            "response.completed" => {
                self.usage(value, events);
                self.done(events);
            }
            "response.incomplete" => {
                self.usage(value, events);
                let reason = value
                    .get("response")
                    .and_then(|response| response.get("incomplete_details"))
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let stop = if reason == "max_output_tokens" {
                    "max_tokens"
                } else {
                    "other"
                };
                if !self.done {
                    events.push(StreamEvent::Done { stop: stop.into() });
                    self.done = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn usage(&self, value: &Value, events: &mut Vec<StreamEvent>) {
        let Some(usage) = value
            .get("response")
            .and_then(|response| response.get("usage"))
            .filter(|usage| usage.is_object())
        else {
            return;
        };
        events.push(StreamEvent::Usage(Usage {
            input_tokens: number(usage, "input_tokens"),
            output_tokens: number(usage, "output_tokens"),
        }));
    }

    fn done(&mut self, events: &mut Vec<StreamEvent>) {
        if self.done {
            return;
        }
        let stop = if self.saw_tool_call {
            "tool_calls"
        } else {
            "end_turn"
        };
        events.push(StreamEvent::Done { stop: stop.into() });
        self.done = true;
    }
}

fn join(content: Option<&Value>, kind: Option<&str>) -> String {
    content
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| match kind {
                    Some(kind) => part.get("type").and_then(Value::as_str) == Some(kind),
                    None => true,
                })
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn parse_arguments(arguments: Option<&Value>) -> Value {
    let text = match arguments {
        Some(Value::String(text)) => text.as_str(),
        Some(other) if !other.is_null() => return other.clone(),
        _ => "",
    };
    if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()))
    }
}

fn number(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// Redaction is applied to the request headers in mode 3.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    const REASONING: &str = r#"{"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"Need to read"}],"encrypted_content":"gAAA"}"#;
    const CALL: &str = r#"{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"read","arguments":"{\"path\":\"a.txt\"}"}"#;

    fn codex_quirks() -> ResponsesQuirks {
        ResponsesQuirks {
            encrypted_reasoning: true,
            prompt_cache_key: true,
            session_header: Some("session_id"),
        }
    }

    fn request() -> Request {
        Request {
            model: "gpt-6.1-terra".into(),
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
                            raw: Some(serde_json::from_str(REASONING).unwrap()),
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
            tools: vec![json!({
                "name": "read",
                "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
            })],
            effort: Some("medium".into()),
            conversation_id: Some("conv-123".into()),
        }
    }

    #[test]
    fn body_is_exact_responses_json_with_replayed_reasoning() {
        let reasoning: Value = serde_json::from_str(REASONING).unwrap();
        assert_eq!(
            body(&request(), &codex_quirks()),
            json!({
                "model": "gpt-6.1-terra",
                "instructions": "Be brief.",
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "What is in a.txt?"}]},
                    reasoning,
                    {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a.txt\"}"},
                    {"type": "function_call_output", "call_id": "call_1", "output": "1\thello"},
                ],
                "tools": [{
                    "type": "function",
                    "name": "read",
                    "description": "Read a file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
                    "strict": false,
                }],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
                "reasoning": {"summary": "auto", "effort": "medium"},
                "store": false,
                "stream": true,
                "include": ["reasoning.encrypted_content"],
                "prompt_cache_key": "conv-123",
            })
        );
    }

    #[test]
    fn quirks_off_drop_reasoning_and_omit_include_and_cache_key() {
        let mut req = request();
        req.effort = None;
        let value = body(&req, &ResponsesQuirks::default());
        assert!(value.get("include").is_none());
        assert!(value.get("prompt_cache_key").is_none());
        assert_eq!(value["reasoning"], json!({"summary": "auto"}));
        assert!(!value["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "reasoning"));
    }

    #[test]
    fn images_join_the_user_message_that_precedes_them() {
        let mut req = request();
        req.messages[0].parts = vec![
            Part::Text {
                text: "look".into(),
            },
            Part::Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
            },
        ];
        let value = body(&req, &codex_quirks());
        assert_eq!(
            value["input"][0],
            json!({
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "look"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AA=="},
                ],
            })
        );
    }

    fn events(stream: &[(&str, &str)]) -> Vec<StreamEvent> {
        let mut parser = StreamParser::new("codex");
        let mut events = Vec::new();
        for (name, data) in stream {
            events.extend(
                parser
                    .feed(format!("event: {name}\ndata: {data}\n\n").as_bytes())
                    .unwrap(),
            );
        }
        events.extend(parser.finish().unwrap());
        events
    }

    #[test]
    fn recorded_stream_normalizes_to_deltas_parts_usage_and_done() {
        let events = events(&[
            (
                "response.created",
                r#"{"type":"response.created","response":{"id":"resp_1"}}"#,
            ),
            (
                "response.output_item.added",
                r#"{"type":"response.output_item.added","item":{"id":"rs_1","type":"reasoning"}}"#,
            ),
            (
                "response.reasoning_summary_part.added",
                r#"{"type":"response.reasoning_summary_part.added"}"#,
            ),
            (
                "response.reasoning_summary_text.delta",
                r#"{"type":"response.reasoning_summary_text.delta","delta":"Need"}"#,
            ),
            (
                "response.reasoning_summary_text.delta",
                r#"{"type":"response.reasoning_summary_text.delta","delta":" to read"}"#,
            ),
            (
                "response.output_item.done",
                &format!(r#"{{"type":"response.output_item.done","item":{REASONING}}}"#),
            ),
            (
                "response.output_item.added",
                r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read","arguments":""}}"#,
            ),
            (
                "response.function_call_arguments.delta",
                r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}"#,
            ),
            (
                "response.function_call_arguments.delta",
                r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"a.txt\"}"}"#,
            ),
            (
                "response.output_item.done",
                &format!(r#"{{"type":"response.output_item.done","item":{CALL}}}"#),
            ),
            (
                "response.completed",
                r#"{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":80},"output_tokens":20,"output_tokens_details":{"reasoning_tokens":7}}}}"#,
            ),
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::ReasoningDelta {
                    text: "Need".into()
                },
                StreamEvent::ReasoningDelta {
                    text: " to read".into()
                },
                StreamEvent::PartDone {
                    part: Part::Reasoning {
                        text: "Need to read".into(),
                        raw: Some(serde_json::from_str(REASONING).unwrap()),
                    }
                },
                StreamEvent::PartDone {
                    part: Part::ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        args: json!({"path": "a.txt"}),
                    }
                },
                StreamEvent::Usage(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                }),
                StreamEvent::Done {
                    stop: "tool_calls".into()
                },
            ]
        );
    }

    #[test]
    fn text_deltas_are_the_message_and_a_done_item_does_not_duplicate_them() {
        let events = events(&[
            (
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","item_id":"msg_1","delta":"Hel"}"#,
            ),
            (
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","item_id":"msg_1","delta":"lo"}"#,
            ),
            (
                "response.output_item.done",
                r#"{"type":"response.output_item.done","item":{"id":"msg_1","type":"message","content":[{"type":"output_text","text":"Hello"}]}}"#,
            ),
            (
                "response.completed",
                r#"{"type":"response.completed","response":{"id":"resp_1"}}"#,
            ),
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta { text: "Hel".into() },
                StreamEvent::TextDelta { text: "lo".into() },
                StreamEvent::Done {
                    stop: "end_turn".into()
                },
            ]
        );
    }

    #[test]
    fn a_message_item_without_deltas_is_emitted_whole() {
        let events = events(&[(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","item":{"id":"msg_1","type":"message","content":[{"type":"output_text","text":"hi"}]}}"#,
        )]);
        assert_eq!(
            events,
            [
                StreamEvent::PartDone {
                    part: Part::Text { text: "hi".into() }
                },
                StreamEvent::Done {
                    stop: "end_turn".into()
                },
            ]
        );
    }

    #[test]
    fn summary_parts_are_separated_by_a_blank_line() {
        let events = events(&[
            (
                "response.reasoning_summary_part.added",
                r#"{"type":"response.reasoning_summary_part.added"}"#,
            ),
            (
                "response.reasoning_summary_text.delta",
                r#"{"type":"response.reasoning_summary_text.delta","delta":"one"}"#,
            ),
            (
                "response.reasoning_summary_part.added",
                r#"{"type":"response.reasoning_summary_part.added"}"#,
            ),
            (
                "response.reasoning_summary_text.delta",
                r#"{"type":"response.reasoning_summary_text.delta","delta":"two"}"#,
            ),
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::ReasoningDelta { text: "one".into() },
                StreamEvent::ReasoningDelta {
                    text: "\n\n".into()
                },
                StreamEvent::ReasoningDelta { text: "two".into() },
                StreamEvent::Done {
                    stop: "end_turn".into()
                },
            ]
        );
    }

    #[test]
    fn incomplete_and_error_events() {
        let got = events(&[(
            "response.incomplete",
            r#"{"type":"response.incomplete","response":{"id":"r","incomplete_details":{"reason":"max_output_tokens"}}}"#,
        )]);
        assert_eq!(
            got,
            [StreamEvent::Done {
                stop: "max_tokens".into()
            }]
        );

        let got = events(&[(
            "response.incomplete",
            r#"{"type":"response.incomplete","response":{"id":"r","incomplete_details":{"reason":"content_filter"}}}"#,
        )]);
        assert_eq!(
            got,
            [StreamEvent::Done {
                stop: "other".into()
            }]
        );

        let mut parser = StreamParser::new("codex");
        let error = parser
            .feed(
                br#"event: response.failed
data: {"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}

"#,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("stream error server_error"),
            "{error}"
        );
        assert!(error.to_string().contains("boom"), "{error}");
    }

    #[test]
    fn bytes_split_everywhere() {
        let input = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"delta\":\"héllo\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\"}}\n\n",
        );
        let mut parser = StreamParser::new("openai");
        let mut events = Vec::new();
        for byte in input.as_bytes() {
            events.extend(parser.feed(std::slice::from_ref(byte)).unwrap());
        }
        events.extend(parser.finish().unwrap());
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta {
                    text: "héllo".into()
                },
                StreamEvent::Done {
                    stop: "end_turn".into()
                },
            ]
        );
    }

    #[test]
    fn headers_carry_auth_then_the_session_header() {
        let headers = request_headers(
            &codex_quirks(),
            Some("conv-123"),
            vec![("authorization".into(), "Bearer test".into())],
        );
        assert_eq!(
            headers,
            vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "text/event-stream".to_string()),
                ("authorization".to_string(), "Bearer test".to_string()),
                ("session_id".to_string(), "conv-123".to_string()),
            ]
        );
        let long = "x".repeat(100);
        let headers = request_headers(&codex_quirks(), Some(&long), Vec::new());
        assert_eq!(headers.last().unwrap().1.len(), CACHE_KEY_MAX);
        assert!(!request_headers(&codex_quirks(), None, Vec::new())
            .iter()
            .any(|(name, _)| name == "session_id"));
    }
}

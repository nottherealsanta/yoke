//! The OpenAI Chat Completions dialect, shared by chat providers.
//!
//! `body` builds the exact request JSON and `stream` posts it, turning the SSE
//! stream into normalized events. Providers supply their URL, auth and quirks.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use futures::stream::{BoxStream, Stream, StreamExt};
use serde_json::{json, Value};

use super::sse;
use super::{Auth, Provider};
use crate::types::{Message, Part, Request, Role, StreamEvent, Usage};

#[derive(Debug, Clone, Copy, Default)]
pub struct ChatQuirks {
    /// Send `reasoning_effort` when the request has an effort.
    pub reasoning_effort: bool,
    /// Header that carries the conversation id, cut to 64 characters.
    pub session_header: Option<&'static str>,
    /// Replay reasoning under this field on assistant messages.
    pub reasoning_replay: Option<&'static str>,
}

/// The exact JSON body for one request.
pub fn body(req: &Request, quirks: &ChatQuirks) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": messages(req, quirks),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|tool| json!({"type": "function", "function": tool}))
                .collect(),
        );
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(false);
    }
    if quirks.reasoning_effort {
        if let Some(effort) = &req.effort {
            body["reasoning_effort"] = json!(effort);
        }
    }
    body
}

fn messages(req: &Request, quirks: &ChatQuirks) -> Vec<Value> {
    let mut out = Vec::new();
    if !req.instructions.is_empty() {
        out.push(json!({"role": "system", "content": req.instructions}));
    }
    for message in &req.messages {
        match message.role {
            Role::User => out.push(user_message(message)),
            Role::Assistant => out.push(assistant_message(message, quirks)),
            Role::Tool => {
                for part in &message.parts {
                    if let Part::ToolResult { id, output, .. } = part {
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": output,
                        }));
                    }
                }
            }
        }
    }
    out
}

fn user_message(message: &Message) -> Value {
    let images = message
        .parts
        .iter()
        .any(|part| matches!(part, Part::Image { .. }));
    if !images {
        return json!({"role": "user", "content": text(message)});
    }
    let mut content = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text { text } if !text.is_empty() => {
                content.push(json!({"type": "text", "text": text}))
            }
            Part::Image { media_type, data } => content.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{media_type};base64,{data}")},
            })),
            _ => {}
        }
    }
    json!({"role": "user", "content": content})
}

fn assistant_message(message: &Message, quirks: &ChatQuirks) -> Value {
    let mut value = json!({
        "role": "assistant",
        "content": match text(message) {
            text if text.is_empty() => Value::Null,
            text => Value::String(text),
        },
    });
    let calls: Vec<Value> = message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall { id, name, args } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(args).unwrap_or_else(|_| "{}".into()),
                },
            })),
            _ => None,
        })
        .collect();
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    if let Some(field) = quirks.reasoning_replay {
        let reasoning = reasoning(message);
        if !reasoning.is_empty() {
            value[field] = Value::String(reasoning);
        }
    }
    value
}

fn text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reasoning(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every request header, in a stable order: transport, auth, then the session.
pub fn request_headers(
    quirks: &ChatQuirks,
    conversation_id: Option<&str>,
    auth: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "user-agent".into(),
            concat!("yoke/", env!("CARGO_PKG_VERSION")).into(),
        ),
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
    ];
    headers.extend(auth);
    if let (Some(name), Some(id)) = (
        quirks.session_header,
        conversation_id.filter(|id| !id.is_empty()),
    ) {
        headers.push((name.into(), id.chars().take(64).collect()));
    }
    headers
}

/// A provider that speaks only the chat dialect, built from a few quirks.
pub struct ChatProvider {
    id: &'static str,
    url: String,
    auth: Box<dyn Auth>,
    quirks: ChatQuirks,
}

impl ChatProvider {
    pub fn new(
        id: &'static str,
        url: impl Into<String>,
        auth: Box<dyn Auth>,
        quirks: ChatQuirks,
    ) -> Self {
        Self {
            id,
            url: url.into(),
            auth,
            quirks,
        }
    }
}

#[async_trait::async_trait]
impl Provider for ChatProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn body(&self, req: &Request) -> Value {
        body(req, &self.quirks)
    }

    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let auth = self.auth.headers().await?;
        let headers = request_headers(&self.quirks, req.conversation_id.as_deref(), auth);
        stream(self.id, &self.url, headers, req, &self.quirks).await
    }
}

/// POST the body and stream normalized events, starting with the exact body.
pub async fn stream(
    provider: &str,
    url: &str,
    headers: Vec<(String, String)>,
    req: &Request,
    quirks: &ChatQuirks,
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
    Ok(Box::pin(wire.chain(parse_response(response))))
}

struct Input {
    response: reqwest::Response,
    parser: StreamParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
}

fn parse_response(response: reqwest::Response) -> impl Stream<Item = Result<StreamEvent>> + Send {
    let input = Input {
        response,
        parser: StreamParser::new(),
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

#[derive(Debug, Default)]
struct Call {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

/// Stateful SSE-to-event parser; chunks may arrive at any byte boundary.
#[derive(Debug, Default)]
pub struct StreamParser {
    sse: sse::Parser,
    calls: Vec<Call>,
    stop: Option<String>,
    done: bool,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<StreamEvent>> {
        let mut events = Vec::new();
        for event in self.sse.feed(chunk) {
            self.data(&event.data, &mut events)?;
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        let mut events = Vec::new();
        for event in self.sse.finish() {
            self.data(&event.data, &mut events)?;
        }
        let stop = self.stop.clone().unwrap_or_else(|| {
            if self.calls.is_empty() {
                "end_turn".into()
            } else {
                "tool_calls".into()
            }
        });
        if !self.done {
            self.flush_calls(&mut events);
            events.push(StreamEvent::Done { stop });
            self.done = true;
        }
        Ok(events)
    }

    fn data(&mut self, data: &str, events: &mut Vec<StreamEvent>) -> Result<()> {
        if data.trim() == "[DONE]" {
            if !self.done {
                self.flush_calls(events);
                let stop = self.stop.clone().unwrap_or_else(|| "end_turn".into());
                events.push(StreamEvent::Done { stop });
                self.done = true;
            }
            return Ok(());
        }

        let chunk: Value =
            serde_json::from_str(data).context("chat: malformed stream chunk from provider")?;
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            events.push(StreamEvent::Usage(Usage {
                input_tokens: number(usage, "prompt_tokens", "input_tokens"),
                output_tokens: number(usage, "completion_tokens", "output_tokens"),
            }));
        }

        let mut stop = None;
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(StreamEvent::TextDelta { text: text.into() });
                }
            }
            for field in ["reasoning_content", "reasoning"] {
                if let Some(text) = delta.get(field).and_then(Value::as_str) {
                    if !text.is_empty() {
                        events.push(StreamEvent::ReasoningDelta { text: text.into() });
                    }
                    break;
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    self.call(call);
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                stop = Some(reason.to_string());
            }
        }

        if let Some(stop) = stop {
            self.stop = Some(stop.clone());
            if !self.done {
                self.flush_calls(events);
                events.push(StreamEvent::Done { stop });
                self.done = true;
            }
        }
        Ok(())
    }

    fn call(&mut self, delta: &Value) {
        let index = delta.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let position = match self.calls.iter().position(|call| call.index == index) {
            Some(position) => position,
            None => {
                self.calls.push(Call {
                    index,
                    id: format!("call_{index}"),
                    ..Call::default()
                });
                self.calls.len() - 1
            }
        };
        let call = &mut self.calls[position];
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                call.id = id.into();
            }
        }
        if let Some(function) = delta.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    call.name = name.into();
                }
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                call.arguments.push_str(arguments);
            }
        }
    }

    fn flush_calls(&mut self, events: &mut Vec<StreamEvent>) {
        self.calls.sort_by_key(|call| call.index);
        for call in self.calls.drain(..) {
            let args = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| Value::String(call.arguments.clone()))
            };
            events.push(StreamEvent::PartDone {
                part: Part::ToolCall {
                    id: call.id,
                    name: call.name,
                    args,
                },
            });
        }
    }
}

fn number(value: &Value, first: &str, second: &str) -> u64 {
    value
        .get(first)
        .or_else(|| value.get(second))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    fn request() -> Request {
        Request {
            model: "openai/gpt-oss-120b".into(),
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
                        Part::Text {
                            text: "Let me look.".into(),
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
            effort: None,
            conversation_id: None,
        }
    }

    #[test]
    fn body_is_exact_chat_completions_json() {
        assert_eq!(
            body(&request(), &ChatQuirks::default()),
            json!({
                "model": "openai/gpt-oss-120b",
                "messages": [
                    {"role": "system", "content": "Be brief."},
                    {"role": "user", "content": "What is in a.txt?"},
                    {
                        "role": "assistant",
                        "content": "Let me look.",
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
                        "description": "Read a file",
                        "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
                    },
                }],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
            })
        );
    }

    #[test]
    fn empty_instructions_tools_and_text_are_omitted_correctly() {
        let mut req = request();
        req.instructions = String::new();
        req.tools.clear();
        req.effort = Some("low".into());
        let body = body(
            &req,
            &ChatQuirks {
                reasoning_effort: true,
                ..ChatQuirks::default()
            },
        );
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["messages"][0]["role"], "user");

        let assistant = Message {
            role: Role::Assistant,
            parts: vec![Part::ToolCall {
                id: "c".into(),
                name: "bash".into(),
                args: json!({}),
            }],
        };
        let value = assistant_message(&assistant, &ChatQuirks::default());
        assert_eq!(value["content"], Value::Null);
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{}".to_string()
        );
    }

    fn events(chunks: &[&str]) -> Vec<StreamEvent> {
        let mut parser = StreamParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk.as_bytes()).unwrap());
        }
        events.extend(parser.finish().unwrap());
        events
    }

    #[test]
    fn text_stream_and_stop_reason() {
        let events = events(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta { text: "Hel".into() },
                StreamEvent::TextDelta { text: "lo".into() },
                StreamEvent::Done {
                    stop: "stop".into()
                },
            ]
        );
    }

    #[test]
    fn reasoning_deltas_from_either_field() {
        let events = events(&[
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\" more\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::ReasoningDelta {
                    text: "thinking".into()
                },
                StreamEvent::ReasoningDelta {
                    text: " more".into()
                },
                StreamEvent::Done {
                    stop: "end_turn".into()
                },
            ]
        );
    }

    #[test]
    fn tool_call_deltas_are_assembled_by_index() {
        let events = events(&[
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.txt\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]);
        assert_eq!(
            events,
            [
                StreamEvent::PartDone {
                    part: Part::ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        args: json!({"path": "a.txt"}),
                    }
                },
                StreamEvent::PartDone {
                    part: Part::ToolCall {
                        id: "call_2".into(),
                        name: "bash".into(),
                        args: json!({"command": "ls"}),
                    }
                },
                StreamEvent::Done {
                    stop: "tool_calls".into()
                },
            ]
        );
    }

    #[test]
    fn usage_chunk_and_bytes_split_everywhere() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut parser = StreamParser::new();
        let mut events = Vec::new();
        for byte in input.as_bytes() {
            events.extend(parser.feed(std::slice::from_ref(byte)).unwrap());
        }
        events.extend(parser.finish().unwrap());
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta { text: "hi".into() },
                StreamEvent::Done {
                    stop: "stop".into()
                },
                StreamEvent::Usage(Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                }),
            ]
        );
    }
}

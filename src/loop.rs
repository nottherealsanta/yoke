//! The agent loop; owns no UI and no storage.

use anyhow::Result;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::providers::Provider;
use crate::tools::{Registry, Tool, ToolResult, ToolStatus};
use crate::types::{Message, Part, Request, Role, StreamEvent};

pub const DEFAULT_STEP_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct Options {
    pub model: String,
    pub instructions: String,
    pub effort: Option<String>,
    pub conversation_id: Option<String>,
    pub step_limit: usize,
    /// Set by `--yes`: skip approval for every tool in this run.
    pub yes: bool,
}

impl Options {
    pub fn new(model: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            instructions: instructions.into(),
            effort: None,
            conversation_id: None,
            step_limit: DEFAULT_STEP_LIMIT,
            yes: false,
        }
    }
}

/// A pending approval. The caller answers by sending on `reply`; dropping the
/// sender counts as a denial.
#[derive(Debug)]
pub struct ApprovalRequest {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub reply: oneshot::Sender<bool>,
}

impl PartialEq for ApprovalRequest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name && self.args == other.args
    }
}

/// Everything the UI sees. The context itself is only the `Message`s.
#[derive(Debug, PartialEq)]
pub enum Event {
    StepStart {
        step: usize,
    },
    Stream(StreamEvent),
    MessageAdded(Message),
    ToolStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolOutput {
        id: String,
        chunk: String,
    },
    ToolEnd {
        id: String,
        result: ToolResult,
        status: ToolStatus,
    },
    ApprovalRequest(ApprovalRequest),
    Notice(String),
    TurnEnd {
        stop: String,
    },
}

#[derive(Clone)]
pub struct CancelToken {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self { tx, rx }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
        std::future::pending::<()>().await;
    }
}

fn emit(events: &mpsc::UnboundedSender<Event>, event: Event) {
    let _ = events.send(event);
}

/// Run one turn: model, tools, model, until the model stops calling tools.
/// The caller appends the new user message to `messages` before calling.
pub async fn run(
    provider: &dyn Provider,
    tools: &Registry,
    options: &Options,
    messages: &mut Vec<Message>,
    cancel: &CancelToken,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<()> {
    for step in 1..=options.step_limit {
        if cancel.is_cancelled() {
            emit(events, Event::Notice("interrupted".into()));
            emit(
                events,
                Event::TurnEnd {
                    stop: "interrupted".into(),
                },
            );
            return Ok(());
        }

        emit(events, Event::StepStart { step });
        let request = Request {
            model: options.model.clone(),
            instructions: options.instructions.clone(),
            messages: messages.clone(),
            tools: tools.schemas(),
            effort: options.effort.clone(),
            conversation_id: options.conversation_id.clone(),
        };

        let mut stream = match provider.stream(&request).await {
            Ok(stream) => stream,
            Err(error) => {
                emit(events, Event::Notice(error.to_string()));
                emit(
                    events,
                    Event::TurnEnd {
                        stop: "error".into(),
                    },
                );
                return Err(error);
            }
        };

        let mut parts = Vec::new();
        let mut stop = String::from("end_turn");
        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    emit(events, Event::Notice(error.to_string()));
                    emit(
                        events,
                        Event::TurnEnd {
                            stop: "error".into(),
                        },
                    );
                    return Err(error);
                }
            };
            emit(events, Event::Stream(event.clone()));
            match event {
                StreamEvent::TextDelta { text } => match parts.last_mut() {
                    Some(Part::Text { text: current }) => current.push_str(&text),
                    _ => parts.push(Part::Text { text }),
                },
                StreamEvent::ReasoningDelta { text } => match parts.last_mut() {
                    Some(Part::Reasoning { text: current, .. }) => current.push_str(&text),
                    _ => parts.push(Part::Reasoning { text, raw: None }),
                },
                StreamEvent::PartDone { part } => {
                    // The Responses dialect streams reasoning deltas and then
                    // the finished item, which is the only place its encrypted
                    // content exists. Merge it into the part the deltas built.
                    let merged = match (&part, parts.last_mut()) {
                        (
                            Part::Reasoning { text, raw },
                            Some(Part::Reasoning {
                                text: current,
                                raw: current_raw,
                            }),
                        ) => {
                            if !text.is_empty() {
                                *current = text.clone();
                            }
                            *current_raw = raw.clone();
                            true
                        }
                        _ => false,
                    };
                    if !merged {
                        parts.push(part);
                    }
                }
                StreamEvent::Done { stop: done } => stop = done,
                StreamEvent::Usage(_) | StreamEvent::Wire(_) => {}
            }
        }

        let assistant = Message {
            role: Role::Assistant,
            parts,
        };
        messages.push(assistant.clone());
        emit(events, Event::MessageAdded(assistant.clone()));

        let calls: Vec<(String, String, Value)> = assistant
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::ToolCall { id, name, args } => Some((id.clone(), name.clone(), args.clone())),
                _ => None,
            })
            .collect();

        if calls.is_empty() {
            emit(events, Event::TurnEnd { stop });
            return Ok(());
        }

        let mut results = Vec::with_capacity(calls.len());
        let mut interrupted = false;
        let mut calls = calls.into_iter();
        while let Some((id, name, args)) = calls.next() {
            emit(
                events,
                Event::ToolStart {
                    id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                },
            );
            let outcome = match tools.get(&name) {
                Some(tool) => run_tool(tool, args, &id, options.yes, cancel, events).await,
                None => Outcome {
                    result: ToolResult::error(format!("unknown tool: {name}")),
                    status: ToolStatus::Error,
                    interrupted: false,
                },
            };
            emit(
                events,
                Event::ToolEnd {
                    id: id.clone(),
                    result: outcome.result.clone(),
                    status: outcome.status,
                },
            );
            results.push(outcome.result.into_part(id));
            if outcome.interrupted {
                interrupted = true;
                for (id, _, _) in calls.by_ref() {
                    results.push(ToolResult::error("interrupted by user").into_part(id));
                }
            }
        }

        let tool_message = Message {
            role: Role::Tool,
            parts: results,
        };
        messages.push(tool_message.clone());
        emit(events, Event::MessageAdded(tool_message));

        if interrupted {
            emit(events, Event::Notice("interrupted".into()));
            emit(
                events,
                Event::TurnEnd {
                    stop: "interrupted".into(),
                },
            );
            return Ok(());
        }
    }

    emit(
        events,
        Event::Notice(format!("step limit reached ({})", options.step_limit)),
    );
    emit(
        events,
        Event::TurnEnd {
            stop: "step_limit".into(),
        },
    );
    Ok(())
}

struct Outcome {
    result: ToolResult,
    status: ToolStatus,
    interrupted: bool,
}

/// Ask first when needed, then run one tool, forwarding its live output.
/// Dropping the future on cancel aborts the tool.
async fn run_tool(
    tool: &dyn Tool,
    args: Value,
    id: &str,
    yes: bool,
    cancel: &CancelToken,
    events: &mpsc::UnboundedSender<Event>,
) -> Outcome {
    if tool.needs_approval() && !yes {
        let (reply, granted) = oneshot::channel();
        emit(
            events,
            Event::ApprovalRequest(ApprovalRequest {
                id: id.into(),
                name: tool.name().into(),
                args: args.clone(),
                reply,
            }),
        );
        let granted = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Outcome {
                    result: ToolResult::error("interrupted by user"),
                    status: ToolStatus::Interrupted,
                    interrupted: true,
                };
            }
            granted = granted => granted.unwrap_or(false),
        };
        if !granted {
            if cancel.is_cancelled() {
                return Outcome {
                    result: ToolResult::error("interrupted by user"),
                    status: ToolStatus::Interrupted,
                    interrupted: true,
                };
            }
            return Outcome {
                result: ToolResult::error("denied by user"),
                status: ToolStatus::Denied,
                interrupted: false,
            };
        }
    }

    let (out, mut chunks) = mpsc::unbounded_channel();
    let mut running = Box::pin(tool.run(args, out));
    let mut open = true;

    loop {
        tokio::select! {
            chunk = chunks.recv(), if open => match chunk {
                Some(chunk) => emit(events, Event::ToolOutput { id: id.into(), chunk }),
                None => open = false,
            },
            result = &mut running => {
                while let Ok(chunk) = chunks.try_recv() {
                    emit(events, Event::ToolOutput { id: id.into(), chunk });
                }
                let status = if result.is_error {
                    ToolStatus::Error
                } else {
                    ToolStatus::Ok
                };
                return Outcome {
                    result,
                    status,
                    interrupted: false,
                };
            }
            _ = cancel.cancelled() => {
                return Outcome {
                    result: ToolResult::error("interrupted by user"),
                    status: ToolStatus::Interrupted,
                    interrupted: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::mpsc::UnboundedReceiver;

    use crate::providers::mock::MockProvider;
    use crate::tools::Sink;

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }

        fn schema(&self) -> Value {
            json!({"name": "echo", "description": "echo the arguments"})
        }

        async fn run(&self, args: Value, out: Sink) -> ToolResult {
            let output = format!("echo {args}");
            let _ = out.send(output.clone());
            ToolResult::ok(output)
        }
    }

    struct Slow;

    #[async_trait]
    impl Tool for Slow {
        fn name(&self) -> &str {
            "slow"
        }

        fn schema(&self) -> Value {
            json!({"name": "slow"})
        }

        async fn run(&self, _args: Value, _out: Sink) -> ToolResult {
            tokio::time::sleep(Duration::from_secs(30)).await;
            ToolResult::ok("too late")
        }
    }

    struct Guarded;

    #[async_trait]
    impl Tool for Guarded {
        fn name(&self) -> &str {
            "guarded"
        }

        fn needs_approval(&self) -> bool {
            true
        }

        fn schema(&self) -> Value {
            json!({"name": "guarded"})
        }

        async fn run(&self, _args: Value, _out: Sink) -> ToolResult {
            ToolResult::ok("ran")
        }
    }

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn opts(step_limit: usize) -> Options {
        Options {
            step_limit,
            ..Options::new("mock-model", "be brief")
        }
    }

    fn call_turn(id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta {
                text: "calling".into(),
            },
            StreamEvent::PartDone {
                part: Part::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    args,
                },
            },
            StreamEvent::Done {
                stop: "tool_calls".into(),
            },
        ]
    }

    fn drain(rx: &mut UnboundedReceiver<Event>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn labels(events: &[Event]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event {
                Event::StepStart { .. } => "step",
                Event::Stream(StreamEvent::TextDelta { .. }) => "text",
                Event::Stream(StreamEvent::ReasoningDelta { .. }) => "reasoning",
                Event::Stream(StreamEvent::PartDone { .. }) => "part",
                Event::Stream(StreamEvent::Usage(_)) => "usage",
                Event::Stream(StreamEvent::Done { .. }) => "done",
                Event::Stream(StreamEvent::Wire(_)) => "wire",
                Event::MessageAdded(message) => match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                },
                Event::ToolStart { .. } => "tool_start",
                Event::ToolOutput { .. } => "tool_output",
                Event::ToolEnd { .. } => "tool_end",
                Event::ApprovalRequest(_) => "approval",
                Event::Notice(_) => "notice",
                Event::TurnEnd { .. } => "turn_end",
            })
            .collect()
    }

    async fn drive(
        provider: &dyn Provider,
        tools: &Registry,
        options: &Options,
        messages: &mut Vec<Message>,
        cancel: &CancelToken,
    ) -> Vec<Event> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(provider, tools, options, messages, cancel, &tx)
            .await
            .unwrap();
        drain(&mut rx)
    }

    /// Drive a turn, answering every approval request with `answer`.
    async fn drive_with_approval(
        provider: &dyn Provider,
        tools: &Registry,
        options: &Options,
        messages: &mut Vec<Message>,
        answer: bool,
    ) -> Vec<Event> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancelToken::new();
        let mut running = Box::pin(run(provider, tools, options, messages, &cancel, &tx));
        let mut events = Vec::new();
        loop {
            tokio::select! {
                result = &mut running => {
                    result.unwrap();
                    events.extend(drain(&mut rx));
                    return events;
                }
                event = rx.recv() => match event {
                    Some(Event::ApprovalRequest(request)) => {
                        let (reply, _) = oneshot::channel();
                        events.push(Event::ApprovalRequest(ApprovalRequest {
                            id: request.id.clone(),
                            name: request.name.clone(),
                            args: request.args.clone(),
                            reply,
                        }));
                        let _ = request.reply.send(answer);
                    }
                    Some(event) => events.push(event),
                    None => {}
                },
            }
        }
    }

    #[tokio::test]
    async fn text_only_turn() {
        let provider = MockProvider::new(vec![vec![
            StreamEvent::TextDelta { text: "hel".into() },
            StreamEvent::TextDelta { text: "lo".into() },
            StreamEvent::Done {
                stop: "end_turn".into(),
            },
        ]]);
        let mut messages = vec![user("hi")];

        let events = drive(
            &provider,
            &Registry::new(),
            &opts(DEFAULT_STEP_LIMIT),
            &mut messages,
            &CancelToken::new(),
        )
        .await;

        assert_eq!(
            labels(&events),
            ["step", "text", "text", "done", "assistant", "turn_end"]
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(
            messages[1].parts,
            vec![Part::Text {
                text: "hello".into()
            }]
        );
    }

    #[tokio::test]
    async fn reasoning_deltas_merge_with_the_finished_item() {
        let raw = json!({"type": "reasoning", "encrypted_content": "gAAA"});
        let provider = MockProvider::new(vec![vec![
            StreamEvent::ReasoningDelta {
                text: "thin".into(),
            },
            StreamEvent::ReasoningDelta {
                text: "king".into(),
            },
            StreamEvent::PartDone {
                part: Part::Reasoning {
                    text: "thinking".into(),
                    raw: Some(raw.clone()),
                },
            },
            StreamEvent::Done {
                stop: "end_turn".into(),
            },
        ]]);
        let mut messages = vec![user("hi")];

        let events = drive(
            &provider,
            &Registry::new(),
            &opts(DEFAULT_STEP_LIMIT),
            &mut messages,
            &CancelToken::new(),
        )
        .await;

        assert_eq!(
            labels(&events),
            [
                "step",
                "reasoning",
                "reasoning",
                "part",
                "done",
                "assistant",
                "turn_end"
            ]
        );
        assert_eq!(
            messages[1].parts,
            vec![Part::Reasoning {
                text: "thinking".into(),
                raw: Some(raw),
            }]
        );
    }

    #[tokio::test]
    async fn three_step_tool_turn() {
        let provider = MockProvider::new(vec![
            call_turn("c1", "echo", json!({"n": 1})),
            call_turn("c2", "echo", json!({"n": 2})),
            vec![
                StreamEvent::TextDelta {
                    text: "done".into(),
                },
                StreamEvent::Done {
                    stop: "end_turn".into(),
                },
            ],
        ]);
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));
        let mut messages = vec![user("go")];

        let events = drive(
            &provider,
            &registry,
            &opts(DEFAULT_STEP_LIMIT),
            &mut messages,
            &CancelToken::new(),
        )
        .await;

        let expected = [
            "step",
            "text",
            "part",
            "done",
            "assistant",
            "tool_start",
            "tool_output",
            "tool_end",
            "tool",
            "step",
            "text",
            "part",
            "done",
            "assistant",
            "tool_start",
            "tool_output",
            "tool_end",
            "tool",
            "step",
            "text",
            "done",
            "assistant",
            "turn_end",
        ];
        assert_eq!(labels(&events), expected);

        let roles: Vec<Role> = messages.iter().map(|message| message.role).collect();
        assert_eq!(
            roles,
            [
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
            ]
        );
        assert_eq!(
            messages[2].parts,
            vec![Part::ToolResult {
                id: "c1".into(),
                output: r#"echo {"n":1}"#.into(),
                is_error: false,
            }]
        );
        assert_eq!(
            messages[4].parts,
            vec![Part::ToolResult {
                id: "c2".into(),
                output: r#"echo {"n":2}"#.into(),
                is_error: false,
            }]
        );
        assert_eq!(
            messages[5].parts,
            vec![Part::Text {
                text: "done".into()
            }]
        );
        assert_eq!(
            events.last(),
            Some(&Event::TurnEnd {
                stop: "end_turn".into()
            })
        );
    }

    #[tokio::test]
    async fn step_limit_stops_a_runaway_turn() {
        let provider = MockProvider::new(
            (0..5)
                .map(|n| call_turn(&format!("c{n}"), "echo", json!({"n": n})))
                .collect(),
        );
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));
        let mut messages = vec![user("go")];

        let events = drive(
            &provider,
            &registry,
            &opts(2),
            &mut messages,
            &CancelToken::new(),
        )
        .await;

        assert_eq!(labels(&events).iter().filter(|l| **l == "step").count(), 2);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Notice(notice) if notice.contains("step limit")
        )));
        assert_eq!(
            events.last(),
            Some(&Event::TurnEnd {
                stop: "step_limit".into()
            })
        );
        assert_eq!(messages.len(), 5);
    }

    #[tokio::test]
    async fn interrupt_mid_tool_keeps_the_result_in_context() {
        let provider = MockProvider::new(vec![call_turn("c1", "slow", json!({}))]);
        let mut registry = Registry::new();
        registry.register(Box::new(Slow));
        let mut messages = vec![user("go")];
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel();
        });

        let events = drive(&provider, &registry, &opts(10), &mut messages, &cancel).await;

        assert_eq!(
            labels(&events),
            [
                "step",
                "text",
                "part",
                "done",
                "assistant",
                "tool_start",
                "tool_end",
                "tool",
                "notice",
                "turn_end",
            ]
        );
        assert_eq!(
            events.last(),
            Some(&Event::TurnEnd {
                stop: "interrupted".into()
            })
        );
        assert_eq!(messages.len(), 3);
        match &messages[2].parts[0] {
            Part::ToolResult {
                id,
                output,
                is_error,
            } => {
                assert_eq!(id, "c1");
                assert!(output.contains("interrupted"));
                assert!(is_error);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_granted_runs_the_tool() {
        let provider = MockProvider::new(vec![
            call_turn("c1", "guarded", json!({"n": 1})),
            vec![
                StreamEvent::TextDelta {
                    text: "done".into(),
                },
                StreamEvent::Done {
                    stop: "end_turn".into(),
                },
            ],
        ]);
        let mut registry = Registry::new();
        registry.register(Box::new(Guarded));
        let mut messages = vec![user("go")];

        let events =
            drive_with_approval(&provider, &registry, &opts(10), &mut messages, true).await;

        assert!(labels(&events).contains(&"approval"));
        assert_eq!(
            events
                .iter()
                .find(|event| matches!(event, Event::ToolEnd { .. })),
            Some(&Event::ToolEnd {
                id: "c1".into(),
                result: ToolResult::ok("ran"),
                status: ToolStatus::Ok,
            })
        );
        assert_eq!(
            messages[2].parts,
            vec![Part::ToolResult {
                id: "c1".into(),
                output: "ran".into(),
                is_error: false,
            }]
        );
    }

    #[tokio::test]
    async fn approval_denied_is_a_tool_result() {
        let provider = MockProvider::new(vec![
            call_turn("c1", "guarded", json!({})),
            vec![
                StreamEvent::TextDelta { text: "ok".into() },
                StreamEvent::Done {
                    stop: "end_turn".into(),
                },
            ],
        ]);
        let mut registry = Registry::new();
        registry.register(Box::new(Guarded));
        let mut messages = vec![user("go")];

        let events =
            drive_with_approval(&provider, &registry, &opts(10), &mut messages, false).await;

        assert_eq!(
            events
                .iter()
                .find(|event| matches!(event, Event::ToolEnd { .. })),
            Some(&Event::ToolEnd {
                id: "c1".into(),
                result: ToolResult::error("denied by user"),
                status: ToolStatus::Denied,
            })
        );
        assert_eq!(
            messages[2].parts,
            vec![Part::ToolResult {
                id: "c1".into(),
                output: "denied by user".into(),
                is_error: true,
            }]
        );
        assert_eq!(
            messages.last().unwrap().parts,
            vec![Part::Text { text: "ok".into() }]
        );
    }

    #[tokio::test]
    async fn yes_skips_approval() {
        let provider = MockProvider::new(vec![call_turn("c1", "guarded", json!({}))]);
        let mut registry = Registry::new();
        registry.register(Box::new(Guarded));
        let mut messages = vec![user("go")];
        let options = Options {
            yes: true,
            ..opts(10)
        };

        let events = drive(
            &provider,
            &registry,
            &options,
            &mut messages,
            &CancelToken::new(),
        )
        .await;

        assert!(!labels(&events).contains(&"approval"));
        assert_eq!(
            messages[2].parts,
            vec![Part::ToolResult {
                id: "c1".into(),
                output: "ran".into(),
                is_error: false,
            }]
        );
    }

    #[tokio::test]
    async fn cancel_while_waiting_for_approval_interrupts() {
        let provider = MockProvider::new(vec![call_turn("c1", "guarded", json!({}))]);
        let mut registry = Registry::new();
        registry.register(Box::new(Guarded));
        let mut messages = vec![user("go")];
        let cancel = CancelToken::new();
        let options = opts(10);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut running = Box::pin(run(
            &provider,
            &registry,
            &options,
            &mut messages,
            &cancel,
            &tx,
        ));
        let mut events = Vec::new();
        loop {
            tokio::select! {
                result = &mut running => {
                    result.unwrap();
                    events.extend(drain(&mut rx));
                    break;
                }
                event = rx.recv() => match event {
                    Some(Event::ApprovalRequest(_)) => cancel.cancel(),
                    Some(event) => events.push(event),
                    None => {}
                },
            }
        }
        drop(running);

        assert_eq!(
            events
                .iter()
                .find(|event| matches!(event, Event::ToolEnd { .. })),
            Some(&Event::ToolEnd {
                id: "c1".into(),
                result: ToolResult::error("interrupted by user"),
                status: ToolStatus::Interrupted,
            })
        );
        assert_eq!(
            events.last(),
            Some(&Event::TurnEnd {
                stop: "interrupted".into()
            })
        );
        assert_eq!(
            messages[2].parts,
            vec![Part::ToolResult {
                id: "c1".into(),
                output: "interrupted by user".into(),
                is_error: true,
            }]
        );
    }
}

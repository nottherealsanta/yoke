//! The timeline: the conversation and its context, built only from loop events.
//!
//! Every view mode renders this same `Timeline`; none of them can see anything
//! that is not in the model's context. The header entries (system prompt,
//! memory, tool schemas) are real entries so mode 3 can show them verbatim.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::context::Sections;
use crate::r#loop::Event;
use crate::tools::ToolStatus;
use crate::types::{Message, Part, Role, StreamEvent, Usage};

/// The three header entries always come first, in this order.
const HEADER_ENTRIES: usize = 3;

#[derive(Debug, Clone)]
pub struct Entry {
    /// The loop step that produced this entry, when it came from a step.
    #[allow(dead_code)] // mode 3 groups entries by step
    pub step: Option<usize>,
    pub kind: EntryKind,
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    SystemPrompt(String),
    Memory {
        #[allow(dead_code)] // mode 3 prints the path next to the text
        path: PathBuf,
        text: String,
    },
    ToolSchemas(#[allow(dead_code)] Vec<Value>),
    /// A message exactly as it sits in the context.
    Message(Message),
    /// One tool call with its live output, final status and wall time.
    Tool(ToolRun),
    Notice {
        text: String,
        level: NoticeLevel,
    },
    /// One step's real usage report.
    Usage {
        #[allow(dead_code)] // mode 3
        usage: Usage,
    },
    /// One step's exact request body (`StreamEvent::Wire`).
    Wire {
        #[allow(dead_code)] // mode 3
        body: Value,
    },
    /// The model output that is arriving right now, before `MessageAdded`.
    Streaming {
        text: String,
        #[allow(dead_code)] // mode 2 shows reasoning
        reasoning: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    #[allow(dead_code)] // modes 2 and 3 can tell a failure from a note
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Ok,
    Error,
    Denied,
    Interrupted,
}

impl RunStatus {
    fn from_tool(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Ok => Self::Ok,
            ToolStatus::Error => Self::Error,
            ToolStatus::Denied => Self::Denied,
            ToolStatus::Interrupted => Self::Interrupted,
        }
    }

    #[allow(dead_code)] // mode 2 prints the status
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolRun {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub status: RunStatus,
    /// Live chunks while running, then the tool's final full result text.
    pub output: String,
    /// Wall time from `ToolStart` to `ToolEnd`; includes any approval wait.
    pub duration: Option<Duration>,
    /// Set while the tool runs so `ToolEnd` can compute the duration.
    pub started: Option<Instant>,
}

impl ToolRun {
    /// Added and removed lines for `edit`/`write`; `None` for other tools.
    pub fn diff_stat(&self) -> Option<(usize, usize)> {
        match self.name.as_str() {
            "write" => {
                let added = self.args.get("content").and_then(Value::as_str);
                Some((added.map_or(0, line_count), 0))
            }
            "edit" => {
                let added = self
                    .args
                    .get("new_string")
                    .and_then(Value::as_str)
                    .map_or(0, line_count);
                let removed = self
                    .args
                    .get("old_string")
                    .and_then(Value::as_str)
                    .map_or(0, line_count);
                Some((added, removed))
            }
            _ => None,
        }
    }

    /// The JSON arguments on one line, cut to `max` characters.
    #[allow(dead_code)] // mode 2
    pub fn compact_args(&self, max: usize) -> String {
        let text = serde_json::to_string(&self.args).unwrap_or_else(|_| "{}".into());
        truncate(&text, max)
    }
}

fn line_count(text: &str) -> usize {
    text.lines().count()
}

#[allow(dead_code)] // used by `compact_args`, which modes 2 and 3 call
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Text parts of a message joined by newlines; reasoning is kept separately.
pub fn message_text(message: &Message) -> String {
    let mut text = String::new();
    for part in &message.parts {
        if let Part::Text { text: chunk } = part {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(chunk);
        }
    }
    text
}

#[allow(dead_code)] // mode 2 shows reasoning dimmed
pub fn message_reasoning(message: &Message) -> String {
    let mut text = String::new();
    for part in &message.parts {
        if let Part::Reasoning { text: chunk, .. } = part {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(chunk);
        }
    }
    text
}

#[derive(Debug, Clone)]
pub struct Timeline {
    /// The full system prompt (memory included, exactly as the model gets it).
    pub system: String,
    pub memory: String,
    pub tools: Vec<Value>,
    pub entries: Vec<Entry>,
    /// The last real usage report, for the status line and context total.
    pub last_usage: Option<Usage>,
    /// The current (or last) loop step number.
    pub step: usize,
    /// Index into `entries` of the live streaming buffer, if any.
    streaming: Option<usize>,
}

impl Timeline {
    /// Start with the three context header entries.
    pub fn new(
        system: impl Into<String>,
        memory: impl Into<String>,
        memory_path: PathBuf,
        tools: Vec<Value>,
    ) -> Self {
        let system = system.into();
        let memory = memory.into();
        let mut timeline = Self {
            system: system.clone(),
            memory: memory.clone(),
            tools: tools.clone(),
            entries: Vec::new(),
            last_usage: None,
            step: 0,
            streaming: None,
        };
        timeline.entries.push(Entry {
            step: None,
            kind: EntryKind::SystemPrompt(system),
        });
        timeline.entries.push(Entry {
            step: None,
            kind: EntryKind::Memory {
                path: memory_path,
                text: memory,
            },
        });
        timeline.entries.push(Entry {
            step: None,
            kind: EntryKind::ToolSchemas(tools),
        });
        timeline
    }

    /// A resumed session: header entries, then every loaded message. Tool runs
    /// are rebuilt from the assistant's calls and the tool message's results.
    pub fn from_messages(
        system: impl Into<String>,
        memory: impl Into<String>,
        memory_path: PathBuf,
        tools: Vec<Value>,
        messages: Vec<Message>,
    ) -> Self {
        let mut timeline = Self::new(system, memory, memory_path, tools);
        for message in messages {
            match message.role {
                Role::Assistant => {
                    let calls: Vec<(String, String, Value)> = message
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::ToolCall { id, name, args } => {
                                Some((id.clone(), name.clone(), args.clone()))
                            }
                            _ => None,
                        })
                        .collect();
                    timeline.push_message(message);
                    for (id, name, args) in calls {
                        timeline.entries.push(Entry {
                            step: None,
                            kind: EntryKind::Tool(ToolRun {
                                id,
                                name,
                                args,
                                status: RunStatus::Running,
                                output: String::new(),
                                duration: None,
                                started: None,
                            }),
                        });
                    }
                }
                Role::Tool => {
                    for part in &message.parts {
                        if let Part::ToolResult {
                            id,
                            output,
                            is_error,
                        } = part
                        {
                            if let Some(run) = timeline.tool_mut(id) {
                                run.output = output.clone();
                                run.status = if *is_error {
                                    RunStatus::Error
                                } else {
                                    RunStatus::Ok
                                };
                            }
                        }
                    }
                    timeline.push_message(message);
                }
                Role::User => timeline.push_message(message),
            }
        }
        timeline
    }

    /// Apply one loop event. Approval requests are the app's business.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::StepStart { step } => self.step = step,
            Event::Stream(StreamEvent::TextDelta { text }) => {
                self.streaming_mut().0.push_str(&text);
            }
            Event::Stream(StreamEvent::ReasoningDelta { text }) => {
                self.streaming_mut().1.push_str(&text);
            }
            Event::Stream(StreamEvent::PartDone { .. }) => {}
            Event::Stream(StreamEvent::Usage(usage)) => {
                self.last_usage = Some(usage);
                self.entries.push(Entry {
                    step: Some(self.step),
                    kind: EntryKind::Usage { usage },
                });
            }
            Event::Stream(StreamEvent::Wire(body)) => {
                self.entries.push(Entry {
                    step: Some(self.step),
                    kind: EntryKind::Wire { body },
                });
            }
            Event::Stream(StreamEvent::Done { .. }) => {}
            Event::MessageAdded(message) => {
                self.drop_streaming();
                self.push_message(message);
            }
            Event::ToolStart { id, name, args } => {
                self.entries.push(Entry {
                    step: Some(self.step),
                    kind: EntryKind::Tool(ToolRun {
                        id,
                        name,
                        args,
                        status: RunStatus::Running,
                        output: String::new(),
                        duration: None,
                        started: Some(Instant::now()),
                    }),
                });
            }
            Event::ToolOutput { id, chunk } => {
                if let Some(run) = self.tool_mut(&id) {
                    run.output.push_str(&chunk);
                }
            }
            Event::ToolEnd { id, result, status } => {
                if let Some(run) = self.tool_mut(&id) {
                    run.output = result.output;
                    run.status = RunStatus::from_tool(status);
                    run.duration = run.started.map(|started| started.elapsed());
                }
            }
            Event::ApprovalRequest(_) => {}
            Event::Notice(text) => self.notice(text),
            Event::TurnEnd { .. } => self.drop_streaming(),
        }
    }

    /// Append a message to the timeline and to the rendered context.
    pub fn push_message(&mut self, message: Message) {
        self.entries.push(Entry {
            step: Some(self.step),
            kind: EntryKind::Message(message),
        });
    }

    /// A visible, non-context notice in the timeline.
    pub fn notice(&mut self, text: impl Into<String>) {
        self.entries.push(Entry {
            step: None,
            kind: EntryKind::Notice {
                text: text.into(),
                level: NoticeLevel::Info,
            },
        });
    }

    /// `/clear`: keep the context header, drop the conversation.
    pub fn clear_messages(&mut self) {
        self.entries.truncate(HEADER_ENTRIES);
        self.last_usage = None;
        self.step = 0;
        self.streaming = None;
    }

    /// `/memory`: replace the stored context text in place.
    pub fn set_context(&mut self, system: String, memory: String) {
        self.system = system;
        self.memory = memory.clone();
        for entry in &mut self.entries {
            match &mut entry.kind {
                EntryKind::SystemPrompt(text) => *text = self.system.clone(),
                EntryKind::Memory { text, .. } => *text = memory.clone(),
                _ => {}
            }
        }
    }

    /// Token estimates for each context section, including the messages.
    pub fn sections(&self) -> Sections {
        let messages: Vec<Message> = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Message(message) => Some(message.clone()),
                _ => None,
            })
            .collect();
        Sections::new(&self.system, &self.memory, &self.tools, &messages)
    }

    /// Tokens used: real usage when the provider sent it, else the estimate.
    pub fn used(&self) -> (usize, bool) {
        let sections = self.sections();
        (sections.used(self.last_usage), self.last_usage.is_some())
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolRun> {
        self.entries
            .iter_mut()
            .rev()
            .find_map(|entry| match &mut entry.kind {
                EntryKind::Tool(run) if run.id == id => Some(run),
                _ => None,
            })
    }

    fn streaming_mut(&mut self) -> (&mut String, &mut String) {
        let index = match self.streaming {
            Some(index) => index,
            None => {
                self.entries.push(Entry {
                    step: Some(self.step),
                    kind: EntryKind::Streaming {
                        text: String::new(),
                        reasoning: String::new(),
                    },
                });
                self.entries.len() - 1
            }
        };
        self.streaming = Some(index);
        match &mut self.entries[index].kind {
            EntryKind::Streaming { text, reasoning } => (text, reasoning),
            _ => unreachable!("streaming index points at a streaming entry"),
        }
    }

    fn drop_streaming(&mut self) {
        if let Some(index) = self.streaming.take() {
            if index < self.entries.len() {
                self.entries.remove(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    use crate::providers::mock::MockProvider;
    use crate::r#loop::{run, CancelToken, Options};
    use crate::tools::{Registry, Sink, Tool, ToolResult};

    struct Echo;

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }

        fn schema(&self) -> Value {
            json!({"name": "echo", "description": "echo"})
        }

        async fn run(&self, args: Value, out: Sink) -> ToolResult {
            let output = format!("echo {args}");
            let _ = out.send(output.clone());
            ToolResult::ok(output)
        }
    }

    fn header() -> Timeline {
        Timeline::new(
            "You are Yoke.",
            "prefers small diffs",
            PathBuf::from("/home/u/.yoke/projects/-w/memory.md"),
            vec![json!({"name": "read"})],
        )
    }

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    #[tokio::test]
    async fn timeline_is_built_from_loop_events() {
        let provider = MockProvider::new(vec![
            vec![
                StreamEvent::TextDelta {
                    text: "checking".into(),
                },
                StreamEvent::PartDone {
                    part: Part::ToolCall {
                        id: "c1".into(),
                        name: "echo".into(),
                        args: json!({"n": 1}),
                    },
                },
                StreamEvent::Done {
                    stop: "tool_calls".into(),
                },
            ],
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
        let mut timeline = header();
        timeline.push_message(user("go"));

        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            &provider,
            &registry,
            &Options::new("mock", "be brief"),
            &mut messages,
            &CancelToken::new(),
            &tx,
        )
        .await
        .unwrap();
        drop(tx);
        while let Some(event) = rx.recv().await {
            timeline.apply(event);
        }

        let message_roles: Vec<Role> = timeline
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Message(message) => Some(message.role),
                _ => None,
            })
            .collect();
        assert_eq!(
            message_roles,
            [Role::User, Role::Assistant, Role::Tool, Role::Assistant]
        );

        let runs: Vec<&ToolRun> = timeline
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Tool(run) => Some(run),
                _ => None,
            })
            .collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "echo");
        assert_eq!(runs[0].status, RunStatus::Ok);
        assert_eq!(runs[0].output, r#"echo {"n":1}"#);
        assert!(runs[0].duration.is_some());

        assert!(timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Message(Message { parts, .. })
                if parts.iter().any(|part| matches!(part, Part::Text { text } if text == "checking"))
        )));
        assert!(timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Message(Message { parts, .. })
                if parts.iter().any(|part| matches!(part, Part::Text { text } if text == "done"))
        )));
        assert!(!timeline
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, EntryKind::Streaming { .. })));
    }

    #[test]
    fn resumed_messages_rebuild_tool_runs() {
        let messages = vec![
            user("go"),
            Message {
                role: Role::Assistant,
                parts: vec![
                    Part::Text {
                        text: "checking".into(),
                    },
                    Part::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        args: json!({"path": "a.rs"}),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: "c1".into(),
                    output: "file body".into(),
                    is_error: false,
                }],
            },
        ];
        let timeline = Timeline::from_messages(
            "system",
            "",
            PathBuf::from("memory.md"),
            vec![json!({"name": "read"})],
            messages,
        );

        let run = timeline
            .entries
            .iter()
            .find_map(|entry| match &entry.kind {
                EntryKind::Tool(run) => Some(run),
                _ => None,
            })
            .unwrap();
        assert_eq!(run.status, RunStatus::Ok);
        assert_eq!(run.output, "file body");
        assert_eq!(run.duration, None);

        let assistant = timeline
            .entries
            .iter()
            .position(|entry| {
                matches!(
                    &entry.kind,
                    EntryKind::Message(message) if message.role == Role::Assistant
                )
            })
            .unwrap();
        let tool = timeline
            .entries
            .iter()
            .position(|entry| matches!(entry.kind, EntryKind::Tool(_)))
            .unwrap();
        assert!(assistant < tool);
    }

    #[test]
    fn diff_stats_come_from_edit_and_write_arguments() {
        let edit = ToolRun {
            id: "c1".into(),
            name: "edit".into(),
            args: json!({"path": "a", "old_string": "one\ntwo", "new_string": "one\ntwo\nthree"}),
            status: RunStatus::Ok,
            output: String::new(),
            duration: None,
            started: None,
        };
        assert_eq!(edit.diff_stat(), Some((3, 2)));

        let write = ToolRun {
            name: "write".into(),
            args: json!({"path": "a", "content": "a\nb\n"}),
            ..edit.clone()
        };
        assert_eq!(write.diff_stat(), Some((2, 0)));

        let read = ToolRun {
            name: "read".into(),
            args: json!({"path": "a"}),
            ..edit
        };
        assert_eq!(read.diff_stat(), None);
    }

    #[test]
    fn clear_keeps_the_header_and_set_context_updates_it() {
        let mut timeline = header();
        timeline.push_message(user("hi"));
        timeline.clear_messages();
        assert_eq!(timeline.entries.len(), HEADER_ENTRIES);

        timeline.set_context("new system".into(), "new memory".into());
        match &timeline.entries[0].kind {
            EntryKind::SystemPrompt(text) => assert_eq!(text, "new system"),
            other => panic!("expected the system prompt, got {other:?}"),
        }
        match &timeline.entries[1].kind {
            EntryKind::Memory { text, .. } => assert_eq!(text, "new memory"),
            other => panic!("expected memory, got {other:?}"),
        }
        assert_eq!(timeline.sections().memory, 3);
    }
}

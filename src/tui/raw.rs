//! Mode 3: Raw.
//!
//! Every line the model receives, in order and verbatim: the full system
//! prompt, the memory text and its path, every tool schema, every message part,
//! per-step usage and the exact request body of each step. Request bodies are
//! shown pretty-printed with header/token values replaced by `[redacted]`.

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use super::timeline::{EntryKind, NoticeLevel, RunStatus, Timeline};
use crate::types::{Message, Part, Role};

/// The whole context as one flat list of lines, wrapped to `width`.
pub fn lines(timeline: &Timeline, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();

    let mut messages = 0;
    for entry in &timeline.entries {
        match &entry.kind {
            EntryKind::SystemPrompt(system) => {
                section(&mut out, "system", width);
                push(&mut out, system, width, Style::default());
            }
            EntryKind::Memory { path, text } => {
                section(&mut out, &format!("memory · {}", path.display()), width);
                if text.is_empty() {
                    push(&mut out, "(empty)", width, dim());
                } else {
                    push(&mut out, text, width, Style::default());
                }
            }
            EntryKind::ToolSchemas(tools) => {
                section(&mut out, &format!("tool schemas · {}", tools.len()), width);
                if tools.is_empty() {
                    push(&mut out, "(none)", width, dim());
                }
                for (index, schema) in tools.iter().enumerate() {
                    let name = schema.get("name").and_then(Value::as_str).unwrap_or("?");
                    push(
                        &mut out,
                        &format!("tool {} · {name}", index + 1),
                        width,
                        dim(),
                    );
                    push_json(&mut out, schema, width);
                }
            }
            EntryKind::Message(message) => {
                messages += 1;
                message_lines(&mut out, messages, entry.step, message, width);
            }
            EntryKind::Tool(run) => {
                let mut label = format!("tool run · {} · {}", run.name, run.status.label());
                if let Some(duration) = run.duration {
                    label.push_str(&format!(" · {}", duration_label(duration)));
                }
                if !run.id.is_empty() {
                    label.push_str(&format!(" · id {}", run.id));
                }
                section(&mut out, &label, width);
                push(&mut out, "args", width, dim());
                push_json(&mut out, &run.args, width);
                out.push(Line::default());
                push(&mut out, "output", width, dim());
                if run.output.is_empty() {
                    let placeholder = if run.status == RunStatus::Running {
                        "(no output yet)"
                    } else {
                        "(empty)"
                    };
                    push(&mut out, placeholder, width, dim());
                } else {
                    push(&mut out, &run.output, width, Style::default());
                }
            }
            EntryKind::Notice { text, level } => {
                let label = match level {
                    NoticeLevel::Info => "notice · not sent",
                    NoticeLevel::Error => "error · not sent",
                };
                let style = match level {
                    NoticeLevel::Info => dim(),
                    NoticeLevel::Error => Style::default().fg(Color::Red),
                };
                section(&mut out, label, width);
                push(&mut out, text, width, style);
            }
            EntryKind::Usage { usage } => {
                section(
                    &mut out,
                    &format!("{}usage", step_prefix(entry.step)),
                    width,
                );
                push(
                    &mut out,
                    &format!("input tokens: {}", usage.input_tokens),
                    width,
                    Style::default(),
                );
                push(
                    &mut out,
                    &format!("output tokens: {}", usage.output_tokens),
                    width,
                    Style::default(),
                );
                push(
                    &mut out,
                    &format!(
                        "total: {}",
                        usage.input_tokens.saturating_add(usage.output_tokens)
                    ),
                    width,
                    Style::default(),
                );
            }
            EntryKind::Wire { body } => {
                let (body, redacted) = redact(body);
                let mut label = format!("{}request body", step_prefix(entry.step));
                if redacted {
                    label.push_str(" · secrets redacted");
                }
                section(&mut out, &label, width);
                push_json(&mut out, &body, width);
            }
            EntryKind::Streaming { text, reasoning } => {
                let label = format!("{}live", step_prefix(entry.step));
                if text.is_empty() && reasoning.is_empty() {
                    section(&mut out, &label, width);
                    push(&mut out, "(waiting for the model)", width, dim());
                    continue;
                }
                section(&mut out, &label, width);
                if !reasoning.is_empty() {
                    push(&mut out, "reasoning", width, dim());
                    push(&mut out, reasoning, width, Style::default());
                }
                if !text.is_empty() {
                    if !reasoning.is_empty() {
                        out.push(Line::default());
                    }
                    push(&mut out, "text", width, dim());
                    push(&mut out, text, width, Style::default());
                }
            }
        }
    }

    out
}

fn message_lines(
    out: &mut Vec<Line<'static>>,
    number: usize,
    step: Option<usize>,
    message: &Message,
    width: usize,
) {
    section(
        out,
        &format!(
            "message {number} · {}{}",
            role_label(message.role),
            step_suffix(step)
        ),
        width,
    );
    if message.parts.is_empty() {
        push(out, "(no parts)", width, dim());
        return;
    }
    for (index, part) in message.parts.iter().enumerate() {
        if index > 0 {
            out.push(Line::default());
        }
        match part {
            Part::Text { text } => {
                push(out, "text", width, dim());
                push(out, text, width, Style::default());
            }
            Part::Reasoning { text, .. } => {
                push(out, "reasoning", width, dim());
                push(out, text, width, Style::default());
            }
            Part::ToolCall { id, name, args } => {
                push(out, &format!("tool call {id} · {name}"), width, dim());
                push_json(out, args, width);
            }
            Part::ToolResult {
                id,
                output,
                is_error,
            } => {
                let status = if *is_error { "error" } else { "ok" };
                push(out, &format!("tool result {id} · {status}"), width, dim());
                push(out, output, width, Style::default());
            }
            Part::Image { media_type, data } => {
                push(
                    out,
                    &format!("image · {media_type} · {} bytes", decoded_len(data)),
                    width,
                    dim(),
                );
            }
        }
    }
}

/// A dim rule that starts a section; sections are separated by a blank line.
fn section(out: &mut Vec<Line<'static>>, label: &str, width: usize) {
    if !out.is_empty() {
        out.push(Line::default());
    }
    push(out, &format!("── {label} ──"), width, rule_style());
}

/// Push `text` wrapped to `width`, one ratatui line per row.
fn push(out: &mut Vec<Line<'static>>, text: &str, width: usize, style: Style) {
    for row in super::wrap(text, width) {
        out.push(Line::from(Span::styled(row, style)));
    }
}

fn push_json(out: &mut Vec<Line<'static>>, value: &Value, width: usize) {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    push(out, &text, width, Style::default());
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn rule_style() -> Style {
    Style::default().add_modifier(Modifier::DIM | Modifier::BOLD)
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn step_prefix(step: Option<usize>) -> String {
    step.map_or_else(String::new, |step| format!("step {step} "))
}

fn step_suffix(step: Option<usize>) -> String {
    step.map_or_else(String::new, |step| format!(" · step {step}"))
}

fn duration_label(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// Approximate decoded size of base64 `data`, for the image placeholder.
fn decoded_len(data: &str) -> usize {
    let data = data.trim();
    let padding = data.chars().rev().take_while(|&ch| ch == '=').count();
    (data.len() / 4 * 3).saturating_sub(padding)
}

/// Key names whose values must never be shown: authorization headers and
/// anything token/key/secret-ish. Suffix matching keeps `prompt_cache_key`,
/// `input_tokens` and `max_output_tokens` (counts, not secrets) visible.
fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    !normalized.is_empty()
        && (normalized == "key"
            || normalized == "cookie"
            || normalized == "setcookie"
            || normalized.ends_with("apikey")
            || normalized.ends_with("authorization")
            || normalized.ends_with("token")
            || normalized.ends_with("secret")
            || normalized.ends_with("password"))
}

/// Replace every sensitive value anywhere in `value`; the flag says whether
/// anything was redacted.
fn redact(value: &Value) -> (Value, bool) {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            let mut redacted = false;
            for (key, item) in map {
                if is_secret_key(key) {
                    out.insert(key.clone(), Value::String("[redacted]".into()));
                    redacted = true;
                } else {
                    let (item, nested) = redact(item);
                    out.insert(key.clone(), item);
                    redacted |= nested;
                }
            }
            (Value::Object(out), redacted)
        }
        Value::Array(items) => {
            let mut redacted = false;
            let items = items
                .iter()
                .map(|item| {
                    let (item, nested) = redact(item);
                    redacted |= nested;
                    item
                })
                .collect();
            (Value::Array(items), redacted)
        }
        other => (other.clone(), false),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::r#loop::Event;
    use crate::tui::timeline::{Entry, ToolRun};
    use crate::types::{StreamEvent, Usage};

    fn timeline() -> Timeline {
        Timeline::new(
            "You are Yoke.",
            "prefers small diffs",
            PathBuf::from("/home/u/.yoke/projects/-w/memory.md"),
            vec![json!({
                "name": "read",
                "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            })],
        )
    }

    fn render(timeline: &Timeline, width: u16) -> String {
        lines(timeline, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn header_content_is_present() {
        let rendered = render(&timeline(), 80);
        assert!(rendered.contains("── system ──"));
        assert!(rendered.contains("You are Yoke."));
        assert!(rendered.contains("── memory · /home/u/.yoke/projects/-w/memory.md ──"));
        assert!(rendered.contains("prefers small diffs"));
        assert!(rendered.contains("── tool schemas · 1 ──"));
        assert!(rendered.contains("\"description\": \"Read a file\""));
        assert!(rendered.contains("\"type\": \"string\""));
    }

    #[test]
    fn every_part_appears_with_full_content() {
        let mut timeline = timeline();
        timeline.push_message(Message {
            role: Role::Assistant,
            parts: vec![
                Part::Text {
                    text: "hello".into(),
                },
                Part::Reasoning {
                    text: "because the user asked".into(),
                    raw: None,
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    args: json!({"path": "src/main.rs"}),
                },
                Part::ToolResult {
                    id: "c1".into(),
                    output: "file body".into(),
                    is_error: true,
                },
                Part::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            ],
        });

        let rendered = render(&timeline, 80);
        for needle in [
            "message 1 · assistant · step 0",
            "text",
            "hello",
            "reasoning",
            "because the user asked",
            "tool call c1 · read",
            "\"path\": \"src/main.rs\"",
            "tool result c1 · error",
            "file body",
            "image · image/png · 3 bytes",
        ] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn long_tool_result_is_not_truncated() {
        let mut timeline = timeline();
        let output: String = (1..=150).map(|line| format!("line {line:03}\n")).collect();
        timeline.entries.push(Entry {
            step: Some(1),
            kind: EntryKind::Message(Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: "c1".into(),
                    output,
                    is_error: false,
                }],
            }),
        });

        let rendered = render(&timeline, 40);
        for line in 1..=150 {
            assert!(
                rendered.contains(&format!("line {line:03}")),
                "missing line {line}"
            );
        }
        assert_eq!(rendered.matches("line ").count(), 150);
        assert!(!rendered.contains('…'));
    }

    #[test]
    fn authorization_values_never_appear() {
        let mut timeline = timeline();
        timeline.apply(Event::StepStart { step: 2 });
        timeline.apply(Event::Stream(StreamEvent::Wire(json!({
            "headers": {
                "Authorization": "Bearer sk-live-secret",
                "x-api-key": "key-live-secret"
            },
            "metadata": {"api_key": "nested-secret"},
            "input": [{"type": "message", "max_output_tokens": 64}],
            "prompt_cache_key": "cache-visible"
        }))));

        let rendered = render(&timeline, 200);
        assert!(!rendered.contains("sk-live-secret"));
        assert!(!rendered.contains("key-live-secret"));
        assert!(!rendered.contains("nested-secret"));
        assert!(!rendered.contains("Bearer"));
        assert_eq!(rendered.matches("[redacted]").count(), 3);
        assert!(rendered.contains("secrets redacted"));
        // Counts and cache keys are not secrets and stay visible.
        assert!(rendered.contains("\"max_output_tokens\": 64"));
        assert!(rendered.contains("\"prompt_cache_key\": \"cache-visible\""));
    }

    #[test]
    fn usage_and_request_bodies_follow_step_order() {
        let mut timeline = timeline();
        for (step, input, marker) in [(1, 10, "body-one"), (2, 20, "body-two")] {
            timeline.apply(Event::StepStart { step });
            timeline.apply(Event::Stream(StreamEvent::Usage(Usage {
                input_tokens: input,
                output_tokens: step as u64,
            })));
            timeline.apply(Event::Stream(StreamEvent::Wire(json!({"marker": marker}))));
        }

        let rendered = render(&timeline, 100);
        let positions: Vec<usize> = [
            "step 1 usage",
            "step 1 request body",
            "body-one",
            "step 2 usage",
            "step 2 request body",
            "body-two",
        ]
        .iter()
        .map(|needle| {
            rendered
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{rendered}"))
        })
        .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{positions:?}"
        );
        assert!(rendered.contains("input tokens: 10"));
        assert!(rendered.contains("output tokens: 1"));
        assert!(rendered.contains("total: 11"));
        assert!(rendered.contains("input tokens: 20"));
        assert!(rendered.contains("total: 22"));
    }

    #[test]
    fn tool_runs_show_full_args_status_and_output() {
        let mut timeline = timeline();
        timeline.entries.push(Entry {
            step: Some(1),
            kind: EntryKind::Tool(ToolRun {
                id: "c1".into(),
                name: "bash".into(),
                args: json!({"command": "printf hi"}),
                status: RunStatus::Ok,
                output: "hi".into(),
                duration: Some(Duration::from_millis(12)),
                started: None,
            }),
        });

        let rendered = render(&timeline, 80);
        assert!(rendered.contains("tool run · bash · ok · 12ms · id c1"));
        assert!(rendered.contains("\"command\": \"printf hi\""));
        assert!(rendered.contains("output\nhi"));
    }

    #[test]
    fn streaming_buffers_are_shown_live() {
        let mut timeline = timeline();
        timeline.entries.push(Entry {
            step: Some(3),
            kind: EntryKind::Streaming {
                text: "half a sentence".into(),
                reasoning: "thinking it over".into(),
            },
        });

        let rendered = render(&timeline, 80);
        assert!(rendered.contains("── step 3 live ──"));
        assert!(rendered.contains("thinking it over"));
        assert!(rendered.contains("half a sentence"));
    }

    #[test]
    fn wrapping_preserves_every_character() {
        let text = "alpha-beta_gamma:delta/epsilon.0123456789".repeat(6);
        let mut out = Vec::new();
        push(&mut out, &text, 17, Style::default());
        assert!(out.len() > 1);
        assert!(out
            .iter()
            .all(|line| line.to_string().chars().count() <= 17));
        assert_eq!(
            out.iter().map(ToString::to_string).collect::<String>(),
            text
        );

        let blank_lines = "one\n\n  two\nthree";
        let mut out = Vec::new();
        push(&mut out, blank_lines, 80, Style::default());
        assert_eq!(
            out.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            blank_lines
        );
    }
}

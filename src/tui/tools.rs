//! Mode 2: Tools.
//!
//! Everything Overview shows, except tool runs are not folded: every call gets
//! its own row with compact arguments, status, duration and a boxed output.
//! `edit` and `write` show their diff, and reasoning is shown dimmed. A tool
//! call's output can be expanded in full by id (`Enter` in the app).

use std::collections::HashSet;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::timeline::{message_text, EntryKind, NoticeLevel, RunStatus, Timeline, ToolRun};
use crate::tui::wrap;
use crate::types::{Part, Role};

/// Lines kept at the head and tail of a folded box.
const HEAD: usize = 4;
const TAIL: usize = 4;
/// Boxes longer than this fold to `HEAD + … N lines … + TAIL`.
const FOLD_OVER: usize = HEAD + 2 + TAIL;

/// Mode 2 lines with every output folded.
#[allow(dead_code)] // tests render the folded view through it
pub fn lines(timeline: &Timeline, width: u16) -> Vec<Line<'static>> {
    lines_expanded(timeline, width, &HashSet::new(), None)
}

/// Mode 2 lines; tool calls whose id is in `expanded` show their full output,
/// and the call whose id is `selected` gets a marker.
pub fn lines_expanded(
    timeline: &Timeline,
    width: u16,
    expanded: &HashSet<String>,
    selected: Option<&str>,
) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut lines = context_header(timeline, width);

    for entry in &timeline.entries {
        match &entry.kind {
            EntryKind::Tool(run) => {
                let is_selected = selected == Some(run.id.as_str());
                push_tool(
                    &mut lines,
                    run,
                    width,
                    expanded.contains(&run.id),
                    is_selected,
                );
            }
            EntryKind::Message(message) => match message.role {
                Role::User => {
                    let text = message_text(message);
                    if !text.is_empty() {
                        blank(&mut lines);
                        labeled(&mut lines, "you", &text, width);
                    }
                }
                Role::Assistant => {
                    // Reasoning keeps its place among the message's parts.
                    for part in &message.parts {
                        match part {
                            Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                                blank(&mut lines);
                                reasoning(&mut lines, text, width);
                            }
                            Part::Text { text } if !text.is_empty() => {
                                blank(&mut lines);
                                labeled(&mut lines, "assistant", text, width);
                            }
                            _ => {}
                        }
                    }
                }
                Role::Tool => {}
            },
            EntryKind::Streaming { text, reasoning } => {
                if !reasoning.is_empty() {
                    blank(&mut lines);
                    self::reasoning(&mut lines, reasoning, width);
                }
                if !text.is_empty() {
                    blank(&mut lines);
                    let mut live = text.clone();
                    live.push('▌');
                    labeled(&mut lines, "assistant", &live, width);
                }
            }
            EntryKind::Notice { text, level } => {
                blank(&mut lines);
                let style = match level {
                    NoticeLevel::Info => Style::default().fg(Color::Yellow),
                    NoticeLevel::Error => Style::default().fg(Color::Red),
                };
                for (index, piece) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                    let prefix = if index == 0 { "! " } else { "  " };
                    lines.push(Line::from(Span::styled(format!("{prefix}{piece}"), style)));
                }
            }
            // Header entries are rendered above; usage and wire bodies belong
            // to mode 3.
            EntryKind::SystemPrompt(_)
            | EntryKind::Memory { .. }
            | EntryKind::ToolSchemas(_)
            | EntryKind::Usage { .. }
            | EntryKind::Wire { .. } => {}
        }
    }

    lines
}

fn context_header(timeline: &Timeline, width: usize) -> Vec<Line<'static>> {
    let sections = timeline.sections();
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        "context",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for (label, tokens) in [
        ("system prompt", sections.system),
        ("memory", sections.memory),
        ("tools", sections.tools),
        ("messages", sections.messages),
    ] {
        lines.push(token_row(label, tokens, width));
    }
    lines.push(truncated_line(
        Line::from(Span::styled(
            format!("  {:<15} ~{} tokens", "total", sections.total()),
            Style::default().add_modifier(Modifier::DIM),
        )),
        width,
    ));
    lines.push(Line::from(Span::styled(
        "─".repeat(width.min(40)),
        Style::default().add_modifier(Modifier::DIM),
    )));

    lines
}

fn token_row(label: &str, tokens: usize, width: usize) -> Line<'static> {
    truncated_line(
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{label:<15}"), Style::default().fg(Color::Gray)),
            Span::raw(format!(" ~{tokens} tokens")),
        ]),
        width,
    )
}

fn blank(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_some_and(|line| line.width() > 0) {
        lines.push(Line::default());
    }
}

fn labeled(lines: &mut Vec<Line<'static>>, label: &str, text: &str, width: usize) {
    let color = if label == "you" {
        Color::Cyan
    } else {
        Color::Green
    };
    lines.push(Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    for part in wrap(text, width) {
        lines.push(Line::from(part));
    }
}

fn reasoning(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    lines.push(Line::from(Span::styled(
        "reasoning",
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )));
    let style = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    for part in wrap(text, width) {
        lines.push(Line::from(Span::styled(part, style)));
    }
}

fn push_tool(
    lines: &mut Vec<Line<'static>>,
    run: &ToolRun,
    width: usize,
    expanded: bool,
    selected: bool,
) {
    blank(lines);
    lines.push(tool_row(run, width, selected));

    // A failed, denied or interrupted call never produced the diff it asked
    // for, so its output is what matters.
    if matches!(
        run.status,
        RunStatus::Error | RunStatus::Denied | RunStatus::Interrupted
    ) {
        output_box(lines, run, width, expanded);
        return;
    }
    match diff(run) {
        Some((title, body)) => push_box(lines, &title, fold(body, expanded), width),
        None => output_box(lines, run, width, expanded),
    }
}

fn tool_row(run: &ToolRun, width: usize, selected: bool) -> Line<'static> {
    let status = status_style(run.status);
    let mut tail = format!(" · {}", run.status.label());
    if let Some(duration) = run.duration {
        tail.push_str(&format!(" · {}", duration_text(duration)));
    }
    if let Some((added, removed)) = run.diff_stat() {
        if added > 0 || removed > 0 {
            tail.push_str(&format!(" · +{added} −{removed}"));
        }
    }
    let marker = if selected { 2 } else { 0 };
    let head = marker + 2 + run.name.chars().count();
    let budget = width.saturating_sub(head + tail.chars().count() + 1);
    let args = run.compact_args(budget);

    let mut spans = Vec::new();
    if selected {
        spans.push(Span::styled(
            "▶ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.extend([
        Span::styled("●", status.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" {}", run.name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {args}")),
        Span::styled(format!(" · {}", run.status.label()), status),
    ]);
    if let Some(duration) = run.duration {
        spans.push(Span::styled(
            format!(" · {}", duration_text(duration)),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    if let Some((added, removed)) = run.diff_stat() {
        if added > 0 || removed > 0 {
            spans.push(Span::styled(
                format!(" · +{added} −{removed}"),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }
    truncated_line(Line::from(spans), width)
}

fn status_style(status: RunStatus) -> Style {
    let color = match status {
        RunStatus::Ok => Color::Green,
        RunStatus::Error => Color::Red,
        RunStatus::Denied => Color::Magenta,
        RunStatus::Running | RunStatus::Interrupted => Color::Yellow,
    };
    Style::default().fg(color)
}

fn duration_text(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// One line inside an output or diff box.
#[derive(Debug, Clone)]
enum BoxLine {
    Plain(String),
    Removed(String),
    Added(String),
    /// The `… N lines …` marker for `N` folded lines.
    Elided(usize),
}

fn output_box(lines: &mut Vec<Line<'static>>, run: &ToolRun, width: usize, expanded: bool) {
    let mut body = if run.output.is_empty() {
        let placeholder = if run.status == RunStatus::Running {
            "(no output yet)"
        } else {
            "(no output)"
        };
        vec![BoxLine::Plain(placeholder.to_string())]
    } else {
        run.output
            .lines()
            .map(|line| BoxLine::Plain(line.to_string()))
            .collect()
    };
    if body.is_empty() {
        body.push(BoxLine::Plain("(no output)".to_string()));
    }
    let count = run.output.lines().count();
    let title = if count == 0 {
        "output".to_string()
    } else {
        format!("output · {count} lines")
    };
    push_box(lines, &title, fold(body, expanded), width);
}

/// The diff a successful `edit`/`write` asked for.
fn diff(run: &ToolRun) -> Option<(String, Vec<BoxLine>)> {
    let (added, removed) = run.diff_stat()?;
    match run.name.as_str() {
        "edit" => {
            let old = run.args.get("old_string").and_then(|v| v.as_str())?;
            let new = run.args.get("new_string").and_then(|v| v.as_str())?;
            let mut body = Vec::new();
            body.extend(old.lines().map(|line| BoxLine::Removed(line.to_string())));
            body.extend(new.lines().map(|line| BoxLine::Added(line.to_string())));
            if body.is_empty() {
                return None;
            }
            Some((format!("diff · +{added} −{removed}"), body))
        }
        "write" => {
            let content = run.args.get("content").and_then(|v| v.as_str())?;
            let body: Vec<BoxLine> = content
                .lines()
                .map(|line| BoxLine::Added(line.to_string()))
                .collect();
            if body.is_empty() {
                return None;
            }
            Some((format!("write · +{added}"), body))
        }
        _ => None,
    }
}

fn fold(body: Vec<BoxLine>, expanded: bool) -> Vec<BoxLine> {
    if expanded || body.len() <= FOLD_OVER {
        return body;
    }
    let hidden = body.len() - HEAD - TAIL;
    let mut folded = Vec::with_capacity(HEAD + TAIL + 1);
    folded.extend_from_slice(&body[..HEAD]);
    folded.push(BoxLine::Elided(hidden));
    folded.extend_from_slice(&body[body.len() - TAIL..]);
    folded
}

fn push_box(lines: &mut Vec<Line<'static>>, title: &str, body: Vec<BoxLine>, width: usize) {
    const INDENT: usize = 2;
    let inner = width.saturating_sub(INDENT + 2).max(1);
    let border = width.saturating_sub(INDENT).max(2);
    let border_style = Style::default().add_modifier(Modifier::DIM);

    let title = truncate_chars(title, border.saturating_sub(4));
    let top = format!("{}┌─ {title} ", " ".repeat(INDENT));
    let fill = border.saturating_sub(top.chars().count() - INDENT);
    lines.push(Line::from(Span::styled(
        format!("{top}{}", "─".repeat(fill)),
        border_style,
    )));

    for item in body {
        match item {
            BoxLine::Plain(text) => {
                for piece in wrap(&text, inner) {
                    lines.push(Line::from(format!("{}│ {piece}", " ".repeat(INDENT))));
                }
            }
            BoxLine::Removed(text) => {
                push_diff_piece(lines, '-', &text, inner, Color::Red);
            }
            BoxLine::Added(text) => {
                push_diff_piece(lines, '+', &text, inner, Color::Green);
            }
            BoxLine::Elided(hidden) => {
                lines.push(Line::from(Span::styled(
                    format!("{}│ … {hidden} lines …", " ".repeat(INDENT)),
                    Style::default().add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    lines.push(Line::from(Span::styled(
        format!(
            "{}└{}",
            " ".repeat(INDENT),
            "─".repeat(border.saturating_sub(1))
        ),
        border_style,
    )));
}

fn push_diff_piece(
    lines: &mut Vec<Line<'static>>,
    sign: char,
    text: &str,
    inner: usize,
    color: Color,
) {
    let style = Style::default().fg(color);
    for piece in wrap(text, inner.saturating_sub(2).max(1)) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(2)),
            Span::styled(
                format!("│ {sign} "),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled(piece, style),
        ]));
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// Cut a line to `width` characters, keeping each span's style.
fn truncated_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut remaining = width.max(1);
    let mut spans = Vec::new();
    for span in line.spans {
        let count = span.content.chars().count();
        if count <= remaining {
            remaining -= count;
            spans.push(span);
            continue;
        }
        let mut text: String = span
            .content
            .chars()
            .take(remaining.saturating_sub(1))
            .collect();
        if remaining > 0 {
            text.push('…');
        }
        spans.push(Span::styled(text, span.style));
        break;
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    use serde_json::{json, Value};

    use super::*;
    use crate::tui::timeline::{Entry, EntryKind, RunStatus, ToolRun};
    use crate::types::Message;

    fn header() -> Timeline {
        Timeline::new(
            "You are Yoke.",
            "prefers small diffs",
            PathBuf::from("/home/u/.yoke/projects/-w/memory.md"),
            vec![json!({"name": "read"})],
        )
    }

    fn message(role: Role, text: &str) -> Message {
        Message {
            role,
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn tool(id: &str, name: &str, args: Value, status: RunStatus, output: &str) -> ToolRun {
        ToolRun {
            id: id.into(),
            name: name.into(),
            args,
            status,
            output: output.into(),
            duration: Some(Duration::from_millis(120)),
            started: None,
        }
    }

    fn push_tool(timeline: &mut Timeline, run: ToolRun) {
        timeline.entries.push(Entry {
            step: Some(1),
            kind: EntryKind::Tool(run),
        });
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    fn all_text(lines: &[Line<'_>]) -> String {
        texts(lines).join("\n")
    }

    fn span_color<'a>(lines: &'a [Line<'a>], content: &str) -> Option<Color> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == content)
            .and_then(|span| span.style.fg)
    }

    fn span_style<'a>(lines: &'a [Line<'a>], content: &str) -> Option<Style> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == content)
            .map(|span| span.style)
    }

    #[test]
    fn tool_rows_show_name_status_and_color() {
        let mut timeline = header();
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "read",
                json!({"path": "a.rs"}),
                RunStatus::Running,
                "",
            ),
        );
        push_tool(
            &mut timeline,
            tool("c2", "bash", json!({"command": "ls"}), RunStatus::Ok, "ok"),
        );
        push_tool(
            &mut timeline,
            tool(
                "c3",
                "read",
                json!({"path": "b.rs"}),
                RunStatus::Error,
                "boom",
            ),
        );
        push_tool(
            &mut timeline,
            tool(
                "c4",
                "write",
                json!({"path": "c.rs"}),
                RunStatus::Denied,
                "denied",
            ),
        );

        let lines = lines(&timeline, 80);
        let text = all_text(&lines);
        assert!(text.contains("● read"));
        assert!(text.contains("● bash"));
        assert!(text.contains("● write"));
        assert!(text.contains("running"));
        assert!(text.contains("ok"));
        assert!(text.contains("error"));
        assert!(text.contains("denied"));
        assert!(text.contains("120ms"));
        assert!(text.contains("no output yet"));

        assert_eq!(span_color(&lines, " · running"), Some(Color::Yellow));
        assert_eq!(span_color(&lines, " · ok"), Some(Color::Green));
        assert_eq!(span_color(&lines, " · error"), Some(Color::Red));
        assert_eq!(span_color(&lines, " · denied"), Some(Color::Magenta));
        // Mode 2 never folds runs into the Overview group line.
        assert!(!text.contains("▸"));
    }

    #[test]
    fn long_output_folds_to_head_tail_with_marker() {
        let mut timeline = header();
        let output = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "bash",
                json!({"command": "seq"}),
                RunStatus::Ok,
                &output,
            ),
        );

        let lines = lines(&timeline, 80);
        let text = texts(&lines);
        for expected in ["  │ line 1", "  │ line 4", "  │ line 17", "  │ line 20"] {
            assert!(
                text.iter().any(|line| line == expected),
                "missing {expected:?}"
            );
        }
        assert!(text.iter().any(|line| line == "  │ … 12 lines …"));
        assert!(!text.iter().any(|line| line.contains("line 5")));
        assert!(!text.iter().any(|line| line.contains("line 16")));
    }

    #[test]
    fn only_the_selected_tool_row_gets_the_marker() {
        let mut timeline = header();
        push_tool(
            &mut timeline,
            tool("c1", "read", json!({"path": "a.rs"}), RunStatus::Ok, "one"),
        );
        push_tool(
            &mut timeline,
            tool("c2", "bash", json!({"command": "ls"}), RunStatus::Ok, "ok"),
        );

        let lines = lines_expanded(&timeline, 80, &HashSet::new(), Some("c2"));
        let text = texts(&lines);
        assert!(text.iter().any(|line| line.starts_with("▶ ● bash")));
        assert!(!text.iter().any(|line| line.contains("▶ ● read")));
        assert_eq!(span_color(&lines, "▶ "), Some(Color::Cyan));
    }

    #[test]
    fn expanded_output_shows_every_line() {
        let mut timeline = header();
        let output = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "bash",
                json!({"command": "seq"}),
                RunStatus::Ok,
                &output,
            ),
        );

        let expanded: HashSet<String> = ["c1".to_string()].into_iter().collect();
        let lines = lines_expanded(&timeline, 80, &expanded, None);
        let text = all_text(&lines);
        assert!(text.contains("  │ line 5\n"));
        assert!(text.contains("  │ line 16\n"));
        assert!(text.contains("  │ line 20"));
        assert!(!text.contains("lines …"));
    }

    #[test]
    fn edit_shows_removed_and_added_lines() {
        let mut timeline = header();
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "edit",
                json!({"path": "a.rs", "old_string": "one\ntwo", "new_string": "one\ntwo\nthree"}),
                RunStatus::Ok,
                "edited a.rs",
            ),
        );

        let lines = lines(&timeline, 80);
        let text = texts(&lines);
        assert!(text.iter().any(|line| line == "  │ - one"));
        assert!(text.iter().any(|line| line == "  │ - two"));
        assert!(text.iter().any(|line| line == "  │ + three"));
        assert_eq!(span_color(&lines, "one"), Some(Color::Red));
        assert_eq!(span_color(&lines, "three"), Some(Color::Green));
        // The edit's diff replaces the raw output box.
        assert!(!text.iter().any(|line| line.contains("edited a.rs")));
    }

    #[test]
    fn write_shows_content_as_added_lines() {
        let mut timeline = header();
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "write",
                json!({"path": "a.rs", "content": "fn main() {}\n"}),
                RunStatus::Ok,
                "wrote a.rs",
            ),
        );

        let lines = lines(&timeline, 80);
        let text = texts(&lines);
        assert!(text.iter().any(|line| line == "  │ + fn main() {}"));
        assert!(text.iter().any(|line| line.starts_with("  ┌─ write · +1 ")));
        assert_eq!(span_color(&lines, "fn main() {}"), Some(Color::Green));
    }

    #[test]
    fn reasoning_is_dimmed_and_italic_in_place() {
        let mut timeline = header();
        timeline.push_message(Message {
            role: Role::Assistant,
            parts: vec![
                Part::Reasoning {
                    text: "checking the file".into(),
                    raw: None,
                },
                Part::Text {
                    text: "it is fine".into(),
                },
            ],
        });

        let lines = lines(&timeline, 80);
        let text = all_text(&lines);
        assert!(text.contains("reasoning"));
        assert!(text.contains("checking the file"));
        assert!(text.contains("it is fine"));

        let style = span_style(&lines, "checking the file").expect("reasoning span");
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn narrow_width_wraps_and_truncates_every_line() {
        let mut timeline = header();
        timeline.push_message(message(Role::User, "abcdefghijklmnopqrstuvwxyz0123456789"));
        let output = (1..=12)
            .map(|n| format!("output line number {n} with some words"))
            .collect::<Vec<_>>()
            .join("\n");
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "bash",
                json!({"command": "long"}),
                RunStatus::Ok,
                &output,
            ),
        );

        let lines = lines(&timeline, 24);
        for line in &lines {
            assert!(
                line.width() <= 24,
                "line wider than 24: {:?} ({})",
                line_text(line),
                line.width()
            );
        }
        let text = all_text(&lines);
        assert!(text.contains("abcdefghijklmnopqrstuv"));
    }

    #[test]
    fn renders_through_a_paragraph() {
        let mut timeline = header();
        timeline.push_message(message(Role::User, "please fix it"));
        timeline.push_message(message(Role::Assistant, "reading a.rs"));
        let output = (1..=12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        push_tool(
            &mut timeline,
            tool(
                "c1",
                "read",
                json!({"path": "a.rs"}),
                RunStatus::Ok,
                &output,
            ),
        );

        let rendered = lines(&timeline, 60);
        let height = rendered.len() as u16;
        let mut terminal = Terminal::new(TestBackend::new(60, height)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new(rendered), frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()));
            }
            text.push_str(row.trim_end());
            text.push('\n');
        }

        assert!(text.contains("context"));
        assert!(text.contains("please fix it"));
        assert!(text.contains("reading a.rs"));
        assert!(text.contains("● read"));
        assert!(text.contains("  │ line 1"));
        assert!(text.contains("  │ … 4 lines …"));
        assert!(!text.contains("▸"));
        assert!(text.contains("  │ line 12"));
    }
}

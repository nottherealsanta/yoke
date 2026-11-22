//! Mode 1: Overview.
//!
//! The context header, then user and assistant messages in full. Runs of tool
//! calls fold into one line each. Reasoning is not shown in this mode.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::timeline::{message_text, EntryKind, NoticeLevel, RunStatus, Timeline, ToolRun};
use crate::tui::wrap;
use crate::types::Role;

pub fn lines(timeline: &Timeline, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let sections = timeline.sections();
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        "context",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(token_row("system prompt", sections.system));
    if let Some(tokens) = sections.memory {
        lines.push(token_row("memory", tokens));
    }
    lines.push(token_row("tools", sections.tools));
    lines.push(token_row("messages", sections.messages));
    lines.push(Line::from(Span::styled(
        format!("  {:<15} ~{} tokens", "total", sections.total()),
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(Span::styled(
        "─".repeat(width.min(40)),
        Style::default().add_modifier(Modifier::DIM),
    )));

    let entries = &timeline.entries;
    let mut index = 0;
    while index < entries.len() {
        match &entries[index].kind {
            EntryKind::Tool(_) => {
                let start = index;
                while index < entries.len() && matches!(entries[index].kind, EntryKind::Tool(_)) {
                    index += 1;
                }
                let runs: Vec<&ToolRun> = entries[start..index]
                    .iter()
                    .filter_map(|entry| match &entry.kind {
                        EntryKind::Tool(run) => Some(run),
                        _ => None,
                    })
                    .collect();
                if !runs.is_empty() {
                    blank(&mut lines);
                    lines.push(group_line(&runs));
                }
            }
            EntryKind::Message(message) => {
                let text = message_text(message);
                match message.role {
                    Role::User => {
                        blank(&mut lines);
                        labeled(&mut lines, "you", &text, width);
                    }
                    Role::Assistant => {
                        if !text.is_empty() {
                            blank(&mut lines);
                            labeled(&mut lines, "assistant", &text, width);
                        }
                    }
                    Role::Tool => {}
                }
                index += 1;
            }
            EntryKind::Streaming { text, reasoning } => {
                if !text.is_empty() {
                    blank(&mut lines);
                    let mut live = text.clone();
                    live.push('▌');
                    labeled(&mut lines, "assistant", &live, width);
                } else if !reasoning.is_empty() {
                    // Reasoning is dim and mode-2 material; Overview stays quiet.
                }
                index += 1;
            }
            EntryKind::Notice { text, level } => {
                blank(&mut lines);
                let style = match level {
                    NoticeLevel::Info => Style::default().fg(Color::Yellow),
                    NoticeLevel::Error => Style::default().fg(Color::Red),
                };
                lines.push(Line::from(Span::styled(format!("! {text}"), style)));
                index += 1;
            }
            // Header entries are rendered above; usage and wire bodies are
            // shown by mode 3, where they belong.
            EntryKind::SystemPrompt(_)
            | EntryKind::Memory { .. }
            | EntryKind::ToolSchemas(_)
            | EntryKind::Usage { .. }
            | EntryKind::Wire { .. } => {
                index += 1;
            }
        }
    }

    lines
}

fn token_row(label: &str, tokens: usize) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{label:<15}"), Style::default().fg(Color::Gray)),
        Span::raw(format!(" ~{tokens} tokens")),
    ])
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

fn group_line(runs: &[&ToolRun]) -> Line<'static> {
    let mut names: Vec<(String, usize)> = Vec::new();
    for run in runs {
        match names.iter_mut().find(|(name, _)| name == &run.name) {
            Some((_, count)) => *count += 1,
            None => names.push((run.name.clone(), 1)),
        }
    }
    let names = names
        .into_iter()
        .map(|(name, count)| {
            if count == 1 {
                name
            } else {
                format!("{name} ×{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    let (added, removed) = runs.iter().fold((0, 0), |(added, removed), run| {
        run.diff_stat()
            .map_or((added, removed), |(a, r)| (added + a, removed + r))
    });
    let plural = if runs.len() == 1 { "tool" } else { "tools" };
    let mut text = format!("▸ {} {plural} · {names}", runs.len());
    if added > 0 || removed > 0 {
        text.push_str(&format!(" · +{added} −{removed}"));
    }
    if runs.iter().any(|run| run.status == RunStatus::Running) {
        text.push_str(" · running");
    }

    Line::from(vec![
        Span::styled("▸", Style::default().fg(Color::Cyan)),
        Span::raw(text.trim_start_matches('▸').to_string()),
    ])
}

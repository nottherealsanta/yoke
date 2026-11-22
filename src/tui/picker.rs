//! The `/model` picker: connected providers, their models, filtering and selection.
//!
//! The picker is view state only. Selecting a row hands the provider and model id
//! back to `App`, which rebuilds the provider and keeps the conversation.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::models::{self, Model};
use crate::providers::PROVIDERS;

/// One provider heading and the models shown under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub provider: String,
    pub label: String,
    pub models: Vec<Model>,
}

/// The providers and models to list: connected ones plus the active provider,
/// with the active model ensured present.
pub fn groups(
    catalog: Option<&models::Catalog>,
    active_provider: &str,
    active_model: &str,
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<Group> {
    let mut groups = Vec::new();
    for info in PROVIDERS {
        let active = info.id == active_provider;
        if !active && (!models::buildable(info.id) || !models::connected(info, home, &env)) {
            continue;
        }
        let current = active.then_some(active_model);
        let models = models::models_for(catalog, info.id, current);
        groups.push(Group {
            provider: info.id.to_string(),
            label: info.label.to_string(),
            models,
        });
    }
    if !groups.iter().any(|group| group.provider == active_provider) {
        groups.push(Group {
            provider: active_provider.to_string(),
            label: active_provider.to_string(),
            models: models::models_for(catalog, active_provider, Some(active_model)),
        });
    }
    groups
}

/// The picker's state: the unfiltered groups, the filter, and the selected row.
#[derive(Debug, Clone)]
pub struct Picker {
    groups: Vec<Group>,
    filter: String,
    selected: usize,
    loading: bool,
    error: Option<String>,
    active_provider: String,
    active_model: String,
}

impl Picker {
    pub fn new(
        groups: Vec<Group>,
        active_provider: &str,
        active_model: &str,
        loading: bool,
    ) -> Self {
        Self {
            groups,
            filter: String::new(),
            selected: 0,
            loading,
            error: None,
            active_provider: active_provider.to_string(),
            active_model: active_model.to_string(),
        }
    }

    pub fn set_groups(&mut self, groups: Vec<Group>) {
        self.groups = groups;
        self.clamp_selection();
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    fn matches(&self, model: &Model) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let filter = self.filter.to_lowercase();
        model.id.to_lowercase().contains(&filter) || model.name.to_lowercase().contains(&filter)
    }

    /// Groups with their models filtered; empty groups disappear.
    pub fn visible_groups(&self) -> Vec<(&Group, Vec<&Model>)> {
        self.groups
            .iter()
            .filter_map(|group| {
                let models: Vec<&Model> = group
                    .models
                    .iter()
                    .filter(|model| self.matches(model))
                    .collect();
                (!models.is_empty()).then_some((group, models))
            })
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.visible_groups()
            .iter()
            .map(|(_, models)| models.len())
            .sum()
    }

    pub fn selected_choice(&self) -> Option<(String, String)> {
        let mut index = 0;
        for (group, models) in self.visible_groups() {
            for model in models {
                if index == self.selected {
                    return Some((group.provider.clone(), model.id.clone()));
                }
                index += 1;
            }
        }
        None
    }

    pub fn push_filter(&mut self, ch: char) {
        self.filter.push(ch);
        self.selected = 0;
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.selected = 0;
    }

    /// Move the selection by `delta` visible rows, stopping at either end.
    pub fn move_by(&mut self, delta: isize) {
        let len = self.visible_count();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let last = len - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs()).min(last)
        } else {
            (self.selected + delta as usize).min(last)
        };
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_count();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    /// The overlay body, one line per provider heading and model, plus status.
    #[cfg(test)]
    pub fn rows(&self) -> Vec<Line<'static>> {
        self.build_rows().0
    }

    /// A window of `max` rows that keeps the selected model visible and the
    /// status line at the bottom.
    pub fn rows_window(&self, max: usize) -> Vec<Line<'static>> {
        let (rows, selected) = self.build_rows();
        if rows.len() <= max || max < 2 {
            return rows;
        }
        let footer = rows.last().cloned();
        let body = &rows[..rows.len() - 1];
        let max = max.saturating_sub(1).min(body.len()).max(1);
        let mut start = selected.saturating_sub(max / 2);
        start = start.min(body.len().saturating_sub(max));
        let mut window: Vec<Line<'static>> = body[start..start + max].to_vec();
        if let Some(footer) = footer {
            window.push(footer);
        }
        window
    }

    fn build_rows(&self) -> (Vec<Line<'static>>, usize) {
        let mut rows = Vec::new();
        let mut selected_row = 0;
        let visible = self.visible_groups();
        let mut index = 0;
        for (group, models) in &visible {
            rows.push(Line::from(Span::styled(
                format!("{} · {}", group.provider, group.label),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for model in models {
                let selected = index == self.selected;
                let current =
                    group.provider == self.active_provider && model.id == self.active_model;
                let marker = if selected {
                    "▶"
                } else if current {
                    "●"
                } else {
                    " "
                };
                let mut text = format!(" {marker} {}", model.label());
                if let Some(context) = model.context {
                    text.push_str(&format!(" · {}", tokens(context)));
                }
                if current {
                    text.push_str("  current");
                }
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                };
                if selected {
                    selected_row = rows.len();
                }
                rows.push(Line::from(Span::styled(text, style)));
                index += 1;
            }
        }
        if visible.is_empty() {
            rows.push(Line::from(Span::styled(
                "  no matching models",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        let status = if let Some(error) = &self.error {
            format!("  {error}")
        } else if self.loading {
            "  loading models…".to_string()
        } else if self.filter.is_empty() {
            "  type to filter · Enter select · Esc cancel".to_string()
        } else {
            format!("  filter: {} · Enter select · Esc cancel", self.filter)
        };
        let style = if self.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        rows.push(Line::from(Span::styled(status, style)));
        (rows, selected_row)
    }
}

fn tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;
        if millions.fract() == 0.0 {
            format!("{millions:.0}M")
        } else {
            format!("{millions:.1}M")
        }
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Catalog;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn model(id: &str, name: &str) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            context: Some(200_000),
        }
    }

    fn groups_for(
        active_provider: &str,
        active_model: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Group> {
        let catalog = Catalog {
            fetched_at: 1,
            providers: BTreeMap::from([
                (
                    "groq".to_string(),
                    vec![model("openai/gpt-oss-120b", "GPT OSS 120B")],
                ),
                ("openai".to_string(), vec![model("gpt-5.4", "GPT-5.4")]),
            ]),
        };
        groups(
            Some(&catalog),
            active_provider,
            active_model,
            &PathBuf::from("/nonexistent"),
            env,
        )
    }

    #[test]
    fn only_connected_providers_and_the_active_one_are_listed() {
        let groups = groups_for("openai", "gpt-5.4", &|_| None);
        let ids: Vec<&str> = groups.iter().map(|group| group.provider.as_str()).collect();
        assert_eq!(ids, ["openai"], "only the active provider is connected");

        let groups = groups_for("openai", "gpt-5.4", &|name| {
            (name == "GROQ_API_KEY").then(|| "key".into())
        });
        let ids: Vec<&str> = groups.iter().map(|group| group.provider.as_str()).collect();
        assert_eq!(ids, ["openai", "groq"]);
    }

    #[test]
    fn filtering_matches_ids_and_names_and_hides_empty_groups() {
        let env = |name: &str| (name == "GROQ_API_KEY").then(|| "key".to_string());
        let mut picker = Picker::new(
            groups_for("openai", "gpt-5.4", &env),
            "openai",
            "gpt-5.4",
            false,
        );
        assert_eq!(picker.visible_count(), 2, "openai gpt-5.4, groq gpt-oss");
        for ch in "5.4".chars() {
            picker.push_filter(ch);
        }
        assert_eq!(picker.visible_count(), 1);
        assert_eq!(
            picker.selected_choice().unwrap(),
            ("openai".into(), "gpt-5.4".into())
        );
        picker.pop_filter();
        picker.pop_filter();
        picker.pop_filter();
        for ch in "OSS 120".chars() {
            picker.push_filter(ch);
        }
        assert_eq!(
            picker.selected_choice().unwrap(),
            ("groq".into(), "openai/gpt-oss-120b".into())
        );
        for ch in "zzz".chars() {
            picker.push_filter(ch);
        }
        assert_eq!(picker.visible_count(), 0);
        assert!(picker.selected_choice().is_none());
    }

    #[test]
    fn selection_moves_and_clamps_within_the_visible_rows() {
        let env = |name: &str| (name == "GROQ_API_KEY").then(|| "key".to_string());
        let mut picker = Picker::new(
            groups_for("openai", "gpt-5.4", &env),
            "openai",
            "gpt-5.4",
            false,
        );
        assert_eq!(picker.selected_choice().unwrap().1, "gpt-5.4");
        picker.move_by(1);
        assert_eq!(picker.selected_choice().unwrap().1, "openai/gpt-oss-120b");
        picker.move_by(100);
        assert_eq!(picker.selected(), picker.visible_count() - 1);
        picker.move_by(-100);
        assert_eq!(picker.selected(), 0);
        for ch in "nomatch".chars() {
            picker.push_filter(ch);
        }
        picker.move_by(5);
        assert_eq!(picker.selected(), 0);
    }

    #[test]
    fn rows_mark_the_current_model_and_show_context_windows() {
        let picker = Picker::new(
            groups_for("openai", "gpt-5.4", &|_| None),
            "openai",
            "gpt-5.4",
            false,
        );
        let rows: Vec<String> = picker.rows().iter().map(ToString::to_string).collect();
        let body = rows.join("\n");
        assert!(body.contains("GPT-5.4 (gpt-5.4) · 200k"), "{body}");
        assert!(body.contains("current"), "{body}");
        assert!(body.contains("openai · OpenAI"), "{body}");
        assert!(body.contains("type to filter"), "{body}");
    }

    #[test]
    fn token_sizes_are_readable() {
        assert_eq!(tokens(128_000), "128k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_050_000), "1.1M");
        assert_eq!(tokens(512), "512");
    }
}

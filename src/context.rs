//! Context accounting: rough token estimates per section, and real usage.

use serde_json::Value;

use crate::types::{Message, Usage};

/// A rough token count for text: one token per four characters, rounded up.
pub fn estimate(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// The model's context window from a catalog the caller already holds; `None`
/// when the model is unknown (the status line shows `?`).
pub fn context_window_from(
    catalog: Option<&crate::models::Catalog>,
    provider: &str,
    model: &str,
) -> Option<usize> {
    catalog?.context_window(provider, model)
}

/// The model's context window from the cached catalog in the default Yoke home.
/// The TUI holds its catalog and calls `context_window_from` instead, so this is
/// for callers that only know the provider and model id.
pub fn context_window(provider: &str, model: &str) -> Option<usize> {
    let home = crate::memory::home(std::env::var("YOKE_HOME").ok().as_deref())?;
    let catalog = crate::models::load_cache(&home)?;
    context_window_from(Some(&catalog), provider, model)
}

/// Estimated tokens for each visible part of the context. `memory` is `None`
/// when memory is disabled, so no memory row is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sections {
    pub system: usize,
    pub memory: Option<usize>,
    pub tools: usize,
    pub messages: usize,
}

impl Sections {
    /// `system` is the full system prompt; the memory text is already inside it,
    /// so it is subtracted here and counted once, under `memory`.
    pub fn new(system: &str, memory: Option<&str>, tools: &[Value], messages: &[Message]) -> Self {
        let memory_text = memory.unwrap_or_default();
        let system = match system.rfind(memory_text) {
            Some(at) if !memory_text.is_empty() => {
                let mut without = String::with_capacity(system.len() - memory_text.len());
                without.push_str(&system[..at]);
                without.push_str(&system[at + memory_text.len()..]);
                without
            }
            _ => system.to_string(),
        };
        Self {
            system: estimate(&system),
            memory: memory.map(estimate),
            tools: estimate(&serde_json::to_string(tools).unwrap_or_default()),
            messages: estimate(&serde_json::to_string(messages).unwrap_or_default()),
        }
    }

    pub fn total(&self) -> usize {
        self.system + self.memory.unwrap_or(0) + self.tools + self.messages
    }

    /// Tokens used: the provider's real input count from the last step when it
    /// sent one, otherwise the estimate.
    pub fn used(&self, usage: Option<Usage>) -> usize {
        usage.map_or_else(|| self.total(), |usage| usage.input_tokens as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Part, Role};
    use serde_json::json;

    #[test]
    fn estimate_rounds_up_and_counts_chars() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
        assert_eq!(estimate("ééééé"), 2);
    }

    #[test]
    fn sections_estimate_each_part_and_total() {
        let tools = vec![json!({"name": "read"})];
        let messages = vec![Message {
            role: Role::User,
            parts: vec![Part::Text {
                text: "hello".into(),
            }],
        }];
        let sections = Sections::new("be brief\n\nmemory", Some("memory"), &tools, &messages);

        assert_eq!(sections.system, estimate("be brief\n\n"));
        assert_eq!(sections.memory, Some(2));
        assert_eq!(
            sections.tools,
            estimate(&serde_json::to_string(&tools).unwrap())
        );
        assert_eq!(
            sections.messages,
            estimate(&serde_json::to_string(&messages).unwrap())
        );
        assert_eq!(
            sections.total(),
            sections.system + sections.memory.unwrap() + sections.tools + sections.messages
        );
    }

    #[test]
    fn memory_is_removed_from_the_end_of_the_prompt() {
        let system = "memory in the heading\n\nmemory";
        let sections = Sections::new(system, Some("memory"), &[], &[]);
        assert_eq!(sections.system, estimate("memory in the heading\n\n"));
        assert_eq!(sections.memory, Some(2));
    }

    #[test]
    fn no_memory_counts_no_memory_section() {
        let sections = Sections::new("just the system prompt", None, &[], &[]);
        assert_eq!(sections.memory, None);
        assert_eq!(sections.system, estimate("just the system prompt"));
        assert_eq!(
            sections.total(),
            sections.system + sections.tools + sections.messages
        );
    }

    #[test]
    fn context_windows_come_from_the_catalog_and_are_unknown_without_one() {
        use crate::models::{Catalog, Model};
        use std::collections::BTreeMap;

        assert_eq!(
            context_window_from(None, "groq", "openai/gpt-oss-120b"),
            None
        );
        let catalog = Catalog {
            fetched_at: 1,
            providers: BTreeMap::from([(
                "groq".to_string(),
                vec![Model {
                    id: "openai/gpt-oss-120b".into(),
                    name: "GPT OSS 120B".into(),
                    context: Some(131_072),
                }],
            )]),
        };
        assert_eq!(
            context_window_from(Some(&catalog), "groq", "openai/gpt-oss-120b"),
            Some(131_072)
        );
        assert_eq!(context_window_from(Some(&catalog), "groq", "unknown"), None);
    }

    #[test]
    fn real_usage_wins_over_the_estimate() {
        let sections = Sections {
            system: 10,
            memory: Some(20),
            tools: 30,
            messages: 40,
        };
        assert_eq!(sections.used(None), 100);
        assert_eq!(
            sections.used(Some(Usage {
                input_tokens: 1234,
                output_tokens: 56,
            })),
            1234
        );
    }
}

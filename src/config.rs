//! Run configuration shared by the CLI and embedders.
//!
//! [`Config`] ties the safety options together: which tools exist, where the
//! file tools may touch disk, and whether memory is part of the prompt. The
//! CLI fills it from `--tools`, `--root` and `--no-memory`; an embedder sets
//! the fields directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::memory;
use crate::prompt;
use crate::tools::{self, Registry};

/// Everything a consumer can restrict before a turn starts.
#[derive(Debug, Clone)]
pub struct Config {
    /// Tool names to keep; `None` keeps all four default tools.
    pub tools: Option<Vec<String>>,
    /// Confine `read`, `write` and `edit` to this directory.
    pub root: Option<PathBuf>,
    /// Include the project memory file in the system prompt.
    pub memory: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tools: None,
            root: None,
            memory: true,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// The default registry, restricted to [`Config::tools`] and confined to
    /// [`Config::root`]. Unknown tool names are an error.
    pub fn registry(&self) -> Result<Registry> {
        let mut registry = tools::default_registry();
        if let Some(names) = &self.tools {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            registry.only(&names).map_err(anyhow::Error::msg)?;
        }
        if let Some(root) = &self.root {
            registry = registry.confine(root)?;
        }
        Ok(registry)
    }

    /// The workspace shown to the model and used as the project slug. With a
    /// `root`, that is the canonical root; otherwise the current directory.
    pub fn workspace(&self, cwd: &Path) -> Result<PathBuf> {
        match &self.root {
            Some(root) => root
                .canonicalize()
                .with_context(|| format!("root {}", root.display())),
            None => Ok(cwd.to_path_buf()),
        }
    }

    /// The memory file text, or `None` when `--no-memory` is in effect.
    pub fn memory(&self, path: &Path) -> Option<(PathBuf, String)> {
        self.memory
            .then(|| (path.to_path_buf(), memory::read(path)))
    }

    /// The system prompt: workspace, date, the enabled tool names and either
    /// the memory text or nothing at all.
    pub fn instructions(
        &self,
        workspace: &Path,
        date: &str,
        tool_names: &[String],
        memory: Option<(&Path, &str)>,
    ) -> String {
        prompt::build(workspace, date, tool_names, memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;

    #[test]
    fn defaults_keep_all_tools_root_free_and_memory_on() {
        let config = Config::default();
        assert!(config.tools.is_none() && config.root.is_none() && config.memory);
        assert_eq!(config.registry().unwrap().names().len(), 4);
    }

    #[test]
    fn restricts_the_registry_and_rejects_unknown_names() {
        let config = Config {
            tools: Some(vec!["read".into(), "edit".into()]),
            ..Config::default()
        };
        assert_eq!(config.registry().unwrap().names(), ["read", "edit"]);

        let config = Config {
            tools: Some(vec!["nope".into()]),
            ..Config::default()
        };
        assert!(config
            .registry()
            .err()
            .unwrap()
            .to_string()
            .contains("nope"));
    }

    #[test]
    fn workspace_is_the_canonical_root_when_given() {
        let dir = TempDir::new("config-root");
        let config = Config {
            root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        assert_eq!(
            config.workspace(Path::new("/elsewhere")).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn no_memory_means_no_memory_text_and_no_memory_section() {
        let dir = TempDir::new("config-memory");
        let path = dir.path().join("memory.md");
        std::fs::write(&path, "keep it short\n").unwrap();

        let config = Config {
            memory: false,
            ..Config::default()
        };
        assert!(config.memory(&path).is_none());
        let prompt = config.instructions(Path::new("/work"), "2026-11-22", &["read".into()], None);
        assert!(!prompt.contains("# Memory"));
        assert!(!prompt.contains("keep it short"));

        let config = Config::default();
        let memory = config.memory(&path).unwrap();
        assert_eq!(memory.1, "keep it short\n");
    }
}

//! The tool registry; one file per tool.

pub mod bash;
pub mod confine;
pub mod edit;
pub mod read;
pub mod write;

pub use bash::Bash;
pub use confine::{resolve, Confined};
pub use edit::Edit;
pub use read::Read;
pub use write::Write;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::types::Part;

/// Live output chunks from a running tool (e.g. bash stdout).
pub type Sink = mpsc::UnboundedSender<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }

    pub fn into_part(self, id: String) -> Part {
        Part::ToolResult {
            id,
            output: self.output,
            is_error: self.is_error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Ok,
    Error,
    Denied,
    Interrupted,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    /// Whether the loop must ask the user before running this tool.
    fn needs_approval(&self) -> bool {
        false
    }
    fn schema(&self) -> Value;
    async fn run(&self, args: Value, out: Sink) -> ToolResult;
}

#[derive(Default)]
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tool. Later tools with the same name win, as before.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Keep only the named tools. An unknown name is an error and leaves the
    /// registry untouched, so `--tools` fails loudly instead of silently.
    pub fn only(&mut self, names: &[&str]) -> Result<(), String> {
        let available = self.names();
        for name in names {
            if !self.tools.iter().any(|tool| tool.name() == *name) {
                return Err(format!(
                    "unknown tool `{name}` (available: {})",
                    available.join(", ")
                ));
            }
        }
        self.tools.retain(|tool| names.contains(&tool.name()));
        Ok(())
    }

    /// Wrap every tool so its `path` argument is confined to `root`.
    ///
    /// The wrapper is generic: tools without a `path` argument (such as
    /// `bash`) are unchanged.
    pub fn confine(mut self, root: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().canonicalize().map_err(|error| {
            anyhow::anyhow!("cannot use {} as root: {error}", root.as_ref().display())
        })?;
        self.tools = self
            .tools
            .into_iter()
            .map(|tool| Box::new(Confined::new(tool, &root).expect("root exists")) as Box<dyn Tool>)
            .collect();
        Ok(self)
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub fn schemas(&self) -> Vec<Value> {
        self.tools.iter().map(|tool| tool.schema()).collect()
    }
}

pub fn default_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(Box::new(read::Read));
    registry.register(Box::new(write::Write));
    registry.register(Box::new(edit::Edit));
    registry.register(Box::new(bash::Bash));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_all_four_tools() {
        let registry = default_registry();
        for name in ["read", "write", "edit", "bash"] {
            assert!(registry.get(name).is_some(), "{name}");
        }
        assert_eq!(registry.schemas().len(), 4);
        assert!(!registry.get("read").unwrap().needs_approval());
        for name in ["write", "edit", "bash"] {
            assert!(registry.get(name).unwrap().needs_approval(), "{name}");
        }
    }

    #[test]
    fn only_keeps_the_named_tools_and_their_schemas() {
        let mut registry = default_registry();
        registry.only(&["read", "edit"]).unwrap();

        assert_eq!(registry.names(), ["read", "edit"]);
        assert!(registry.get("bash").is_none());
        let schemas = registry.schemas();
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0]["name"], "read");
        assert_eq!(schemas[1]["name"], "edit");
    }

    #[test]
    fn only_rejects_an_unknown_name_and_changes_nothing() {
        let mut registry = default_registry();
        let error = registry.only(&["read", "nope"]).unwrap_err();
        assert!(error.contains("unknown tool `nope`"), "{error}");
        assert_eq!(registry.names().len(), 4);
    }

    #[test]
    fn confine_wraps_file_tools_and_leaves_bash_alone() {
        let dir = testutil::TempDir::new("registry-confine");
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let registry = default_registry().confine(dir.path()).unwrap();
        assert_eq!(registry.names().len(), 4);
        assert!(registry.get("bash").is_some());
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A uniquely named directory under the system temp dir, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "yoke-{tag}-{}-{sequence}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

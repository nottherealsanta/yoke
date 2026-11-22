//! The tool registry; one file per tool.

pub mod bash;
pub mod edit;
pub mod read;
pub mod write;

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

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
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

//! The `write` tool: create or replace a file, making parent directories.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Sink, Tool, ToolResult};

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn schema(&self) -> Value {
        json!({
            "name": "write",
            "description": "Write a file, creating parent directories as needed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to write"},
                    "content": {"type": "string", "description": "Full file contents"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }
        })
    }

    async fn run(&self, args: Value, _out: Sink) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::error("write: missing `path`");
        };
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::error("write: missing `content`");
        };

        let file = Path::new(path);
        if let Some(parent) = file.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    return ToolResult::error(format!("write: {}: {error}", parent.display()));
                }
            }
        }
        if let Err(error) = std::fs::write(file, content) {
            return ToolResult::error(format!("write: {path}: {error}"));
        }
        ToolResult::ok(format!("wrote {} bytes to {path}", content.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;
    use tokio::sync::mpsc;

    async fn write(args: Value) -> ToolResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        Write.run(args, tx).await
    }

    #[tokio::test]
    async fn writes_and_creates_parent_dirs() {
        let dir = TempDir::new("write");
        let file = dir.path().join("a/b/c.txt");

        let result = write(json!({"path": file, "content": "hello"})).await;

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
        assert!(result.output.contains("5 bytes"));
    }

    #[tokio::test]
    async fn missing_content_is_an_error() {
        let dir = TempDir::new("write");
        let result = write(json!({"path": dir.path().join("x")})).await;

        assert!(result.is_error);
        assert!(result.output.contains("content"));
    }

    #[tokio::test]
    async fn needs_approval() {
        assert!(Write.needs_approval());
    }
}

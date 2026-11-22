//! The `read` tool: a text file with line numbers.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Sink, Tool, ToolResult};

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> Value {
        json!({
            "name": "read",
            "description": "Read a text file, with line numbers.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to read"},
                    "offset": {"type": "integer", "description": "First line, 1-based (default 1)"},
                    "limit": {"type": "integer", "description": "Maximum number of lines"}
                },
                "required": ["path"],
                "additionalProperties": false
            }
        })
    }

    async fn run(&self, args: Value, _out: Sink) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::error("read: missing `path`");
        };
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        if offset == 0 {
            return ToolResult::error("read: `offset` is 1-based");
        }

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ToolResult::error(format!("read: {path}: {error}")),
        };

        let mut output = String::new();
        for (index, line) in text
            .lines()
            .enumerate()
            .skip(offset as usize - 1)
            .take(limit.min(usize::MAX as u64) as usize)
        {
            use std::fmt::Write as _;
            let _ = writeln!(output, "{:>6}\t{line}", index + 1);
        }
        ToolResult::ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;
    use tokio::sync::mpsc;

    async fn read(args: Value) -> ToolResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        Read.run(args, tx).await
    }

    #[tokio::test]
    async fn numbers_lines() {
        let dir = TempDir::new("read");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let result = read(json!({"path": file})).await;

        assert!(!result.is_error);
        assert_eq!(result.output, "     1\tone\n     2\ttwo\n     3\tthree\n");
    }

    #[tokio::test]
    async fn honors_offset_and_limit() {
        let dir = TempDir::new("read");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();

        let result = read(json!({"path": file, "offset": 2, "limit": 2})).await;

        assert_eq!(result.output, "     2\ttwo\n     3\tthree\n");
    }

    #[tokio::test]
    async fn errors_on_a_missing_file() {
        let result = read(json!({"path": "/nonexistent/yoke/read"})).await;

        assert!(result.is_error);
        assert!(result.output.contains("read:"));
    }
}

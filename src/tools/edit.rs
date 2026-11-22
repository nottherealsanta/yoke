//! The `edit` tool: exact string replacement.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Sink, Tool, ToolResult};

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn schema(&self) -> Value {
        json!({
            "name": "edit",
            "description": "Replace an exact string in a file. `old_string` must be unique unless `replace_all` is true.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to edit"},
                    "old_string": {"type": "string", "description": "Exact text to replace"},
                    "new_string": {"type": "string", "description": "Replacement text"},
                    "replace_all": {"type": "boolean", "description": "Replace every match (default false)"}
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }
        })
    }

    async fn run(&self, args: Value, _out: Sink) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::error("edit: missing `path`");
        };
        let Some(old) = args.get("old_string").and_then(Value::as_str) else {
            return ToolResult::error("edit: missing `old_string`");
        };
        let Some(new) = args.get("new_string").and_then(Value::as_str) else {
            return ToolResult::error("edit: missing `new_string`");
        };
        if old.is_empty() {
            return ToolResult::error("edit: `old_string` must not be empty");
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ToolResult::error(format!("edit: {path}: {error}")),
        };
        let count = text.matches(old).count();
        if count == 0 {
            return ToolResult::error(format!("edit: `old_string` not found in {path}"));
        }
        if count > 1 && !replace_all {
            return ToolResult::error(format!(
                "edit: `old_string` matches {count} times in {path}; make it unique or pass `replace_all`"
            ));
        }

        let updated = if replace_all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
        if let Err(error) = std::fs::write(path, updated) {
            return ToolResult::error(format!("edit: {path}: {error}"));
        }
        let plural = if count == 1 { "" } else { "s" };
        ToolResult::ok(format!("replaced {count} occurrence{plural} in {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;
    use tokio::sync::mpsc;

    async fn edit(args: Value) -> ToolResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        Edit.run(args, tx).await
    }

    #[tokio::test]
    async fn replaces_a_unique_match() {
        let dir = TempDir::new("edit");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello world").unwrap();

        let result =
            edit(json!({"path": file, "old_string": "world", "new_string": "there"})).await;

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello there");
    }

    #[tokio::test]
    async fn errors_when_nothing_matches() {
        let dir = TempDir::new("edit");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();

        let result = edit(json!({"path": file, "old_string": "nope", "new_string": "x"})).await;

        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn errors_on_multiple_matches() {
        let dir = TempDir::new("edit");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x x").unwrap();

        let result = edit(json!({"path": file, "old_string": "x", "new_string": "y"})).await;

        assert!(result.is_error);
        assert!(result.output.contains("2 times"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x x");
    }

    #[tokio::test]
    async fn replace_all_allows_multiple_matches() {
        let dir = TempDir::new("edit");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x x").unwrap();

        let result = edit(json!({
            "path": file, "old_string": "x", "new_string": "y", "replace_all": true
        }))
        .await;

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "y y");
    }
}

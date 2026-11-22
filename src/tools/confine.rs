//! Confine a tool's `path` argument to a root directory.
//!
//! The wrapper composes with any [`Tool`]: when the arguments
//! contain a string `path`, it is resolved inside the root (relative paths) or
//! checked against it (absolute paths). Symlinks are resolved with
//! `canonicalize`, so an escape through a link is rejected. For a file that
//! does not exist yet, the deepest existing parent is canonicalized and the
//! missing components are appended.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use super::{Sink, Tool, ToolResult};

/// Wrap a tool so its `path` argument cannot leave `root`.
pub struct Confined {
    inner: Box<dyn Tool>,
    root: PathBuf,
}

impl Confined {
    /// `root` must exist; it is canonicalized once here.
    pub fn new(inner: Box<dyn Tool>, root: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self {
            inner,
            root: root.as_ref().canonicalize()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl Tool for Confined {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn needs_approval(&self) -> bool {
        self.inner.needs_approval()
    }

    fn schema(&self) -> Value {
        self.inner.schema()
    }

    async fn run(&self, mut args: Value, out: Sink) -> ToolResult {
        if let Some(Value::String(path)) = args.get("path") {
            match resolve(&self.root, path) {
                Ok(resolved) => args["path"] = Value::String(resolved.to_string_lossy().into()),
                Err(error) => {
                    return ToolResult::error(format!("{}: {error}", self.inner.name()));
                }
            }
        }
        self.inner.run(args, out).await
    }
}

/// Resolve `requested` inside `root`, rejecting anything that escapes it.
///
/// Error text is meant for the model: it says what was rejected and why.
pub fn resolve(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve root {}: {error}", root.display()))?;
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("empty path".into());
    }
    let raw = Path::new(requested);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };

    let mut probe = joined.as_path();
    let mut missing = Vec::new();
    let existing = loop {
        match probe.canonicalize() {
            Ok(path) => break path,
            Err(_) => {
                let Some(name) = probe.file_name() else {
                    return Err(format!(
                        "{requested}: cannot resolve inside {}",
                        root.display()
                    ));
                };
                missing.push(name.to_os_string());
                let Some(parent) = probe.parent() else {
                    return Err(format!(
                        "{requested}: cannot resolve inside {}",
                        root.display()
                    ));
                };
                probe = parent;
            }
        }
    };
    let mut resolved = existing;
    for part in missing.iter().rev() {
        resolved.push(part);
    }

    if resolved == root || resolved.starts_with(&root) {
        // Reject any lexical `..` that survived (for example when the missing
        // suffix contains one), even if it happens to land inside the root.
        if resolved
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!("{requested}: `..` is not allowed"));
        }
        Ok(resolved)
    } else {
        Err(format!("{requested}: outside the root {}", root.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;
    use crate::tools::{Read, Write};
    use serde_json::json;
    use tokio::sync::mpsc;

    async fn run(tool: &dyn Tool, args: Value) -> ToolResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        tool.run(args, tx).await
    }

    #[test]
    fn relative_paths_resolve_inside_the_root() {
        let dir = TempDir::new("confine");
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let resolved = resolve(dir.path(), "a.txt").unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap().join("a.txt"));
    }

    #[test]
    fn missing_files_resolve_through_their_deepest_existing_parent() {
        let dir = TempDir::new("confine");
        let resolved = resolve(dir.path(), "a/b/c.txt").unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(resolved, root.join("a/b/c.txt"));
    }

    #[test]
    fn parent_escapes_are_rejected() {
        let dir = TempDir::new("confine");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let error = resolve(&root, "../x").unwrap_err();
        assert!(error.contains("outside the root"), "{error}");
    }

    #[test]
    fn absolute_paths_outside_are_rejected() {
        let dir = TempDir::new("confine");
        let error = resolve(dir.path(), "/etc/passwd").unwrap_err();
        assert!(error.contains("outside the root"), "{error}");
    }

    #[test]
    fn absolute_paths_inside_are_accepted() {
        let dir = TempDir::new("confine");
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let absolute = dir.path().canonicalize().unwrap().join("a.txt");
        assert_eq!(
            resolve(dir.path(), absolute.to_str().unwrap()).unwrap(),
            absolute
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escapes_are_rejected() {
        let dir = TempDir::new("confine");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let error = resolve(&root, "link/secret").unwrap_err();
        assert!(error.contains("outside the root"), "{error}");
    }

    #[tokio::test]
    async fn the_wrapper_passes_a_confined_path_to_the_inner_tool() {
        let dir = TempDir::new("confine");
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let tool = Confined::new(Box::new(Read), dir.path()).unwrap();

        let result = run(&tool, json!({"path": "a.txt"})).await;

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("one"));

        let write = Confined::new(Box::new(Write), dir.path()).unwrap();
        let result = run(&write, json!({"path": "new.txt", "content": "hi"})).await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hi"
        );
    }

    #[tokio::test]
    async fn escapes_become_ordinary_error_results() {
        let dir = TempDir::new("confine");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let tool = Confined::new(Box::new(Read), &root).unwrap();

        let result = run(&tool, json!({"path": "../secret"})).await;

        assert!(result.is_error);
        assert!(result.output.starts_with("read: "), "{}", result.output);
        assert!(
            result.output.contains("outside the root"),
            "{}",
            result.output
        );
    }
}

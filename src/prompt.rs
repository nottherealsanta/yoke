//! The system prompt: instructions, workspace, date and the project memory.

use std::path::Path;

/// Build the system prompt. `memory` is the project's memory file text, which
/// is included verbatim under `# Memory` even when empty.
pub fn build(workspace: &Path, date: &str, memory_path: &Path, memory: &str) -> String {
    format!(
        "You are Yoke, a coding agent. Work in the workspace below and keep your answers short.\n\
         \n\
         Workspace: {}\n\
         Today: {date}\n\
         \n\
         Use the read, write, edit and bash tools to inspect and change files.\n\
         \n\
         # Memory\n\
         \n\
         Memory file: {}\n\
         Keep it short: project facts and user preferences, not a log. Update it with the edit and write tools.\n\
         \n\
         {memory}",
        workspace.display(),
        memory_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn prompt(memory: &str) -> String {
        build(
            Path::new("/work/yoke"),
            "2026-11-22",
            &PathBuf::from("/home/u/.yoke/projects/-work-yoke/memory.md"),
            memory,
        )
    }

    #[test]
    fn includes_workspace_date_path_and_memory_rule() {
        let prompt = prompt("prefers tabs\n");
        assert!(prompt.contains("Workspace: /work/yoke"));
        assert!(prompt.contains("Today: 2026-11-22"));
        assert!(prompt.contains("/home/u/.yoke/projects/-work-yoke/memory.md"));
        assert!(prompt.contains("not a log"));
        assert!(prompt.contains("edit and write tools"));
    }

    #[test]
    fn memory_sits_under_its_heading() {
        let prompt = prompt("the user prefers small diffs\n");
        let heading = prompt.find("# Memory").unwrap();
        let text = prompt.find("the user prefers small diffs").unwrap();
        assert!(heading < text);
    }

    #[test]
    fn empty_memory_still_shows_the_heading() {
        assert!(prompt("").contains("# Memory"));
    }
}

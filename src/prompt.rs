//! The system prompt: instructions, workspace, date, tools and project memory.

use std::path::Path;

/// Build the system prompt.
///
/// `tools` is the enabled tool list (so a restricted registry is visible to
/// the model). `memory` is the file path and text; passing `None` (with
/// `--no-memory`) leaves the memory section out entirely.
pub fn build(
    workspace: &Path,
    date: &str,
    tools: &[String],
    memory: Option<(&Path, &str)>,
) -> String {
    let tools_line = if tools.is_empty() {
        "No tools are enabled.".to_string()
    } else {
        format!(
            "Use the {} tools to inspect and change files.",
            tools.join(", ")
        )
    };

    let mut prompt = format!(
        "You are Yoke, a coding agent. Work in the workspace below and keep your answers short.\n\
         \n\
         Workspace: {}\n\
         Today: {date}\n\
         \n\
         {tools_line}",
        workspace.display()
    );

    if let Some((path, text)) = memory {
        prompt.push_str(&format!(
            "\n\
             \n\
             # Memory\n\
             \n\
             Memory file: {}\n\
             Keep it short: project facts and user preferences, not a log. Update it with the edit and write tools.\n\
             \n\
             {text}",
            path.display()
        ));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tools() -> Vec<String> {
        ["read", "write", "edit", "bash"].map(String::from).to_vec()
    }

    fn prompt(memory: &str) -> String {
        build(
            Path::new("/work/yoke"),
            "2026-11-22",
            &tools(),
            Some((
                &PathBuf::from("/home/u/.yoke/projects/-work-yoke/memory.md"),
                memory,
            )),
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
    fn lists_only_the_enabled_tools() {
        let prompt = build(
            Path::new("/work"),
            "2026-11-22",
            &["read".to_string(), "edit".to_string()],
            None,
        );
        assert!(prompt.contains("Use the read, edit tools"));
        assert!(!prompt.contains("bash"));
    }

    #[test]
    fn no_memory_means_no_section_and_no_path() {
        let prompt = build(Path::new("/work"), "2026-11-22", &tools(), None);
        assert!(!prompt.contains("# Memory"));
        assert!(!prompt.contains("memory file"));
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
        let prompt = build(
            Path::new("/work"),
            "2026-11-22",
            &tools(),
            Some((Path::new("/tmp/memory.md"), "")),
        );
        assert!(prompt.contains("# Memory"));
    }
}

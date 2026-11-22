//! Per-project memory file and Yoke's state directory.

use std::path::{Path, PathBuf};

/// Yoke's state directory: a non-empty `YOKE_HOME`, else `~/.yoke`.
pub fn home(env_home: Option<&str>) -> Option<PathBuf> {
    match env_home {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => dirs::home_dir().map(|home| home.join(".yoke")),
    }
}

/// The project slug: the absolute project path with `/` replaced by `-`.
pub fn slug(project: &Path) -> String {
    project.to_string_lossy().replace('/', "-")
}

/// Everything Yoke keeps for one project.
pub fn project_dir(home: &Path, slug: &str) -> PathBuf {
    home.join("projects").join(slug)
}

/// The project's memory file: `<home>/projects/<slug>/memory.md`.
pub fn memory_path(home: &Path, slug: &str) -> PathBuf {
    project_dir(home, slug).join("memory.md")
}

/// The memory text, or an empty string when the file does not exist.
pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;

    #[test]
    fn slug_replaces_separators() {
        assert_eq!(slug(Path::new("/Users/a/b")), "-Users-a-b");
        assert_eq!(slug(Path::new("/")), "-");
    }

    #[test]
    fn yoke_home_overrides_the_default() {
        assert_eq!(home(Some("/tmp/x")), Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn empty_yoke_home_falls_back_to_the_real_home() {
        assert_ne!(home(Some("")), Some(PathBuf::new()));
    }

    #[test]
    fn memory_path_lives_under_the_project() {
        let home = Path::new("/home/u/.yoke");
        assert_eq!(
            memory_path(home, "-work-x"),
            PathBuf::from("/home/u/.yoke/projects/-work-x/memory.md")
        );
    }

    #[test]
    fn missing_memory_reads_as_empty() {
        let home = TempDir::new("memory");
        assert_eq!(read(&memory_path(home.path(), "-missing")), "");
    }

    #[test]
    fn memory_round_trips() {
        let home = TempDir::new("memory");
        let path = memory_path(home.path(), "-work-x");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "uses tabs\n").unwrap();
        assert_eq!(read(&path), "uses tabs\n");
    }
}

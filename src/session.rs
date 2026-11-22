//! Save and load a session as JSONL, one message per line.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::clock;
use crate::memory;
use crate::types::Message;

/// An open session file; every appended message is visible on disk at once.
pub struct Session {
    path: PathBuf,
    file: File,
}

impl Session {
    /// Create a new session named with the current UTC time.
    pub fn create(home: &Path, slug: &str) -> Result<Self> {
        Self::create_at(home, slug, &clock::timestamp(SystemTime::now()))
    }

    fn create_at(home: &Path, slug: &str, timestamp: &str) -> Result<Self> {
        let dir = sessions_dir(home, slug);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create sessions dir {}", dir.display()))?;
        // Two starts (or `/clear`) within one second must not share a file.
        let mut path = dir.join(format!("{timestamp}.jsonl"));
        let mut suffix = 1;
        let file = loop {
            match OpenOptions::new().create_new(true).append(true).open(&path) {
                Ok(file) => break file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    path = dir.join(format!("{timestamp}-{suffix}.jsonl"));
                    suffix += 1;
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("open session {}", path.display()));
                }
            }
        };
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one message as a single JSON line and flush it.
    pub fn append(&mut self, message: &Message) -> Result<()> {
        let line = serde_json::to_string(message)?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        Ok(())
    }
}

fn sessions_dir(home: &Path, slug: &str) -> PathBuf {
    memory::project_dir(home, slug).join("sessions")
}

/// Read a session file back into messages. Blank lines are ignored.
pub fn load(path: &Path) -> Result<Vec<Message>> {
    let file = File::open(path).with_context(|| format!("open session {}", path.display()))?;
    let mut messages = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read session {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let message = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: bad message", path.display(), index + 1))?;
        messages.push(message);
    }
    Ok(messages)
}

/// The newest session file for a project, if any.
pub fn latest(home: &Path, slug: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(sessions_dir(home, slug)).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;
    use crate::types::{Part, Role};
    use serde_json::json;

    fn message(role: Role, part: Part) -> Message {
        Message {
            role,
            parts: vec![part],
        }
    }

    #[test]
    fn append_then_load_round_trips() {
        let home = TempDir::new("session");
        let mut session = Session::create_at(home.path(), "-work-x", "20261122T101500Z").unwrap();
        assert_eq!(
            session.path(),
            home.path()
                .join("projects/-work-x/sessions/20261122T101500Z.jsonl")
        );

        let messages = vec![
            message(Role::User, Part::Text { text: "hi".into() }),
            message(
                Role::Assistant,
                Part::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    args: json!({"path": "a.rs"}),
                },
            ),
            message(
                Role::Tool,
                Part::ToolResult {
                    id: "c1".into(),
                    output: "ok".into(),
                    is_error: false,
                },
            ),
        ];
        for message in &messages {
            session.append(message).unwrap();
        }

        assert_eq!(load(session.path()).unwrap(), messages);
    }

    #[test]
    fn latest_picks_the_newest_name() {
        let home = TempDir::new("session");
        for stamp in ["20250101T000000Z", "20261122T101500Z", "20260301T080000Z"] {
            Session::create_at(home.path(), "-work-x", stamp).unwrap();
        }
        let latest = latest(home.path(), "-work-x").unwrap();
        assert_eq!(
            latest,
            home.path()
                .join("projects/-work-x/sessions/20261122T101500Z.jsonl")
        );
    }

    #[test]
    fn sessions_created_in_the_same_second_get_distinct_files() {
        let home = TempDir::new("session");
        let first = Session::create_at(home.path(), "-work-x", "20261122T101500Z").unwrap();
        let second = Session::create_at(home.path(), "-work-x", "20261122T101500Z").unwrap();

        assert_ne!(first.path(), second.path());
        assert_eq!(
            second.path(),
            home.path()
                .join("projects/-work-x/sessions/20261122T101500Z-1.jsonl")
        );
    }

    #[test]
    fn missing_sessions_dir_has_no_latest() {
        let home = TempDir::new("session");
        assert_eq!(latest(home.path(), "-never-ran"), None);
    }
}

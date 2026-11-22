//! The `bash` tool: timeout, live output and truncation.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, ChildStdout, Command};

use super::{Sink, Tool, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_OUTPUT_LINES: usize = 2000;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn needs_approval(&self) -> bool {
        true
    }

    fn schema(&self) -> Value {
        json!({
            "name": "bash",
            "description": "Run a shell command. Output is streamed live and truncated if very long.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command passed to `sh -c`"},
                    "timeout": {"type": "integer", "description": "Seconds before the process is killed (default 120)"}
                },
                "required": ["command"],
                "additionalProperties": false
            }
        })
    }

    async fn run(&self, args: Value, out: Sink) -> ToolResult {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::error("bash: missing `command`");
        };
        let timeout_secs = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .max(1);

        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return ToolResult::error(format!("bash: failed to start: {error}")),
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");

        let mut output = String::new();
        let timed_out = match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            read_streams(&mut stdout, &mut stderr, &out, &mut output),
        )
        .await
        {
            Ok(Ok(())) => false,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return ToolResult::error(format!("bash: {error}\n{}", truncate(&output)));
            }
            Err(_) => {
                let _ = child.kill().await;
                true
            }
        };
        let status = match child.wait().await {
            Ok(status) => status,
            Err(error) => {
                return ToolResult::error(format!("bash: {error}\n{}", truncate(&output)))
            }
        };
        let output = truncate(&output);

        if timed_out {
            return ToolResult::error(format!("bash: timed out after {timeout_secs}s\n{output}"));
        }
        if status.success() {
            ToolResult::ok(output)
        } else {
            let code = status.code().map_or_else(
                || "terminated by signal".to_string(),
                |code| code.to_string(),
            );
            ToolResult::error(format!("bash: exit code {code}\n{output}"))
        }
    }
}

async fn read_streams(
    stdout: &mut ChildStdout,
    stderr: &mut ChildStderr,
    out: &Sink,
    output: &mut String,
) -> std::io::Result<()> {
    let mut out_buf = [0u8; 4096];
    let mut err_buf = [0u8; 4096];
    let (mut out_open, mut err_open) = (true, true);

    while out_open || err_open {
        tokio::select! {
            read = stdout.read(&mut out_buf), if out_open => match read? {
                0 => out_open = false,
                n => push(out, output, &out_buf[..n]),
            },
            read = stderr.read(&mut err_buf), if err_open => match read? {
                0 => err_open = false,
                n => push(out, output, &err_buf[..n]),
            },
        }
    }
    Ok(())
}

fn push(out: &Sink, output: &mut String, bytes: &[u8]) {
    let chunk = String::from_utf8_lossy(bytes).into_owned();
    output.push_str(&chunk);
    let _ = out.send(chunk);
}

/// Keep the first and last `MAX_OUTPUT_LINES / 2` lines, with a visible marker.
fn truncate(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= MAX_OUTPUT_LINES {
        return output.to_string();
    }
    let keep = MAX_OUTPUT_LINES / 2;
    let omitted = lines.len() - keep * 2;
    format!(
        "{}\n[truncated {omitted} lines]\n{}",
        lines[..keep].join("\n"),
        lines[lines.len() - keep..].join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    async fn bash(args: Value) -> ToolResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        Bash.run(args, tx).await
    }

    #[tokio::test]
    async fn runs_a_command() {
        let result = bash(json!({"command": "printf 'hello\\n'"})).await;

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(result.output, "hello\n");
    }

    #[tokio::test]
    async fn reports_a_nonzero_exit() {
        let result = bash(json!({"command": "echo bad >&2; exit 3"})).await;

        assert!(result.is_error);
        assert!(result.output.contains("exit code 3"));
        assert!(result.output.contains("bad"));
    }

    #[tokio::test]
    async fn times_out_and_kills_the_process() {
        let started = std::time::Instant::now();
        let result = bash(json!({"command": "sleep 30", "timeout": 1})).await;

        assert!(result.is_error);
        assert!(result.output.contains("timed out after 1s"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn truncates_long_output_with_a_marker() {
        let result = bash(json!({
            "command": "i=0; while [ $i -lt 2100 ]; do echo line$i; i=$((i+1)); done"
        }))
        .await;

        assert!(!result.is_error);
        assert!(result.output.starts_with("line0\n"));
        assert!(result.output.contains("[truncated 100 lines]"));
        assert!(result.output.ends_with("line2099"));
    }

    #[tokio::test]
    async fn streams_chunks_while_running() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = tokio::spawn(async move {
            Bash.run(
                json!({"command": "printf first; sleep 0.3; printf second"}),
                tx,
            )
            .await
        });

        let first = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("first chunk arrives before the process exits")
            .unwrap();
        let second = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("second chunk")
            .unwrap();
        let result = tool.await.unwrap();

        assert_eq!(first, "first");
        assert_eq!(second, "second");
        assert_eq!(result.output, "firstsecond");
    }
}

//! A minimal embedding: a custom `clock` tool, the real loop and a provider
//! chosen from the environment. Run it with a key set, for example:
//!
//! ```sh
//! GROQ_API_KEY=... cargo run --example minimal -- "what time is it?"
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use yoke::agent::{self, CancelToken};
use yoke::providers;
use yoke::tools::{Registry, Sink, Tool, ToolResult};
use yoke::types::{Message, Part, Role};

struct Clock;

#[async_trait]
impl Tool for Clock {
    fn name(&self) -> &str {
        "clock"
    }

    fn schema(&self) -> Value {
        json!({
            "name": "clock",
            "description": "The current Unix time in seconds.",
            "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
        })
    }

    async fn run(&self, _args: Value, out: Sink) -> ToolResult {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let text = format!("{secs}");
        let _ = out.send(text.clone());
        ToolResult::ok(text)
    }
}

fn main() -> anyhow::Result<()> {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "what time is it?".into());
    let id = std::env::var("YOKE_PROVIDER").unwrap_or_else(|_| "groq".into());
    let model =
        std::env::var("YOKE_MODEL").unwrap_or_else(|_| providers::default_model(&id).into());
    let home =
        yoke::memory::home(std::env::var("YOKE_HOME").ok().as_deref()).expect("home directory");
    let provider = providers::make(&id, &model, None, &home, |name| std::env::var(name).ok())?;

    let mut registry = Registry::new();
    registry.register(Box::new(Clock));
    let mut messages = vec![Message {
        role: Role::User,
        parts: vec![Part::Text { text: prompt }],
    }];
    let options = agent::Options::new(model, "You are a small agent. Keep it short.");
    let cancel = CancelToken::new();

    yoke::tokio::runtime::Runtime::new()?.block_on(async {
        let (tx, mut rx) = agent::event_channel();
        let pump = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        agent::run(
            provider.as_ref(),
            &registry,
            &options,
            &mut messages,
            &cancel,
            &tx,
        )
        .await?;
        drop(tx);
        pump.await?;
        anyhow::Ok(())
    })?;

    for message in &messages {
        for part in &message.parts {
            match part {
                Part::Text { text } => println!("{}: {text}", role(&message.role)),
                Part::ToolCall { name, args, .. } => println!("  {name} {args}"),
                Part::ToolResult { output, .. } => println!("  → {output}"),
                _ => {}
            }
        }
    }
    Ok(())
}

fn role(role: &Role) -> &'static str {
    match role {
        Role::User => "you",
        Role::Assistant => "yoke",
        Role::Tool => "tool",
    }
}

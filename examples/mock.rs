//! A fully offline agent: the mock provider scripts one tool call, the loop
//! runs it, and a second scripted turn answers. `cargo run --example mock`.

use async_trait::async_trait;
use serde_json::{json, Value};
use yoke::agent::{self, CancelToken, Event};
use yoke::providers::mock::MockProvider;
use yoke::tools::{Registry, Sink, Tool, ToolResult};
use yoke::types::{Message, Part, Role, StreamEvent};

struct Weather;

#[async_trait]
impl Tool for Weather {
    fn name(&self) -> &str {
        "weather"
    }

    fn schema(&self) -> Value {
        json!({
            "name": "weather",
            "description": "Weather for a city (offline demo).",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        })
    }

    async fn run(&self, args: Value, out: Sink) -> ToolResult {
        let city = args
            .get("city")
            .and_then(Value::as_str)
            .unwrap_or("somewhere");
        let text = format!("{city}: sunny, 21C");
        let _ = out.send(text.clone());
        ToolResult::ok(text)
    }
}

fn main() -> anyhow::Result<()> {
    let call = Part::ToolCall {
        id: "call-1".into(),
        name: "weather".into(),
        args: json!({"city": "Lisbon"}),
    };
    let provider = MockProvider::new(vec![
        vec![
            StreamEvent::PartDone { part: call },
            StreamEvent::Done {
                stop: "tool_calls".into(),
            },
        ],
        vec![
            StreamEvent::TextDelta {
                text: "It is sunny in Lisbon.\n".into(),
            },
            StreamEvent::Done {
                stop: "end_turn".into(),
            },
        ],
    ]);

    let mut registry = Registry::new();
    registry.register(Box::new(Weather));
    let mut messages = vec![Message {
        role: Role::User,
        parts: vec![Part::Text {
            text: "weather in Lisbon?".into(),
        }],
    }];
    let options = agent::Options::new("mock", "Use the weather tool.");
    let cancel = CancelToken::new();

    yoke::tokio::runtime::Runtime::new()?.block_on(async {
        let (tx, mut rx) = agent::event_channel();
        let pump = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    Event::Stream(StreamEvent::TextDelta { text }) => print!("{text}"),
                    Event::ToolStart { name, args, .. } => println!("▶ {name} {args}"),
                    Event::ToolOutput { chunk, .. } => print!("{chunk}"),
                    Event::ToolEnd { status, .. } => println!("  ✓ {status:?}"),
                    Event::Notice(text) => println!("! {text}"),
                    _ => {}
                }
            }
        });
        agent::run(&provider, &registry, &options, &mut messages, &cancel, &tx).await?;
        drop(tx);
        pump.await?;
        anyhow::Ok(())
    })?;
    Ok(())
}

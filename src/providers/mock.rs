//! Mock provider: replays scripted turns. Used by tests and `YOKE_DEV=1`.

use std::sync::Mutex;

use anyhow::Result;
use futures::stream::BoxStream;
use serde_json::{json, Value};

use super::Provider;
use crate::types::{Request, StreamEvent};

/// One scripted turn per model call, in order. Exhausted turns end the turn.
pub struct MockProvider {
    turns: Vec<Vec<StreamEvent>>,
    next: Mutex<usize>,
}

impl MockProvider {
    pub fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            turns,
            next: Mutex::new(0),
        }
    }

    /// The `YOKE_DEV=1` default: one short text reply.
    pub fn dev() -> Self {
        Self::new(vec![vec![
            StreamEvent::TextDelta {
                text: "hello".into(),
            },
            StreamEvent::Done {
                stop: "end_turn".into(),
            },
        ]])
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn body(&self, req: &Request) -> Value {
        json!({
            "provider": "mock",
            "model": req.model,
            "instructions": req.instructions,
            "messages": req.messages,
            "tools": req.tools,
        })
    }

    async fn stream(&self, _req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let turn = {
            let mut next = self.next.lock().unwrap();
            let turn = self.turns.get(*next).cloned();
            *next += 1;
            turn
        };
        let events = turn.unwrap_or_else(|| {
            vec![StreamEvent::Done {
                stop: "end_turn".into(),
            }]
        });
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

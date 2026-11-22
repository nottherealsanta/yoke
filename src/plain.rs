//! The throwaway line-mode REPL behind `yoke --plain`.
//!
//! It exists so the loop is usable before the TUI lands. Input is a line on
//! stdin, output is the loop's events as they arrive.

use std::collections::HashMap;
use std::io::{self, Write};

use anyhow::Result;
use tokio::sync::mpsc;

use crate::providers::Provider;
use crate::r#loop::{CancelToken, Event, Options};
use crate::session::Session;
use crate::tools::Registry;
use crate::types::{Message, Part, Role, StreamEvent};

pub struct Plain {
    pub provider: Box<dyn Provider>,
    pub registry: Registry,
    pub options: Options,
    pub messages: Vec<Message>,
    pub session: Session,
}

impl Plain {
    pub fn run(&mut self) -> Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;
        loop {
            print!("> ");
            io::stdout().flush()?;
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 {
                println!();
                return Ok(());
            }
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if text == "/quit" || text == "/exit" {
                return Ok(());
            }

            let message = Message {
                role: Role::User,
                parts: vec![Part::Text {
                    text: text.to_string(),
                }],
            };
            self.messages.push(message.clone());
            self.session.append(&message)?;
            self.turn(&runtime)?;
        }
    }

    fn turn(&mut self, runtime: &tokio::runtime::Runtime) -> Result<()> {
        let cancel = CancelToken::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let Plain {
            provider,
            registry,
            options,
            messages,
            session,
        } = self;
        let mut output = Output::default();
        let mut names = HashMap::new();

        runtime.block_on(async {
            let mut running = Box::pin(crate::r#loop::run(
                &**provider,
                registry,
                options,
                messages,
                &cancel,
                &tx,
            ));
            loop {
                tokio::select! {
                    biased;
                    result = &mut running => {
                        while let Ok(event) = rx.try_recv() {
                            output.handle(event, session, &mut names)?;
                        }
                        result?;
                        return Ok(());
                    }
                    event = rx.recv() => {
                        if let Some(event) = event {
                            output.handle(event, session, &mut names)?;
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        cancel.cancel();
                    }
                }
            }
        })
    }
}

struct Output {
    line_start: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self { line_start: true }
    }
}

impl Output {
    fn handle(
        &mut self,
        event: Event,
        session: &mut Session,
        names: &mut HashMap<String, String>,
    ) -> Result<()> {
        match event {
            Event::StepStart { .. } => {}
            Event::Stream(StreamEvent::TextDelta { text }) => self.text(&text)?,
            Event::Stream(StreamEvent::ReasoningDelta { .. }) => {}
            Event::Stream(
                StreamEvent::PartDone { .. }
                | StreamEvent::Usage(_)
                | StreamEvent::Done { .. }
                | StreamEvent::Wire(_),
            ) => {}
            Event::MessageAdded(message) => session.append(&message)?,
            Event::ToolStart { id, name, args } => {
                self.line(&format!("▶ {name} {args}"))?;
                names.insert(id, name);
            }
            Event::ToolOutput { chunk, .. } => self.text(&chunk)?,
            Event::ToolEnd { id, status, .. } => {
                let name = names.remove(&id).unwrap_or(id);
                match status {
                    crate::tools::ToolStatus::Ok => self.line(&format!("✓ {name}"))?,
                    status => self.line(&format!("✗ {name} ({status:?})"))?,
                }
            }
            Event::ApprovalRequest(request) => {
                self.line(&format!("Allow {} {}? [y/N]", request.name, request.args))?;
                print!("> ");
                io::stdout().flush()?;
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                let yes = matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
                let _ = request.reply.send(yes);
                self.line_start = true;
            }
            Event::Notice(notice) => self.line(&format!("! {notice}"))?,
            Event::TurnEnd { .. } => self.end_line()?,
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<()> {
        print!("{text}");
        io::stdout().flush()?;
        self.line_start = text.ends_with('\n');
        Ok(())
    }

    fn line(&mut self, text: &str) -> Result<()> {
        if !self.line_start {
            println!();
        }
        println!("{text}");
        self.line_start = true;
        io::stdout().flush()?;
        Ok(())
    }

    fn end_line(&mut self) -> Result<()> {
        if !self.line_start {
            println!();
            self.line_start = true;
            io::stdout().flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::TempDir;

    #[test]
    fn output_starts_at_a_line_start_and_keeps_track() {
        let output = Output::default();
        assert!(output.line_start);
    }

    #[test]
    fn session_appending_is_the_only_side_effect_we_test() {
        let home = TempDir::new("plain");
        let mut session = Session::create(home.path(), "-work-x").unwrap();
        let message = Message {
            role: Role::User,
            parts: vec![Part::Text { text: "hi".into() }],
        };
        session.append(&message).unwrap();
        assert_eq!(crate::session::load(session.path()).unwrap(), vec![message]);
    }
}

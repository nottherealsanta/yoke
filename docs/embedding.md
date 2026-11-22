# Embedding Yoke

Yoke is a library first. The `yoke` binary — the TUI and the `--plain` REPL —
is one consumer of the same public API your agent can use. The web demo is
another.

```text
types ──▶ providers (wire dialects)     tools (read/write/edit/bash, yours)
   │            │                              │
   └────────────┴──────────▶ agent::run ◀──────┘
                                  │ events
                    ┌─────────────┼─────────────┐
                  TUI         --plain      your agent
```

## The loop contract

`yoke::agent::run` is one turn: model → tool calls → results → model, until the
model stops calling tools.

```rust
pub async fn run(
    provider: &dyn Provider,
    tools: &Registry,
    options: &Options,
    messages: &mut Vec<Message>,
    cancel: &CancelToken,
    events: &EventSender,
) -> anyhow::Result<()>
```

- **Events in, messages out.** The loop appends every assistant message and
  tool result to `messages` (the full context, resent each step) and emits
  `Event`s for the UI. It owns no terminal and no storage.
- The caller appends the new user message before calling. `Options` carries the
  model, system prompt, optional `conversation_id` and step limit.
- `Event::ApprovalRequest` carries a `tokio::sync::oneshot::Sender<bool>`.
  Dropping the sender counts as a denial. `--yes` maps to `Options::yes`.
- `CancelToken::cancel()` kills the running tool; the interrupted result stays
  in the context. The next turn can reuse a fresh token.
- `agent::event_channel()` creates the channel. `yoke` re-exports `tokio` for
  convenience, but any `tokio::sync::mpsc::UnboundedSender<Event>` works.

`examples/minimal.rs` is a 60-line agent with a custom tool and a real provider;
`examples/mock.rs` does a tool call fully offline.

## Implementing a Provider

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn body(&self, req: &Request) -> Value;   // exact wire body, shown by mode 3
    async fn stream(&self, req: &Request)
        -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>>;
}
```

Emit `TextDelta` / `ReasoningDelta` as they arrive, then `PartDone` for tool
calls and reasoning items, `Usage`, and `Done { stop }`. Reuse the shared
dialects in `providers::chat` and `providers::responses` rather than writing a
new wire format. `providers::make` builds the built-in providers;
`providers::mock::MockProvider` replays scripted turns and needs no network.

## Implementing a Tool

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn needs_approval(&self) -> bool { false }
    fn schema(&self) -> Value;
    async fn run(&self, args: Value, out: Sink) -> ToolResult;
}
```

- `needs_approval` makes the loop ask before running it (write, edit, bash).
- `Sink` is an unbounded sender of live output chunks; the loop forwards them
  as `Event::ToolOutput`. Return the final text in `ToolResult`.
- Errors go back to the model as ordinary tool results.
- `Registry::new()` plus `register` is a custom registry;
  `tools::default_registry()` is the four built-ins.
- `Registry::only(&["read", "write"])` restricts it (unknown names error), and
  the loop sends exactly those schemas.
- `Registry::confine(root)` wraps every tool whose arguments carry a string
  `path`: relative paths resolve inside `root`, absolute paths must be inside,
  and `..`/symlink escapes are rejected. This composes with custom tools.

## Configuration

`yoke::Config` bundles the safety options the CLI exposes:

```rust
let config = yoke::Config {
    tools: Some(vec!["read".into(), "write".into()]),
    root: Some(workspace.clone()),
    memory: false,
};
let registry = config.registry()?;
let prompt = config.instructions(&config.workspace(&cwd)?, &date, &registry.names(), None);
```

`--root` only confines the file tools; `bash` is not confined. The CLI warns
when both are enabled.

## Sessions, memory and context

- `memory::home(Some("YOKE_HOME"))` / `memory::slug(&workspace)` — state paths.
- `memory::read` / `memory_path` — the per-project Markdown file.
- `session::Session::create` + `append` — JSONL on disk; `session::load`,
  `session::latest` for resume.
- `context::Sections::new(system, memory, tools, messages)` — per-section token
  estimates; `Sections::used(last_usage)` prefers real usage.
- `prompt::build(workspace, date, &tool_names, memory)` — the system prompt.
  Memory is `None` under `--no-memory`, and then it is absent from the prompt
  and from the UI, not merely empty.

## Stability

Pre-1.0. The shape above is deliberate and changes are meant to stay small and
readable in one sitting, but names and signatures may still move. The TUI and
plain REPL are currently ungated (ratatui/crossterm are plain dependencies).

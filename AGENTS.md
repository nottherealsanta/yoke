# AGENTS.md

Yoke is a small agent harness, written entirely in Rust, that gives the user a **one-to-one view of
the agent's context**: what the model receives is what the user sees, in order, nothing hidden and
nothing extra. It is inspired by [Nexus](../nexus) (read its `docs/llm-providers.md`, `docs/loop.md`
and `docs/auth.md` for prior art), but it is deliberately much smaller: one binary, one process, one
loop, one memory file per project.

If a change makes Yoke harder to read in one sitting, it is probably the wrong change.

## Principles

- **One timeline = the context.** Every message, tool call, tool result, the system prompt, the
  memory file and the tool schemas are in the timeline in the order the model gets them. The UI
  never shows something the model does not have, and never hides something it does (it may
  *fold* it, and folds are always openable).
- **The loop is simple.** Model → tool calls → results → model, until the model stops calling tools.
  No daemon, no HTTP API, no subagents, no hooks, no plugins. The TUI drives the loop in-process.
- **Show tool calls.** Every tool call and its output is visible, live, as it runs.
- **Rust first, Python where it pays.** The harness (loop, tools, TUI, HTTP providers) is Rust.
  Python is allowed for a provider whose only good client is a Python SDK; today that is `claude`,
  which runs the Claude Agent SDK in a small worker script (see below). Python is managed with `uv`
  (never `pip install`), and the Rust side talks to it over stdin/stdout JSON lines.
- **Few dependencies.** `tokio`, `reqwest` (streaming, rustls), `serde`/`serde_json`, `ratatui`,
  `crossterm`, `anyhow`/`thiserror`, `dirs`. Ask before adding anything else.

## Library first

Yoke is an embeddable Rust library; the CLI is a consumer. `src/lib.rs` exposes
the public API (`yoke::types`, `yoke::agent`, `yoke::providers`, `yoke::tools`,
`yoke::prompt`, `yoke::memory`, `yoke::session`, `yoke::context`, `yoke::auth`,
`yoke::models`, `yoke::Config`). The consumers are the TUI (`yoke::tui`), the
`--plain` REPL (`yoke::plain`) and the web demo someone else builds under
`demo/`; all three drive `yoke::agent::run` and render its events. The TUI and
plain REPL are not feature-gated yet (keeping them ungated is simpler today).

`examples/minimal.rs` is a real provider plus a custom tool; `examples/mock.rs`
runs a tool call fully offline. `docs/embedding.md` documents the contract.

Safety options shared by the CLI and embedders live in `Config`:

- `--tools read,write,edit` restricts the registry (unknown names error, and
  the prompt and wire schemas show only the kept tools).
- `--root DIR` confines `read`/`write`/`edit` to DIR with `Registry::confine`;
  relative paths resolve inside it, `..` and symlink escapes are rejected.
  `Workspace:` in the prompt becomes DIR. `bash` is **not** confined; the CLI
  warns on stderr when both are enabled.
- `--no-memory` removes the memory section and file path from the prompt and
  the memory row from the context header; `/memory` reports that it is off.

## Layout

One crate, binary `yoke`.

```
src/
  lib.rs           The public API: `pub mod`s, `Config`, crate docs and a doc-test
  main.rs          CLI entry: `yoke`, `yoke login <provider>`, `yoke logout <provider>`,
                   `yoke status`; parses flags and hands off to the consumers
  config.rs        Config: --tools/--root/--no-memory, registry + prompt helpers
  types.rs         Conversation (Message, Part: Text | Reasoning | ToolCall | ToolResult | Image),
                   Request, StreamEvent, Usage
  loop.rs          The agent loop (below), exported as `yoke::agent`. Owns no UI and no storage
  plain.rs         Throwaway line-mode REPL behind `yoke --plain`
  tools/           One file per tool: read.rs, write.rs, edit.rs, bash.rs; mod.rs holds the
                   registry; confine.rs wraps tools with a path root
  memory.rs        The per-project memory file
  models.rs        Model catalog (models.dev + cache + fallbacks) and the config writer
  prompt.rs        The system prompt: instructions, workspace, date, tools, memory
  context.rs       Per-section token estimates, real usage and per-model context windows
  clock.rs         UTC date and session timestamp from SystemTime, no crate
  session.rs       Save/load a session as JSONL (one Message per line)
  dotenv.rs        Parser for the cwd's `.env` (API keys); pure, no crate
  auth.rs          Credential store (~/.yoke/auth.json, 0600) and the Auth trait
  providers/
    mod.rs         Provider trait, PROVIDERS table, make(id, model)
    sse.rs         Server-sent events parser (shared)
    responses.rs   OpenAI Responses dialect (shared by codex, grok, openai)
    chat.rs        OpenAI Chat Completions dialect (shared by copilot, opencode, groq, compat)
    codex.rs       ChatGPT (Codex) — OAuth PKCE / device code
    grok.rs        xAI (SuperGrok) — device code
    claude.rs      Claude — spawns the Python Agent SDK worker
    claude_worker.py  The worker (uv script, inline deps: claude-agent-sdk)
    mock.rs        Scripted turns for tests and `YOKE_DEV=1`; always public for embedders
    copilot.rs     GitHub Copilot — device code
    opencode.rs    OpenCode Go — API key
    openai.rs      OpenAI — API key
    groq.rs        Groq — API key
    compat.rs      OpenAI-compatible server — base URL + optional API key
  tui/
    mod.rs         App state, event loop, key handling, mode dispatch and draw
    picker.rs      The inline `/model` picker (groups, filter, selection, rows)
    timeline.rs    `Timeline`/`Entry`, built only from loop events
    overview.rs    Mode 1
    tools.rs       Mode 2
    raw.rs         Mode 3
    composer.rs    Input box
examples/          minimal.rs (real provider, custom tool), mock.rs (offline)
docs/              embedding.md (the library contract)
```

## Providers

Yoke supports these providers, with one file per provider in `src/providers/`. A provider file is
short: its id, label, base URL, which dialect it speaks, its quirks, and its sign-in. The wire
format lives in the shared dialect files, never copied per provider.

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    /// The exact JSON body that will be sent. Mode 3 shows this verbatim.
    fn body(&self, req: &Request) -> serde_json::Value;
    /// Stream the response as normalized events.
    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>>;
}
```

| id | Dialect | Endpoint | Sign-in |
| --- | --- | --- | --- |
| `codex` | responses | `https://chatgpt.com/backend-api/codex` | browser PKCE or device code |
| `grok` | responses | `https://cli-chat-proxy.grok.com/v1` | device code |
| `claude` | agent-sdk (Python worker) | the user's own Claude Code | `claude auth login` (Yoke stores nothing) |
| `copilot` | chat (GPT models: responses) | `https://api.githubcopilot.com` | GitHub device code |
| `opencode` | chat | `https://opencode.ai/zen/go/v1` | API key / `$OPENCODE_API_KEY` |
| `openai` | responses | `https://api.openai.com/v1` | API key / `$OPENAI_API_KEY` |
| `groq` | chat | `https://api.groq.com/openai/v1` | API key / `$GROQ_API_KEY` |
| `compat` | chat | `$OPENAI_COMPAT_BASE_URL` (user-configured, includes `/v1`) | base URL + optional API key / `$OPENAI_COMPAT_API_KEY` |

**compat.** `yoke login compat` prompts for the base URL (including any `/v1`) and an optional API
key; both go in `auth.json`, and the environment is read only when nothing is stored. There is no
default model, so `--model` is required unless the picker supplies one. `GET <base>/models` supplies
the ids the model picker lists. `yoke status` prints the base URL and never the key.

## Models

`models.rs` keeps the catalog used by the `/model` picker and the status line's context window. It
fetches [models.dev](https://models.dev/api.json), keeps only models with `tool_call: true`, and
maps models.dev ids onto Yoke's: `opencode` → `opencode-go`, `copilot` → `github-copilot`,
`claude` → `anthropic`, `grok` → `xai`, and `openai`/`groq` as themselves. `codex` is the `codex`
subset of models.dev's `openai` list (falling back to `gpt-5*`). `compat` is not in models.dev: its
live `GET <base>/models` is merged when a base URL is configured.

The reduced catalog is cached in `<home>/models.json` and refreshed when missing or older than 24
hours; a failed fetch keeps the stale cache and never blocks the TUI. With no cache, each provider
falls back to a short list that includes its default model, and the active model is always added.
Only connected providers that `providers::make` can build are offered (the same check `yoke status`
uses: stored credentials, the provider's key variable, a stored/env compat base URL, or codex's
token file), plus the currently active provider; grok, claude and copilot stay hidden until their
providers are implemented. Selecting a row rebuilds the provider with `providers::make` for the
next turn, keeps the conversation, and notes the switch in the timeline. Nothing is written to
`config.toml` unless that file already exists, in which case its `provider` and `model` lines are
updated.

Copy the quirks from Nexus's `docs/llm-providers.md` (encrypted reasoning replay, `replay_raw`,
`prompt_cache_key`, session headers, Copilot's `x-initiator` and vision headers). Always stream,
`parallel_tool_calls: false`, `store: false`, full history resent each request. Providers never read
`auth.json` directly; they get an `Auth` that returns headers and refreshes tokens. Never log or
display tokens; redact `Authorization` headers in mode 3.

**codex sign-in.** `yoke login codex` runs the browser PKCE flow on `127.0.0.1:1455`;
`yoke login codex --device` uses the device code instead. The tokens, refresh (with rotation) and
request headers are implemented in `auth.rs`; the streaming provider is `codex.rs` on top of
`responses.rs`, with `session_id`, encrypted-reasoning replay and `prompt_cache_key`. Crypto and
encoding are hand-rolled in `codex.rs` (SHA-256, base64url, random bytes, percent encoding) rather
than adding crates.

**claude.** Same design as Nexus's `llmhub/claude.py` (read its section in `docs/llm-providers.md`):
Yoke owns the loop, and the SDK is used as a plain client. Each request runs
`uv run --script src/providers/claude_worker.py` with a reduced environment; Rust writes the request as
one JSON line on stdin and reads normalized stream events as JSON lines on stdout. The SDK gets no
built-in tools, hooks or settings; Yoke's tools are offered as SDK MCP tools whose handlers only record
the call, and the worker returns at the first tool call so Yoke approves and runs it like any other.

Adding a provider: add a file, add a row to `PROVIDERS` in `providers/mod.rs`, add a row to the
table above, and run one real multi-step tool turn through it before calling it done.

## The loop

`loop.rs` is the heart of Yoke and should stay about a hundred lines.

1. Append the user message.
2. Build the `Request` (system prompt + memory + history + tool schemas), call `provider.stream`.
   Forward every `StreamEvent` to the UI as it arrives.
3. Append the assistant message built from the finished parts.
4. If it has no tool calls, the turn ends.
5. Otherwise run the calls one at a time, in order, emitting `ToolStart` / `ToolOutput` (live
   chunks, e.g. bash stdout) / `ToolEnd`. Append one tool message with all results. Go to 2.

A step limit (default 200) ends a runaway turn with a visible notice. Esc interrupts: the running
tool is killed, its result says it was interrupted, and that result stays in the context. The loop
sends events over a `tokio::mpsc` channel; it never touches the terminal.

## Memory

One plain Markdown file per project, kept out of the repo: `~/.yoke/projects/<slug>/memory.md`,
where `<slug>` is the project's absolute path with `/` replaced by `-` (as Claude Code does). It is
read at session start and
placed in the system prompt under a `# Memory` heading, so it is part of the visible context like
everything else. The model updates it with the ordinary `edit`/`write` tools (the system prompt
tells it the path and to keep it short: facts about the project and the user's preferences, not a
log). No index, no frontmatter, no search. The user can open it with `/memory`.

## Tools

`read`, `write`, `edit` (exact string replace), `bash` (with timeout, output streamed live and
truncated with a visible `[truncated N lines]` marker). Each is one file implementing:

```rust
pub trait Tool {
    fn name(&self) -> &str;
    fn needs_approval(&self) -> bool { false }
    fn schema(&self) -> Value;
    async fn run(&self, args: Value, out: Sink) -> ToolResult;
}
```

`write`, `edit` and `bash` ask for approval unless the user started with `--yes`. A denial is a
tool result the model sees.

## The TUI: three modes

The same timeline at three levels of detail. `Tab` cycles, `1` / `2` / `3` jump. The mode is a
view setting only; it never changes what the model receives. The current mode, model, and context
size (tokens used / window) are always in the status line.

1. **Overview** (like Nexus's default view). A context header first: system prompt, memory, tools,
   each a single row with its token estimate (chars ÷ 4, marked `~`) and a total. Then user and
   assistant messages in full; each run of tool calls folds into one line per group, e.g.
   `▸ 4 tools · read ×2, edit, bash · +12 −3`. For reading the conversation.
2. **Tools.** Everything in Overview, plus every tool call on its own row: name, compact
   arguments, status (running / ok / error / denied), duration, and its output in a clean box
   (first and last few lines with `… N lines …` between; `Enter` expands). Diffs for `edit`/`write`.
   Reasoning summaries shown dimmed. For following what the agent did.
3. **Raw.** Every single thing in the context, nothing folded or summarized: the full system
   prompt, the memory text, every tool schema, every message part as stored, reasoning items,
   tool results verbatim, usage per step, and the exact request body each step sent
   (`provider.body`). For debugging and trust.

The timeline itself lives in `tui/timeline.rs`: a `Vec<Entry>` built only from loop `Event`s, with
header entries for the system prompt, memory and tool schemas, then message entries, tool runs
(args, live and final output, status, duration), notices, per-step usage, per-step wire bodies and
the live streaming buffer. All three modes render that one timeline; a mode is a pure
`fn lines(&Timeline, width) -> Vec<Line>` dispatched by `tui::lines(timeline, mode, width)`, and
drawing applies the mode's scroll. Live streaming text appears in all three modes.

`Tab` cycles modes; `1`/`2`/`3` jump while the composer is empty. `PgUp`/`PgDn` always scroll a
page, the mouse wheel scrolls three lines per notch, and `Ctrl-Up`/`Ctrl-Down` (or
`Alt-Up`/`Alt-Down`) always scroll one line. With an empty composer, `Home` jumps to the top and
`End` returns to the bottom and follows new output; `Up`/`Down` scroll one line when a turn is
running or the view is already scrolled up, and otherwise drive the composer (line movement, then
history). Scrolling up stops following; reaching the bottom, `End`, or sending a message resumes
it. Scroll offsets are per mode and clamped to that mode's content, so the view never scrolls past
the first line. Mouse capture is on, so selecting terminal text needs Shift- or Option-drag. In
Tools mode with an empty composer, `Ctrl-N`/`Ctrl-P` move the selected tool row (marked `▶`) and
`Enter` toggles that call's output between folded and full; the expanded ids live in the app's
`expanded` set. The status line's `context_window(provider, model)` reads the cached catalog and
shows `?` when the model is unknown.

`/model` opens the inline picker above the composer: models grouped under their connected
providers, the current one marked `●` and selected `▶`. Type to filter (substring of id or name),
`Up`/`Down` (or the wheel) move, `PgUp`/`PgDn` move a page, `Enter` switches provider and model
for the next turn, `Esc` cancels; it shows a loading line while the catalog refreshes and keeps
listing the stale/fallback entries if the fetch fails. `/model <id>` changes the model on the
current provider and `/model <provider>/<id>` changes both; a model id that itself contains a slash
(`/model openai/gpt-oss-120b` on groq) is kept whole when the first segment is not a provider switch
for the current catalog. `/memory` opens the file in `$EDITOR`, then re-reads it into the prompt (with
`--no-memory` it reports that memory is disabled); `/clear`, `/save` (prints the
live session path), `/quit`.

## State

`~/.yoke/` (override with `$YOKE_HOME`): `auth.json`, `config.toml` (default provider/model),
`models.json` (the cached model catalog), `projects/<slug>/memory.md`,
`projects/<slug>/sessions/<timestamp>.jsonl`. Nothing is written into
the project directory. A `.env` in the current directory is read once at startup for API keys
(`KEY=VALUE` lines; existing process variables win); it is never written. Tests use a temporary
`$YOKE_HOME`; never touch the real one from a test or an agent session.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo run -- login groq        # then:
cargo run                      # the TUI
```

- Provider dialects get fixture tests: a recorded SSE stream in, normalized events out; and a
  request in, exact JSON body out.
- The loop gets tests with a scripted mock provider (`#[cfg(test)]`), no network.
- Work from [PLAN.md](PLAN.md): tick off steps as they land and note decisions there.
- Keep this file current: when you add a provider, tool, mode behavior or file, update it in the
  same change.
- Commits and PRs are authored by the user alone. Never add `Co-Authored-By` trailers, "Generated
  with" lines or any other AI attribution (Claude or otherwise) to commit messages or PR bodies.

# PLAN

The build order for Yoke. [AGENTS.md](AGENTS.md) is the spec; this file is the order of work and
what is done. Tick a box when the step has landed with its tests, and add a line under
**Decisions** when something changes from the spec.

Each milestone ends with something runnable. Do not start a milestone before the previous one runs.

## M0 — Skeleton

- [x] `cargo new yoke`, edition 2021, add the deps from AGENTS.md, `rustfmt.toml`, CI-free
      `cargo test` + `clippy -D warnings` clean
- [x] `src/` layout from AGENTS.md with empty modules so the tree matches the spec
- [x] `main.rs` CLI parsing (hand-rolled or `clap`, decide and record): `yoke`, `yoke login <p>`,
      `yoke logout <p>`, `yoke status`, `--provider`, `--model`, `--yes`
- [x] `$YOKE_HOME` / `~/.yoke` resolution and project slug (`/Users/a/b` → `-Users-a-b`)

**Done when:** `cargo run -- status` prints the home dir and the project slug.

## M1 — Types and the loop, offline

- [x] `types.rs`: `Message { role, parts }`, `Part` (Text, Reasoning, ToolCall, ToolResult, Image),
      `Request { model, instructions, messages, tools, effort, conversation_id }`,
      `StreamEvent` (TextDelta, ReasoningDelta, PartDone(Part), Usage, Done(stop), Wire(body)),
      serde JSON for all of it
- [x] `providers/mod.rs`: `Provider` trait, `PROVIDERS` table, `make(id, model)`
- [x] Mock provider (`#[cfg(test)]` + `YOKE_DEV=1`) that replays scripted turns
- [x] `loop.rs`: the five steps, step limit with a visible notice, cancellation token for Esc,
      events over `mpsc` (`StepStart`, stream events, `MessageAdded`, `ToolStart`, `ToolOutput`,
      `ToolEnd`, `TurnEnd`)
- [x] Loop tests with the mock: text-only turn, multi-step tool turn, step limit, interrupt mid-tool

**Done when:** `cargo test` drives a 3-step tool turn through the mock.

## M2 — Tools

- [x] Tool trait + registry (`tools/mod.rs`), JSON schemas
- [x] `read` (line numbers, offset/limit), `write`, `edit` (exact unique replace; error on 0 or >1 hits),
      `bash` (timeout, live output chunks, kill on interrupt, `[truncated N lines]` marker)
- [x] Approval: `write`/`edit`/`bash` ask through the loop's event channel; `--yes` skips; a denial is
      a tool result
- [x] Tool tests in a temp dir

## M3 — Memory, prompt, sessions

- [x] `memory.rs`: read `~/.yoke/projects/<slug>/memory.md` (missing = empty), expose its path
- [x] System prompt: base instructions, workspace path, date, memory under `# Memory` with its path and
      the rule "keep it short: project facts and user preferences, not a log"
- [x] `session.rs`: append each `Message` to `projects/<slug>/sessions/<ts>.jsonl`; `--resume` loads the
      latest
- [x] Context accounting: per-section token estimate (chars ÷ 4, `~`) for system, memory, tools,
      messages; real usage from `Usage` events when present

## M4 — First real provider, plain CLI

Prove the loop on the wire before building the TUI.

- [x] `providers/sse.rs` with tests (multi-line data, comments, `[DONE]`, split chunks)
- [x] `providers/chat.rs` (Chat Completions): body builder + stream parser, tool-call delta assembly,
      `reasoning_content`, usage; fixture tests (request → exact JSON, recorded SSE → events)
- [x] `auth.rs`: `auth.json` store (0600, atomic write), `Auth` trait (`headers()`), API-key auth
      reading stored key or `$ENV`
- [x] `groq.rs` (API key, chat)
- [x] A throwaway line-mode REPL behind `yoke --plain` that prints events, so the loop is usable now
- [ ] Live check: one multi-step tool turn on groq (needs `GROQ_API_KEY`; not run here)

**Done when:** `yoke --plain` on groq reads a file, edits it, and answers (needs `GROQ_API_KEY`; not
verified here).

## M5 — TUI

- [x] `tui/mod.rs`: ratatui + crossterm app, alt screen, panic-safe restore, loop on a tokio task,
      UI state = the timeline (`Vec<Entry>`) built only from loop events
- [x] Composer: multi-line input, Enter sends, Shift+Enter newline, Esc interrupts, history
- [x] Status line: mode, provider/model, context `used / window`, running indicator
- [x] Mode 1 **Overview**: context header (system / memory / tools rows with `~tokens` and a total),
      user + assistant messages, tool runs folded to one line per group
      (`▸ 4 tools · read ×2, edit, bash · +12 −3`), live streaming text
- [x] Mode 2 **Tools**: one row per call (name, compact args, status, duration), output box with head/
      tail and `… N lines …`, `Enter` expands, diffs for `edit`/`write`, dimmed reasoning
  - `Ctrl-N`/`Ctrl-P` select a row with an empty composer (marked `▶`); `Enter` toggles the
    selected call's id in `App.expanded`.
- [x] Mode 3 **Raw**: full system prompt, memory, every tool schema, every part verbatim, per-step usage,
      the exact request body per step (`Authorization` redacted)
- [x] `Tab` cycles modes, `1`/`2`/`3` jump; scroll per mode, follow-bottom while streaming
- [x] Approval prompt inline (y / n / a = always this session)
- [x] Commands: `/model <id>`, `/memory` (opens in `$EDITOR`, re-reads immediately), `/clear`, `/save`, `/quit`
- [x] Renderer is a pure function of (timeline, mode, scroll, width): snapshot tests with
      `ratatui::backend::TestBackend` for each mode
  - All three modes render one scripted timeline (messages, reasoning, a read/edit/bash run, usage,
    wire body); purity and 40-column tests sit next to them.

**Done when:** all three modes are usable; the live interactive check (real groq session) is
pending the user. `--plain` stays for now.

## M6 — The rest of the chat-dialect providers

Each is one small file plus a row in `PROVIDERS`, a live multi-step tool turn, and a row ticked here.

- [x] `opencode` (API key, `x-opencode-session` header)
  - Provider and fixtures landed; the live multi-step tool turn is still pending (`OPENCODE_API_KEY`).
- [ ] `compat` (OpenAI-compatible: base URL + optional API key, models from `GET <base>/models`)
  - Provider, `yoke login compat`, URL/model parsing and fixtures landed; the live multi-step tool
    turn needs a local OpenAI-compatible server (none answers here).
- [ ] `copilot` (GitHub device code RFC 8628, token exchange, `x-initiator`, `copilot-vision-request`)

## M7 — Responses dialect

- [x] `providers/responses.rs`: body builder (`instructions`, `input` items, `reasoning`, `store:false`,
      `include`, `prompt_cache_key`), stream parser, encrypted-reasoning replay; fixtures
  - Text and function-call items are rebuilt from parts (the Responses API accepts that); only a
    reasoning item is replayed verbatim, because its `encrypted_content` cannot be rebuilt. Parts
    carry no provider origin, so there is no cross-provider raw replay decision.
- [x] `openai` (API key)
  - Provider and fixtures landed; the live multi-step tool turn needs `OPENAI_API_KEY`.
- [x] `codex` (ChatGPT OAuth: browser PKCE on `127.0.0.1:1455` + device code, refresh, session header)
  - Sign-in landed early: `yoke login codex` (browser) and `yoke login codex --device`, credential
    storage, refresh rotation and headers. The Responses streaming provider landed with `session_id`,
    encrypted-reasoning replay and `prompt_cache_key`. A live turn is impossible without the user's
    ChatGPT login, so it is unverified here.
- [ ] `grok` (xAI device code, `x-grok-conv-id`)
- [ ] `copilot` GPT models routed to `/responses`

## M8 — Claude via Python

- [ ] `providers/claude_worker.py` as a `uv` inline-script (`# /// script` deps: `claude-agent-sdk`):
      JSON line in, JSON-line events out, no built-in tools/hooks/settings, Yoke tools as recording MCP
      tools, return at first tool call
- [ ] `providers/claude.rs`: spawn `uv run --script`, reduced env, timeout, stderr tail in errors,
      `body()` = the logical request for mode 3
- [ ] `yoke login claude` checks the user's Claude Code sign-in; Yoke stores nothing
- [ ] Live multi-step tool turn

## M9 — Polish

- [x] Model picker for `/model` (per-provider model list; models.dev catalog or the provider's
      `/models` — decide and record)
- [x] Context window per model for the status line
- [ ] Retry with backoff on network errors before any stream event, shown as a timeline notice
- [ ] `config.toml`: default provider/model, approval default
- [ ] README with install and a screenshot of each mode

## M10 — Library

- [x] `src/lib.rs`: public modules, crate docs, `Config`, `yoke::agent` (the loop's public name)
- [x] `main.rs` is a thin consumer via `yoke::…`; no blanket `#[allow(dead_code)]` left
- [x] Public, documented surface: types, agent, providers (incl. always-available mock), tools,
      prompt, memory, session, context, auth, models; `yoke::tokio` re-export for embedders
- [x] `Registry::only` (tool restriction) and `Registry::confine` + `tools::Confined` (path root)
- [x] `--tools`, `--root`, `--no-memory` in the CLI, TUI and plain REPL; `--help` lists them
- [x] `examples/minimal.rs` (real provider, custom tool) and `examples/mock.rs` (offline)
- [x] `docs/embedding.md`
- [x] Doc-test on `lib.rs` runs a full mock turn offline; tests for restriction, confinement and
      the no-memory prompt

## M11 — Web demo

Built by another builder under `demo/`; this checklist is only a placeholder here.

- [ ] Terminal in the browser
- [ ] Sandboxed container
- [ ] Groq only
- [ ] BYO key or provided tokens
- [ ] Deploy as `yoke.santa.build`

## Decisions

- 2026-11-22: Yoke is a library first. `src/lib.rs` owns the modules; `main.rs` only parses flags
  and hands off. The loop's public name is `yoke::agent` (the file stays `loop.rs`; a private
  `crate::r#loop` alias keeps internal paths unchanged). TUI and plain REPL are ungated for now —
  a feature would touch every consumer for little benefit at this size.
- 2026-11-22: `--tools` restricts through `Registry::only` (unknown names are an error before the
  registry changes), so `prompt::build` gets the kept tool names and `registry.schemas()` sends only
  those. `--root` wraps every tool in `tools::Confined`: it resolves a string `path` argument inside
  the canonical root (deepest existing parent for new files) and returns an ordinary error result on
  an escape, so `bash` stays visibly unconfined (the CLI warns on stderr). `--no-memory` makes
  memory `Option` end to end: absent from the prompt, the timeline and the context header, and
  `/memory` reports it is disabled.
- 2026-11-22: `providers::mock::MockProvider` is always public (`text`/`new`/`dev`), so embedders
  and examples test offline without `YOKE_DEV`. `yoke::tokio` is re-exported; `agent::event_channel`
  avoids making callers name the channel type.

- 2026-11-22: Python is allowed where a provider's only good client is Python (`claude`); managed by `uv`.
- 2026-11-22: Memory lives in `~/.yoke/projects/<slug>/memory.md`, not in the project.
- 2026-11-22: Chat dialect + groq first (simplest wire, API key auth) to prove the loop before the TUI.
- 2026-11-22: M0 crate created in place with `cargo init --name yoke` (no nested `yoke/` dir), edition 2021.
- 2026-11-22: CLI parsing is hand-rolled instead of adding `clap`; flags are parsed now and acted on in
  later milestones.
- 2026-11-22: State-dir resolution lives in `memory.rs`; `home(env_home)` takes `$YOKE_HOME` as a
  parameter so tests are pure. An empty `$YOKE_HOME` counts as unset.
- 2026-11-22: The loop's caller appends the new user message to `messages`; `run` starts at the
  request. Provider output is re-emitted as `Event::Stream` next to the loop's own events.
- 2026-11-22: Esc cancellation is a hand-rolled `watch`-backed `CancelToken` (`tokio-util` not added);
  dropping the tool future aborts the tool, and its interrupted `ToolResult` stays in context.
- 2026-11-22: `make` takes `$YOKE_DEV`, the Yoke home and an env lookup closure as parameters; the
  mock is reachable only when `$YOKE_DEV` is exactly `1`, and then replays a one-line `hello` script.
- 2026-11-22: Tool schemas use `{name, description, parameters}` (OpenAI's shape); dialect body
  builders adapt it rather than each tool carrying per-provider schemas.
- 2026-11-22: `Tool::needs_approval()` defaults to false; `Options::yes` skips prompts per run.
  Requests arrive as `Event::ApprovalRequest(id, name, args, reply: oneshot<bool>)`; a dropped reply
  is a denial. `ToolEnd` carries `ToolStatus` (`Ok`/`Error`/`Denied`/`Interrupted`) so the UI can
  tell a denial from a failure. Esc while waiting for approval counts as an interrupt.
- 2026-11-22: `bash` timeout is whole seconds (default 120) and its return output keeps the first
  and last 1000 lines of anything over 2000, with `[truncated N lines]` between; live chunks are
  never truncated.
- 2026-11-22: One `clock.rs` holds the UTC date math, so the prompt date and the sortable session
  timestamp (`YYYYMMDDTHHMMSSZ`) share it without a crate. Dates are UTC, not local.
- 2026-11-22: `startup(home, workspace, resume)` in `main.rs` is the single entry point the TUI/plain
  REPL will call: it builds the prompt from the memory file, creates a fresh session, and on
  `--resume` loads the latest session's messages into the context (the new run still writes its own
  session file).
- 2026-11-22: `Sections::new` counts the memory once: the memory text is removed from the full
  system prompt before estimating `system`, then counted under `memory`.
- 2026-11-22: SSE parsing is line-buffered and UTF-8-safe across arbitrary byte boundaries; CR, LF
  and CRLF all end a line. `[DONE]` is emitted as data and interpreted by the chat parser.
- 2026-11-22: The Chat dialect parser emits `Done` at the provider's `finish_reason` (or `[DONE]`
  when absent) and assembles tool calls by `index` before it. `Wire(body)` is the first event.
- 2026-11-22: Auth is a small `{providers: {id: {api_key}}}` JSON file written to a mode-0600 temp
  file then renamed. API keys trim, stored keys win over `$<ENV>`, and env values are passed in.
- 2026-11-22: `make` takes the Yoke home and an env lookup closure, so tests never read or mutate
  the process environment. Groq's default model is `openai/gpt-oss-120b`.
- 2026-11-22: `--plain` is a synchronous stdin loop around one tokio runtime per process; each turn
  runs the loop on a future selected against `ctrl_c`, which cancels that turn's `CancelToken`.
  The `mock` provider has a one-line `hello` script under `YOKE_DEV=1` for smoke tests.
- 2026-11-22: M4 is complete except the live groq check, which needs `GROQ_API_KEY` (not available
  here). The wire path is covered by fixtures; `--plain --provider mock` was smoke-tested.
- 2026-11-22: Codex sign-in landed ahead of M7 (browser PKCE, device code, refresh, headers). SHA-256,
  base64url, `/dev/urandom` reads and percent-encoding are hand-rolled in `providers/codex.rs`; no
  crates or reqwest features were added. The live OAuth flow is unverified.
- 2026-11-22: `.env` in the working directory is read once at startup for API keys (`KEY=VALUE`,
  comments, optional `export` and quotes); values already in the process environment win, and the
  file is never written. The parser (`src/dotenv.rs`) is pure; `main` applies it before anything
  reads the environment.
- 2026-11-22: `opencode` uses the chat dialect, `x-opencode-session` from `conversation_id` (cut to
  64 characters), and replays reasoning as `reasoning_content`; its default model is
  `deepseek-v4.1-flash` (the one Nexus is configured to use). Live check not run yet.
- 2026-11-22: `nvidia` and `ollama` were dropped in favour of one `compat` provider: a
  user-configured OpenAI-compatible base URL and optional API key, always the chat dialect. The base
  URL (which includes any `/v1`) and the key are stored in `auth.json`; `$OPENAI_COMPAT_BASE_URL` /
  `$OPENAI_COMPAT_API_KEY` are read only when nothing is stored. There is no default model, so
  `--model` is required (the M9 picker will offer `GET <base>/models`). `yoke status` shows the base
  URL and never the key.
- 2026-11-22: The TUI's timeline (`src/tui/timeline.rs`) is the only view state: header entries for
  system prompt, memory and tool schemas, then `Message` entries, `Tool` runs, notices, per-step
  `Usage` and `Wire` bodies, and a live `Streaming` entry. Modes are pure `lines(timeline, width)`
  functions; `tui::draw` / `draw_body` only apply scroll. The app reconstructs its message list from
  `Event::MessageAdded` (and the user message it submits), so the loop task can own its own copy
  while the session and timeline stay append-only.
- 2026-11-22: Key routing: `Tab` always cycles; `1`/`2`/`3` jump only with an empty composer so they
  remain typeable. `PgUp`/`PgDn` always scroll a page; `Ctrl-Up`/`Ctrl-Down` and `Alt-Up`/`Alt-Down`
  always scroll one line. With an empty composer, `Home`/`End` jump to the top / bottom (End resumes
  following) and `Up`/`Down` scroll when a turn is running or the mode is already scrolled up;
  otherwise they move the cursor and then recall history. `Scroll` is measured from the bottom
  (`follow` or `back` lines) and clamped per mode to that mode's rendered line count; sending a
  message returns every mode to the bottom and following. Mouse capture
  is enabled, so the wheel scrolls three lines per notch and terminal text selection needs
  Shift/Option-drag; capture is disabled while `/memory` has the alternate screen suspended and
  restored with it.
- 2026-11-22: The TUI waits at most 40 ms for a loop event, then polls crossterm's keyboard without a
  background reader thread. A blocking reader would steal keystrokes from `$EDITOR` while `/memory`
  has suspended the alternate screen. The loop runs on a spawned tokio task per turn.
- 2026-11-22: `ToolRun::duration` is wall time from `ToolStart` to `ToolEnd` and therefore includes
  any approval wait. `/memory` re-reads the file and rebuilds the system prompt immediately (and
  updates its header entry), rather than waiting for the next session, so the timeline still shows
  exactly what the model receives. `context::context_window` now reads the cached catalog (M9).
- 2026-11-22: `Session::create_at` opens with `create_new` and adds a `-N` suffix on collision, so two
  starts (or `/clear`) in the same second get distinct files instead of sharing one.
- 2026-11-22: M5 part A is verified with `cargo build`, `cargo test` (122 tests, three stable runs),
  `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. A pty smoke run
  (`YOKE_DEV=1 yoke --provider mock`, then `hello`, `/model m2`, `/save`, `/quit`) exited 0 and
  showed the reply, notices, status line and alternate-screen restore. `/memory` (it would launch
  `$EDITOR`) and the interactive approval prompt are covered by unit tests, not a TTY run.
  `cargo install --path . --locked` was not run (per part-A instructions).
- 2026-11-22: M5 part B: Tools mode keeps an `expanded: HashSet<String>` of tool call ids and a
  `selected` row index in `App`; `Ctrl-N`/`Ctrl-P` move the selection (empty composer, Tools mode),
  `Enter` on an empty composer toggles it, and `lines_with`/`tools::lines_expanded` render the `▶`
  marker. The renderer stays pure: `lines_with` sees only the timeline, mode, width, expanded ids
  and selected id. Snapshots for all three modes use one hand-built timeline; `lines` remains the
  no-state entry point for tests. Verified with build, tests (three runs, 128 tests), clippy, fmt
  and `cargo install --path . --locked`. The live interactive check is still pending the user.
- 2026-11-22: M7 Responses dialect: `responses.rs` builds `instructions` + ordered `input` items,
  flat function tools with `strict: false`, `tool_choice: auto`, `parallel_tool_calls: false`,
  `store: false`, `stream: true`, `reasoning: {summary: auto, effort?}`, `include` when
  `encrypted_reasoning`, and `prompt_cache_key`/the session header from the conversation id (cut to
  64). The parser ignores function-argument deltas (Yoke has no live argument event) and emits the
  finished call. `Part::Reasoning` gained an optional `raw` item (`serde(default)`) so encrypted
  reasoning survives a replay; the loop merges the finished reasoning item over the part its deltas
  built. Text and function calls are rebuilt, since Yoke's parts carry no provider origin/raw.
  `codex` uses `session_id`, encrypted reasoning and `prompt_cache_key`; `openai` uses only encrypted
  reasoning and `prompt_cache_key`. Both default to `gpt-6.1-terra`.
- 2026-11-22: The `openai` live check was skipped because `.env` has no `OPENAI_API_KEY`; the
  `codex` live check is impossible without the user's ChatGPT login. Both are covered by
  request/stream fixtures.
- 2026-11-22: M9 model picker. models.dev (`api.json`) is the catalog: only `tool_call: true`
  models, mapped from models.dev ids to Yoke ids (`opencode`→`opencode-go`, `claude`→`anthropic`,
  `copilot`→`github-copilot`, `grok`→`xai`, `groq`/`openai` as themselves). `codex` is the codex
  subset of models.dev's `openai` list (falling back to `gpt-5*`). `compat` merges a live
  `GET <base>/models` (reusing `providers::compat::models`) when a base URL is configured. The
  reduced catalog lives in `~/.yoke/models.json`,
  refreshed on TUI start and on `/model` when missing or older than 24 h; a failed fetch keeps the
  stale cache (or per-provider fallbacks), never blocks the TUI, and the picker shows the error.
  Only connected providers that `providers::make` can build are listed (stored key, key env var,
  codex token file, or a stored/env compat base URL) plus the active provider;
  `models.rs::connected` mirrors `yoke status`, and `buildable` hides grok/claude/copilot until they
  are implemented.
  Selecting a row rebuilds the provider with `providers::make` for the next turn, keeps the
  conversation and adds a timeline notice. `/model` opens the inline overlay (type to filter,
  arrows/wheel/PgUp/PgDn, Enter, Esc); `/model <id>` and `/model <provider>/<id>` remain. `config.toml`
  is written only when it already exists, and `context::context_window` reads the same catalog
  (`context_window_from` for the App's temp-home catalog). Tests use a recorded fixture, a temp
  home and no network.

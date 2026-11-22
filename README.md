# Yoke, explained simply

## The problem

When you use an AI coding agent, a lot happens out of sight. The program quietly sends the
model a long system prompt, saved notes, tool descriptions, earlier messages and tool results.
You see a tidy chat, but the model sees something much bigger and messier. When the agent does
something odd, you can't tell why, because you never saw what it was given.

## The idea

**Yoke shows you exactly what the model sees. Nothing hidden, nothing extra.**

Think of it as a glass box instead of a black box. The screen and the model's input are the
same list of things, in the same order:

1. The instructions (system prompt)
2. The project notes (memory)
3. The tools the model can use
4. Your messages, the model's replies, every tool it ran and what came back

If it's in the model's head, it's on your screen. If it's on your screen, it's in the model's
head.

## How it works

Yoke is a very small agent. It repeats one simple loop:

```
you ask  →  model answers or asks to use a tool
         →  Yoke runs the tool (you approve risky ones)
         →  the result goes back to the model
         →  repeat until the model is done
```

The model gets four tools: **read** a file, **write** a file, **edit** a file, and run a shell
command (**bash**). You watch each one run live.

## Three ways to look at the same thing

Press `Tab` (or `1`, `2`, `3`) to zoom in or out. The view changes; what the model gets does not.

| Mode | What you see | Good for |
| --- | --- | --- |
| **1. Overview** | The conversation, with tool calls folded into short summaries | Reading the chat |
| **2. Tools** | Every tool call, its arguments, result and diffs | Following what the agent did |
| **3. Raw** | Every byte sent to the model, unfolded | Debugging and trust |

## Memory

Each project gets one plain notes file (`~/.yoke/projects/<project>/memory.md`). It goes into the
prompt where you can see it, and the model updates it with the same edit tool it uses for code.
No hidden database.

## Many models, one app

Yoke talks to many AI providers: ChatGPT (Codex), Claude, Grok, GitHub Copilot, OpenAI, Groq,
OpenCode and any OpenAI-compatible server (`compat`). You can switch between them; the screen
works the same way for all of them.

## Build your own agent

Yoke is also a Rust library. The CLI is just one consumer: `yoke::agent::run` takes a provider,
a tool registry and your messages, and emits events for whatever UI you write. See
[docs/embedding.md](docs/embedding.md), or start from `examples/minimal.rs` and the fully offline
`examples/mock.rs`.

For hosted use there are safety flags: `--tools read,write,edit` limits which tools exist,
`--root DIR` confines the file tools to a directory, and `--no-memory` leaves the notes file out
of the prompt entirely. (`bash` is not confined by `--root`; the CLI warns when both are on.)

## Kept small on purpose

- One program, written in Rust.
- One loop, no background services, no plugins, no helper agents.
- Few dependencies.
- Rule of thumb: if a change makes Yoke harder to read in one sitting, it's probably the wrong
  change.

## In one sentence

Yoke is a tiny coding agent that never keeps secrets from you: what you see is exactly what the
model sees.

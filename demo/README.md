# Yoke hosted demo

This folder is the live demo behind <https://yoke.santa.build>: a plain static
page with a real terminal in it. Pressing **Start** opens one WebSocket; the Go
bridge starts a real `yoke` process in a PTY, in a throwaway directory, and
pipes the two together. Nothing is recorded.

```
browser (ghostty-web) ⇄ WebSocket ⇄ demo/bridge ⇄ PTY ⇄ /yoke
                                      │
                                      └── /tmp/yoke-demo/<session>/work  (deleted)
```

## What a session is

- The bridge creates `work/` (copied from [`workspace/`](workspace)) and `home/`, then runs:

  ```
  yoke --provider groq --model $YOKE_DEMO_MODEL \
       --tools read,write,edit --root <work> --no-memory --yes
  ```

- The child environment is only `PATH`, `HOME`, `YOKE_HOME`, `TERM`, `COLORTERM`,
  `LANG` and `GROQ_API_KEY`. Nothing else from the bridge leaks in.
- The key is either the visitor's (sent in the first WebSocket frame) or the
  bridge's own `GROQ_API_KEY`. It is placed in the child's environment, never
  written to disk, never logged and never echoed by the bridge.
- On disconnect, idle timeout or the session limit the whole process group is
  killed and the directory is removed. Leftovers are swept at startup.

## Env

| Variable | Default | Meaning |
| --- | --- | --- |
| `ADDR` | `127.0.0.1:8080` | listen address |
| `YOKE_DEMO_TMP` | `/tmp/yoke-demo` | root for per-session dirs |
| `YOKE_DEMO_BIN` | `/yoke` | yoke binary (or a fake for local tests) |
| `YOKE_DEMO_SEED` | `/workspace-template`, else `../workspace` | the demo project, copied per session |
| `YOKE_DEMO_PROVIDER` | `groq` | provider id passed to yoke |
| `YOKE_DEMO_MODEL` | `qwen/qwen3.8-27b` | model passed to yoke |
| `GROQ_API_KEY` | — | free demo tokens; if empty only BYO-key works |
| `YOKE_DEMO_MAX_SESSIONS` | `6` | concurrent PTYs |
| `YOKE_DEMO_HOURLY_PER_IP` | `3` | provided-token sessions per IP per hour |
| `YOKE_DEMO_DAILY_TOTAL` | `150` | provided-token sessions per UTC day, globally |
| `YOKE_DEMO_SESSION_MINUTES` | `15` | wall-clock limit |
| `YOKE_DEMO_IDLE_MINUTES` | `3` | disconnect after this much quiet |
| `YOKE_DEMO_ORIGINS` | `https://yoke.santa.build,http://localhost,http://127.0.0.1` | allowed `Origin`s (bare localhost means any port) |
| `YOKE_DEV` | — | passed through to yoke; `1` enables the mock provider |

BYO-key sessions are never counted against the provided-token quotas, but they
do use one of the six concurrent slots.

## Protocol

Client → server:

- first text frame: `{"type":"start","cols":120,"rows":36,"key":"…"}` — `key`
  is optional; omit or send `""` to use the demo tokens
- binary frames: keystrokes/bytes straight to the PTY
- text: `{"type":"resize","cols":120,"rows":36}`

Server → client:

- binary frames: PTY output
- `{"type":"ready","limit_seconds":900}`
- `{"type":"status","msg":"…"}` (busy, bad start, unavailable)
- `{"type":"limit","msg":"…","retry_after":3600}`
- `{"type":"end","reason":"…"}`

## Local development

```sh
cd demo/web
npm ci
npm run build        # writes demo/bridge/web/dist, which the bridge embeds

cd ../bridge
go test ./...

# Real TUI with the offline mock provider (needs a yoke checkout that builds).
# demo/dev-yoke.sh ignores the hosted flags the current CLI may not know yet.
YOKE_DEV=1 YOKE_DEMO_BIN=../dev-yoke.sh YOKE_DEMO_SEED=../workspace \
  YOKE_DEMO_PROVIDER=mock YOKE_DEMO_MODEL=mock GROQ_API_KEY=dummy \
  go run .
# open http://127.0.0.1:8080
```

Without a suitable yoke binary, point `YOKE_DEMO_BIN` at any test child (the
Go tests use a small `/bin/cat` wrapper). `YOKE_DEMO_BIN=/bin/cat` also works
to see bytes flow, though it is not a TUI.

The Docker build is the real deployment shape:

```sh
docker build -t yoke-demo-test .
docker run --rm -p 8085:8080 -e YOKE_DEV=1 -e GROQ_API_KEY=dummy yoke-demo-test
```

## Threat model

A visitor controls the terminal: its keystrokes, its files, and the prompts it
sends. They can:

- run `read`, `write` and `edit` against files **in their own scratch folder**;
- ask the model to create or change those files, and read the result back;
- hit the demo-token rate limits, or use their own Groq key;
- see the real TUI, switch views, and interrupt a turn.

They cannot:

- run a shell: `bash` is not in the tool list, and the image has no shell;
- reach outside the scratch folder: `--root` confines the tools, and the
  process runs as non-root in a read-only container with all capabilities
  dropped;
- see another visitor's files or key: each session is its own directory and
  process, and keys are never logged or persisted;
- keep anything after the session: the process group is killed and the folder
  removed. The tmpfs makes leftovers vanish on restart.

The remaining trust boundary is the model provider: prompts and file contents
in a session go to Groq. The page says so. The bridge is deliberately small so
that this list stays true.

## Notes

- `demo/workspace/` is the project every session starts in. Yoke runs with that
  folder as its cwd and as `--root`, so the TUI's workspace lines and relative
  paths match what the page describes. Keep it tiny and harmless.
- The page has no CDNs, analytics or external requests. Fonts (Monaspace Argon)
  and the ghostty-web WASM are bundled by Vite.
- Build output is embedded in the Go binary, so the container is just `/yoke`,
  `/bridge`, `/workspace-template` and an empty `/tmp`.

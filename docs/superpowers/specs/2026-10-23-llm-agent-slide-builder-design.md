# LLM agent slide builder — design

> **Status:** draft for review · **Date:** 2026-10-23 · **Branch:** `feature/llm-agent` (local fork, no remote)

## 1. Goal

A collapsible chat pane on the right of the DeckCraft web editor. A team member types what they
want ("make slide 3 a two-column comparison", "use the dark theme and add speaker notes"), and an
LLM agent edits the open deck. Every change is visible immediately on the canvas, every agent turn
is tracked, and any turn can be reverted.

### Requirements (from the user)

- Web build only for v1 (the deck lives in the browser's wasm editor).
- A backend exists from v1: it holds LLM credentials, user accounts and chat history.
- LLM access goes through a LiteLLM proxy (Claude Sonnet 5.5). For local testing, Qwen 3.8 served
  by llama-swap is also selectable.
- Gateway URL and API key come from environment variables or a server config file.
- Small team on a shared server: simple username/password accounts now, Microsoft Entra ID later.
- The agent can make any change: through the app's ~320 commands first, and through a JSON patch
  of the document model for anything commands can't express.
- Each chat request is one checkpoint: one Ctrl+Z undoes the whole turn; the chat shows what each
  turn changed with a "Revert to before this" button.
- The agent may look at rendered slides (vision) when the model supports it.

### Non-goals (v1)

- Desktop build integration (the pane is written so it can follow later).
- Entra ID login (the auth interface is shaped for it).
- Real-time multi-user co-editing of one deck; decks still live in one browser tab.
- Persisting undo snapshots across page reloads.
- Agent access to the file system (open/save/export/insert-from-path) or UI-only tools.

## 2. Architecture

```
Browser: DeckCraft wasm (Rust, egui)                Backend: Python (FastAPI)              LLM
┌─────────────────────────────────────┐  HTTPS+WS  ┌────────────────────────────────┐
│ Editor + Session (deck, undo)       │◀──────────▶│ auth (accounts → Entra later)  │   ┌───────────────────┐
│ Agent pane (egui, right, collapsible│ same origin│ agent loop (provider-neutral)  │──▶│ LiteLLM /v1/messages│
│ Agent client: WS, login, chat state │            │ providers: anthropic,          │   │  claude-sonnet-5-5  │
│ Tool executor → LiveBackend         │◀─tool_call─│            openai_compat       │──▶│ llama-swap          │
│   (mcp::call_tool over control.rs)  │─tool_result▶ store (SQLite)                 │   │  /v1/chat/completions│
│                                     │            │ static host for the wasm site  │   │  qwen3.8            │
└─────────────────────────────────────┘            └────────────────────────────────┘   └───────────────────┘
```

The deck never leaves the browser. The backend runs the agent loop and forwards each tool call to
the browser over a WebSocket; the browser executes it on the live session and returns the result.
The backend serves the wasm site, so editor, API and WebSocket share one origin (no CORS).

### 2.1 Browser side (Rust)

| Unit | Where | Responsibility |
|---|---|---|
| Agent pane | `crates/ui-egui/src/agentpane.rs` | Right-side collapsible panel: login form, conversation list for the deck, message stream, input box, model picker, Stop button, per-turn cards (actions, slides touched, Revert). Reads `AgentState`, acts only through its methods. Theme tokens for colours. |
| Agent state | `crates/ui-egui/src/agent/state.rs` | Conversation, messages, turn status (`Idle`, `Running{checkpoint}`, `Error`), pending tool calls. Pure data + transitions; unit-testable without egui or network. |
| Transport | `crates/ui-egui/src/agent/transport.rs` (trait) + `apps/deckcraft-web/src/agent_ws.rs` (impl) | `AgentTransport` trait: `send(ClientMsg)`, `poll() -> Vec<ServerMsg>`, `status()`. The web impl wraps `web_sys::WebSocket` and `fetch` (login/REST, `credentials: same-origin`). A fake impl drives tests. Keeps `ui-egui` free of web-sys. |
| Protocol types | `crates/ui-egui/src/agent/protocol.rs` | serde types for every message in §4, with size caps on decode. |
| Tool executor | `crates/ui-egui/src/agent/tools.rs` | Advertised tool list (§3.3), allow-list check, runs `mcp::call_tool` against `LiveBackend`, handles `get_deck_json` / `patch_deck_json`. |
| LiveBackend | `crates/ui-egui/src/agent/live_backend.rs` | Implements `deckcraft_mcp::Backend` by calling the existing in-process dispatcher in `control.rs` (`engine.execute`, `document.inspect`, `ui.render`, …) on the running `SlideApp`. |
| Engine: turn squash | `crates/engine/src/lib.rs` | `History::squash_since(depth, label)`: replaces the undo entries above `depth` with the first of them, relabelled. Clears redo. |
| Engine: replace document | `crates/engine/src/cmd/doc.rs` | `doc.replace {json}` command: deserialize a `Presentation`, validate (ids unique, references resolve, sizes and depths bounded, as the `.deckcraft` reader does), replace the document as one undoable step. Never opens dialogs. |
| Canvas lock | `crates/ui-egui/src/canvas.rs`, `lib.rs` | While a turn runs, editing pointer/key input on the canvas, ribbon and panes is ignored (a single `agent_busy` check at the input dispatch points); navigation, zoom and the agent pane's Stop stay active. |
| mcp feature gate | `crates/mcp/Cargo.toml`, `lib.rs` | `remote` feature (default on) gates `Remote` and `std::net`; the web build depends on `deckcraft-mcp` with `default-features = false`. |

Calls into the session happen inside the egui `update` of the frame that polled the tool call, so a
change is drawn in that same or the next frame. One tool call is executed per frame at most, so the
UI stays responsive during long turns.

### 2.2 Backend (Python)

New top-level folder `server/` (package `deckcraft_server`, Python ≥ 3.12, managed with `uv`).

| Module | Responsibility |
|---|---|
| `config.py` | Loads `server/config.toml`, expands `${ENV}` references, env vars override (`LITELLM_BASE_URL`, `LITELLM_API_KEY`, `AGENT_DEFAULT_PROVIDER`, `DECKCRAFT_SITE_DIR`, `DECKCRAFT_DB`, `SESSION_SECRET`). Secrets are never sent to the browser. |
| `auth/base.py` | `AuthProvider` interface: `authenticate(request) -> User \| None`, `login_routes()`. |
| `auth/local.py` | Local accounts: argon2id hashes in SQLite, signed HttpOnly session cookie (SameSite=Strict, Secure on HTTPS), login rate limit per IP and user. |
| `cli.py` | `python -m deckcraft_server users add NAME [--admin]`, `users passwd`, `users disable`, `serve`. No self sign-up. |
| `providers/base.py` | `LLMProvider` interface over a provider-neutral turn format: `stream(system, messages, tools) -> AsyncIterator[Event]` where events are text deltas, tool-use requests, usage and stop. Each provider declares `vision: bool`. |
| `providers/anthropic.py` | `anthropic.AsyncAnthropic(base_url=…, api_key=…)` against LiteLLM's Anthropic-compatible `/v1/messages`; streaming, adaptive thinking, `effort: "medium"`, prompt caching on the system prompt + tool list. |
| `providers/openai_compat.py` | `openai.AsyncOpenAI(base_url=…, api_key=…)` against llama-swap `/v1/chat/completions`; maps tool schemas to function definitions and tool calls back. |
| `agent/loop.py` | The agent loop (§3). |
| `agent/prompt.py` | System prompt: role, the command-first rule, the JSON-patch escape hatch, verify-by-render guidance when vision is on, deck text is data. |
| `ws.py` | `/ws/agent` endpoint: authenticates the cookie, checks `Origin`, one active turn per connection, routes messages. |
| `store.py` | SQLite (`aiosqlite`): `users`, `conversations(id, user_id, deck_key, title, created)`, `messages(id, conversation_id, role, content_json, created)`, `turns(id, conversation_id, prompt, provider, actions_json, slides_json, status, usage_json, started, ended)`. |
| `app.py` | FastAPI app: REST routes, WS route, static files for the wasm site at `/`. |

Provider config example (`server/config.toml`):

```toml
default_provider = "claude"

[providers.claude]
kind = "anthropic"
base_url = "${LITELLM_BASE_URL}"
api_key  = "${LITELLM_API_KEY}"
model    = "claude-sonnet-5-5"
vision   = true

[providers.qwen-local]
kind = "openai_compat"
base_url = "http://localhost:8080/v1"   # llama-swap
api_key  = ""
model    = "qwen3.8"
vision   = false

[limits]
turns_per_minute = 6
max_tool_calls_per_turn = 60
max_turn_seconds = 300
tool_call_timeout_seconds = 30
```

## 3. Agent behaviour

### 3.1 One turn

1. User sends a message. The browser sends `user_message {conversation_id, text, deck}` where
   `deck` is a short context: deck key, slide count, current slide index, selection, slide size.
2. Browser opens a checkpoint: records `history.undo.len()` and marks the turn `Running`. While
   running, the canvas ignores pointer and keyboard editing input (navigation and zoom still work)
   and shows a banner with Stop.
3. Backend appends the message, calls the selected provider with the system prompt, the
   conversation so far and the tool list advertised by this browser in `hello`.
4. Text deltas stream to the pane (`assistant_delta`). Each tool-use request becomes `tool_call`.
   The browser validates it against the allow-list and schema, executes it, and replies
   `tool_result {id, ok, content}`; `render_slide` results carry a PNG only when the provider has
   `vision = true`, otherwise the image is omitted and only structure is returned.
5. The loop ends when the model stops, the user presses Stop, or a limit in `[limits]` is hit.
6. Backend sends `turn_end {turn_id, summary, actions, slides, usage, status}`. The browser closes
   the checkpoint with `squash_since(depth, "AI: " + first line of the prompt)` (no entry when
   nothing changed) and stores `turn_id → (undo entry address, pre-turn snapshot)` in memory.

### 3.2 Reverting

- Ctrl+Z right after a turn undoes the whole turn (one undo entry); Ctrl+Y redoes it.
- "Revert to before this" on a turn card:
  - when the turn's entry is the newest undo entry: performs `edit.undo`;
  - otherwise: confirms (listing the later turns/edits that will be lost), then runs
    `doc.replace` with the pre-turn snapshot — itself one undoable step labelled
    "Revert AI turn N".
- After a page reload, turn cards are restored from the backend for reading; Revert is disabled
  for turns whose snapshot is no longer in memory.

### 3.3 Tools advertised to the model

From `mcp::tool_definitions()`, allow-listed: `list_commands`, `run_command`, `batch`,
`inspect_document`, `inspect_slide`, `render_slide` (no `path`), `add_slide`, `go_to_slide`,
`add_shape`, `add_text_box`, `set_text`, `insert_table`, `insert_chart`, `select`.

Excluded: `new_presentation`, `open_presentation`, `save_presentation`, `export`,
`insert_picture` (path-based), `pointer`, `select_tool`, `key`, `type_text`, `screenshot`,
`call_app`. `run_command`/`batch` refuse command ids in the `file.*` family and any command
parameter named `path`.

Added:

- `get_deck_json {scope: "deck" | "slide", slide?: index, pointer?: JSON Pointer}` — the deck
  model as JSON (the same structure as `presentation.json` in `.deckcraft`), optionally narrowed.
  Media bytes are replaced by references. Output capped (default 256 KiB) with a truncation note.
- `patch_deck_json {patch: RFC 6902 operations}` — applied to the full model JSON, then
  `doc.replace`. Invalid patch or failed validation returns an error and changes nothing.

The same allow-list is enforced in the browser executor (authoritative) and used by the backend
to filter what it passes to the model.

## 4. Protocol

WebSocket `/ws/agent`, JSON text frames, every message has `"v": 1` and `"type"`. Frames over
16 MiB close the connection.

Browser → server: `hello {tools, client_version}`, `user_message {conversation_id?, text, deck,
provider?}`, `tool_result {id, ok, content}`, `stop {}`.

Server → browser: `ready {user, providers: [{id, model, vision}], default_provider}`,
`conversation {id, title}`, `assistant_delta {text}`, `tool_call {id, name, input}`,
`turn_end {turn_id, summary, actions, slides, usage, status: ok|stopped|limit|error|interrupted}`,
`error {code, message}`.

REST: `POST /api/login`, `POST /api/logout`, `GET /api/me`,
`GET /api/conversations?deck=KEY`, `GET /api/conversations/{id}` (messages + turn cards).

`deck_key`: for a deck opened from a file, its file name plus the SHA-256 of the opened file's
bytes (so reopening the same file finds its conversations); for a new untitled deck, a random id
generated when the document is created. Saving under a new name does not change the key within the
session.

## 5. Security

- LLM base URLs and keys exist only on the server.
- Cookie auth on REST and WS; WS also checks `Origin` against the configured site origin.
- Passwords: argon2id; login rate-limited; admin-created accounts only.
- Agent scope is the open deck only (§3.3). Deck content reaches the model inside tool results and
  the system prompt marks it as data; the worst outcome of an injected instruction is a bad edit
  to the same deck, undone with one step.
- Per-user limits from `[limits]`; token usage per turn recorded in `turns.usage_json`.
- Browser side follows the project's never-crash rules: no `unwrap`/`expect`/`panic`, bounded
  decode sizes, all tool inputs treated as hostile.

## 6. Error handling

| Situation | Behaviour |
|---|---|
| Provider error after SDK retries (timeout, 429, 5xx) | `error` to the pane; turn ends `error`; changes made so far stay as one revertible turn |
| Tool fails | Returned to the model as an error result; the model may retry or explain |
| Browser does not answer a tool call within the timeout | Call fails; turn ends `error` |
| WebSocket drops mid-turn | Server cancels the turn (`interrupted`); on reconnect the browser closes any open checkpoint the same way |
| Limits reached | Turn ends `limit` with a message saying which limit |
| `patch_deck_json` invalid | Error result naming the failing operation or validation rule; deck unchanged |
| Not logged in / session expired | Pane shows the login form; WS closes with code 4401 |

## 7. Testing

- **Engine:** `squash_since` (empty turn, single step, many steps, redo cleared, byte budget
  accounting), `doc.replace` (round trip, invalid ids, dangling references, oversized/deep input,
  undo restores).
- **ui-egui:** `AgentState` transitions; tool executor allow-list and refusals; `LiveBackend`
  against a `SlideApp` with the sample deck; JSON patch path; egui_kittest snapshots of the pane
  (collapsed, logged out, idle, running, error).
- **`cargo xtask ci`** stays green, including the wasm check with `deckcraft-mcp` default features off.
- **Python (pytest):** config loading and env override; local auth (hashing, cookie, rate limit);
  store; both providers against recorded fake HTTP responses; agent loop with a scripted fake
  provider and a fake browser socket (tool round trip, stop, timeout, disconnect, limits).
- **End to end (Playwright):** server with a scripted fake provider + the built wasm site: log in,
  send a message; the fake provider inserts a shape, then calls `inspect_slide`, and the test
  asserts (through the backend's recorded tool results) that the shape exists; press Revert, the
  fake provider's next `inspect_slide` shows it gone. A screenshot diff of the canvas before/after
  confirms it was drawn.
- **Manual:** one session each against llama-swap Qwen 3.8 and LiteLLM Sonnet 5.5.

## 8. Build and run (v1)

- Web build from source: Nix dev shell with a Rust toolchain that has the `wasm32-unknown-unknown`
  target, `trunk`, and `wasm-bindgen-cli`; `trunk build --release` in `apps/deckcraft-web`.
- Backend: `cd server && uv run python -m deckcraft_server serve --site ../apps/deckcraft-web/dist`.
- First user: `uv run python -m deckcraft_server users add hannes --admin`.

## 9. Later

- Entra ID: an `auth/entra.py` OIDC provider behind `AuthProvider`.
- Desktop build: a native `AgentTransport` implementation (tungstenite) — the pane, state and
  executor are shared.
- Server-side deck snapshots per turn, so history and revert survive reloads.
- Shared decks, step 1: decks stored on the backend with an edit lock (one editor at a time,
  others read-only or take over), reusing auth and storage.
- Shared decks, step 2 (real-time co-editing): the server holds the authoritative deck in a
  headless engine; browsers send commands and receive updates. Out of v1 because the deck lives in
  one tab, the engine's undo is snapshot-based (no operation stream to sync), and concurrent edits
  need conflict resolution and a shared undo model.

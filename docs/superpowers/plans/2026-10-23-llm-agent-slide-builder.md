# LLM Agent Slide Builder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A collapsible agent chat pane in the DeckCraft web editor, backed by a Python server that runs an LLM agent (LiteLLM/Claude Sonnet 5.5 or llama-swap/Qwen 3.8) whose tool calls execute live on the browser's deck, with one undo step per turn and per-turn revert.

**Architecture:** The deck stays in the browser's wasm `Session`. A Python FastAPI server authenticates users, runs a provider-neutral agent loop and forwards each tool call over a WebSocket; the egui pane executes it through the existing `mcp::call_tool` → control dispatcher path and answers. Engine additions give one-step turn undo (`squash_undo_since`) and a JSON escape hatch (`doc.json`, `doc.patch`).

**Tech Stack:** Rust 1.95 (egui/eframe 0.36, wasm-bindgen 0.2.129, web-sys, trunk 0.21), Python 3.12+ (FastAPI, uvicorn, anthropic, openai, aiosqlite, argon2-cffi, itsdangerous, pytest), SQLite.

**Spec:** `docs/superpowers/specs/2026-10-23-llm-agent-slide-builder-design.md`

## Global Constraints

- Never crash: no `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!` in non-test Rust; `#![forbid(unsafe_code)]`; hostile-input bounds on every decoded message and tool input.
- Never break wasm: `cargo check -p deckcraft-web --target wasm32-unknown-unknown` must pass after every Rust task.
- Every user-visible engine action is a command with tests (`crates/engine/src/cmd/*`).
- Layering: `ui-egui` may depend on `mcp` only via an explicit `INTRA_LAYER_ORDER` entry `["mcp", "ui-egui"]`; nothing below L6 uses egui.
- LLM base URLs and keys exist only on the server.
- WebSocket frames > 16 MiB are refused on both ends.
- Default limits: 6 turns/min/user, 60 tool calls/turn, 300 s/turn, 30 s per tool call.
- Default Claude model id `claude-sonnet-5-5`; local model id `qwen3.8-27b-q5` at `http://127.0.0.1:8080/v1`.

## Review Focus

- A tool call arriving while no deck is open (start screen/backstage) → error tool result, no crash.
- The user closes or switches the document mid-turn → remaining tool calls fail cleanly; turn checkpoint is dropped, not squashed into another document.
- `patch_deck_json` with a patch that leaves dangling ids or huge arrays → refused by validation, deck unchanged.
- Server restarted / WebSocket dropped mid-turn → pane leaves the running state, canvas unlocks, partial changes squashed as "(interrupted)".
- Model returns malformed tool JSON or an unknown tool name → error tool result to the model, loop continues within limits.

---

### Task 1: Dev environment and baseline

**Files:**
- Create: `scripts/dev-env.sh`

- [ ] **Step 1:** Write `scripts/dev-env.sh` that puts the Nix `rustup` and `~/.cargo/bin` on PATH (`export PATH="$(nix build nixpkgs#rustup --no-link --print-out-paths)/bin:$HOME/.cargo/bin:$PATH"`, `export RUSTUP_TOOLCHAIN=1.95.0`).
- [ ] **Step 2:** `source scripts/dev-env.sh && cargo test -p deckcraft-engine --lib -q 2>&1 | tail -3` → all pass.
- [ ] **Step 3:** `cd apps/deckcraft-web && trunk build --release` → `dist/web/index.html` exists.
- [ ] **Step 4:** Commit `chore: dev env script`.

### Task 2: Engine — one undo step per agent turn

**Files:**
- Modify: `crates/engine/src/lib.rs` (add `Session::squash_undo_since`)
- Test: `crates/engine/src/tests.rs`

**Interfaces:**
- Produces: `impl Session { pub fn squash_undo_since(&mut self, uid: u64, before: &Arc<Presentation>, label: &str) -> bool }` — in document `uid`, finds the undo entry whose `doc` is `before` (pointer-equal), removes every entry above it, relabels it `label`, clears redo; returns `false` when the document is gone, the entry is not found, or nothing changed (`st.doc` is `before`).
- Produces: `impl Session { pub fn active_uid(&self) -> Option<u64>; pub fn active_doc_arc(&self) -> Option<Arc<Presentation>> }` (add only if absent).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn squash_undo_since_merges_a_turn_into_one_step() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let uid = s.active_uid().unwrap();
    let before = s.active_doc_arc().unwrap();
    s.execute("slide.new", &json!({"layout": "title"})).unwrap();
    s.execute("slide.new", &json!({"layout": "title"})).unwrap();
    s.execute("shape.insert", &json!({"preset": "rect", "rect": [10, 10, 100, 50]})).unwrap();
    assert!(s.squash_undo_since(uid, &before, "AI: test"));
    let h = &s.active().unwrap().history;
    assert_eq!(h.undo.last().unwrap().label, "AI: test");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(Arc::ptr_eq(&s.active_doc_arc().unwrap(), &before));
}

#[test]
fn squash_undo_since_without_changes_is_false() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let (uid, before) = (s.active_uid().unwrap(), s.active_doc_arc().unwrap());
    assert!(!s.squash_undo_since(uid, &before, "AI: nothing"));
}

#[test]
fn squash_undo_since_unknown_document_is_false() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let before = s.active_doc_arc().unwrap();
    assert!(!s.squash_undo_since(u64::MAX, &before, "x"));
}
```

- [ ] **Step 2:** `cargo test -p deckcraft-engine squash_undo -q` → FAIL (no method).
- [ ] **Step 3: Implement** in `impl Session`:

```rust
pub fn squash_undo_since(&mut self, uid: u64, before: &Arc<Presentation>, label: &str) -> bool {
    let Some(st) = self.documents_mut().iter_mut().find(|d| d.uid == uid) else { return false };
    if Arc::ptr_eq(&st.doc, before) { return false; }
    let Some(i) = st.history.undo.iter().rposition(|e| Arc::ptr_eq(&e.doc, before)) else { return false };
    st.history.undo.truncate(i + 1);
    if let Some(e) = st.history.undo.get_mut(i) { e.label = label.to_string(); }
    st.history.redo.clear();
    true
}
```
(Use the session's existing accessor for the document list; add `documents_mut` only if absent.)
- [ ] **Step 4:** Tests pass; `cargo clippy -p deckcraft-engine -- -D warnings` clean.
- [ ] **Step 5:** Commit `engine: squash_undo_since for agent turns`.

### Task 3: Engine — deck JSON read and patch commands

**Files:**
- Create: `crates/engine/src/cmd/doc.rs`
- Modify: `crates/engine/src/cmd/mod.rs` (register `doc::specs()`), `crates/engine/Cargo.toml` (add `json-patch = "4"` or a hand-written RFC 6902 subset if the crate does not build for wasm)
- Test: in `crates/engine/src/cmd/doc.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces commands:
  - `doc.json` (query): `{pointer?: JSON Pointer, maxBytes?: int ≤ 4 MiB, default 262144}` → `{json, bytes, truncated}`; `json` is `serde_json::to_value(&*doc)` narrowed by `pointer` (media `data` is not serialized by the model already).
  - `doc.patch` (undoable, label "Edit Document"): `{patch: [RFC 6902 ops]}` → `{ok: true, slides}`; applies to the full JSON, deserializes `Presentation`, copies media bytes from the old document by `MediaId`, runs `deckcraft_format::repair` and `sane_numbers`, rejects when: patch fails, JSON > 64 MiB, deserialize fails, or slide count > 10 000.
  - `doc.replace` (undoable, label "Replace Document"): `{json: <presentation JSON>}` → same validation path (used by Revert).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn doc_json_reads_a_slide() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("slide.new", &json!({"layout": "title", "title": "Hello"})).unwrap();
    let r = s.execute("doc.json", &json!({"pointer": "/slides/0"})).unwrap();
    assert!(r["json"].is_object());
    assert_eq!(r["truncated"], false);
}

#[test]
fn doc_patch_replaces_text_and_is_one_undo_step() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("slide.new", &json!({"layout": "title"})).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let r = s.execute("doc.patch", &json!({"patch": [{"op": "remove", "path": "/slides/0"}]})).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(s.active().unwrap().doc.slides.len(), before - 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
}

#[test]
fn doc_patch_rejects_invalid_and_keeps_the_deck() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let before = s.active_doc_arc().unwrap();
    assert!(s.execute("doc.patch", &json!({"patch": [{"op": "replace", "path": "/slides", "value": "nope"}]})).is_err());
    assert!(s.execute("doc.patch", &json!({"patch": [{"op": "remove", "path": "/nope/1"}]})).is_err());
    assert!(s.execute("doc.patch", &json!({"patch": "x"})).is_err());
    assert!(Arc::ptr_eq(&s.active_doc_arc().unwrap(), &before));
}

#[test]
fn doc_patch_keeps_media_bytes() {
    // Insert a picture from bytes (existing command used by the clipboard path), patch an
    // unrelated field, and assert the media item still has its bytes.
}
```
(The last test uses whichever existing command inserts picture bytes; look it up with `grep -n "insert.picture" crates/engine/src/cmd/insert.rs`.)

- [ ] **Step 2:** Run → FAIL (unknown command).
- [ ] **Step 3: Implement** `doc.rs` with `specs()` using `cmd!(query "doc.json", …)`, `cmd!("doc.patch", …)`, `cmd!("doc.replace", …)`; helpers `fn validated(old: &Presentation, v: Value) -> Result<Presentation>` and `fn narrow(v: &Value, pointer, max) -> (Value, usize, bool)` (on overflow return `{"truncated": true}` with a string preview of the first `max` bytes). Replace the document via `s.edit(|doc, _sel| { *doc = new; Ok(()) })`.
- [ ] **Step 4:** Tests pass; clippy clean; `cargo check -p deckcraft-web --target wasm32-unknown-unknown` passes.
- [ ] **Step 5:** Commit `engine: doc.json, doc.patch, doc.replace`.

### Task 4: mcp feature gate and layering

**Files:**
- Modify: `crates/mcp/Cargo.toml` (`[features] default = ["remote"]; remote = []`), `crates/mcp/src/lib.rs` (`#[cfg(feature = "remote")]` on `Remote`, `control_addr` stays), `crates/mcp/src/backend.rs` (gate `Remote` + `std::net` imports), `crates/mcp/src/tools.rs` (add `get_deck_json` and `patch_deck_json` tool definitions mapping to `doc.json` / `doc.patch` through `engine.execute`), `xtask/src/layers.rs` (`INTRA_LAYER_ORDER = &[&["mcp", "ui-egui"]]`), `crates/ui-egui/Cargo.toml` (`deckcraft-mcp = { workspace = true, default-features = false }`).
- Test: `crates/mcp/src/tests.rs`

- [ ] **Step 1: Failing test** — `call_tool(&mut Headless::default(), "get_deck_json", &json!({"pointer": "/slideSize"}))` returns non-error content; `patch_deck_json` with a remove of slide 0 on the sample deck reduces slide count.
- [ ] **Step 2:** Run `cargo test -p deckcraft-mcp -q` → FAIL.
- [ ] **Step 3:** Implement tool definitions + dispatch in `call_tool`.
- [ ] **Step 4:** `cargo test -p deckcraft-mcp -q`, `cargo run -p xtask -- layers`, wasm check → pass.
- [ ] **Step 5:** Commit `mcp: deck JSON tools, remote feature gate`.

### Task 5: ui-egui agent core (protocol, state, executor)

**Files:**
- Create: `crates/ui-egui/src/agent/mod.rs`, `protocol.rs`, `state.rs`, `tools.rs`, `live_backend.rs`, `tests.rs`
- Modify: `crates/ui-egui/src/lib.rs` (`pub mod agent;`)

**Interfaces:**
- `protocol.rs`: `#[serde(tag = "type", rename_all = "snake_case")] enum ClientMsg { Hello{v, tools, client_version}, UserMessage{v, conversation_id: Option<String>, text, deck: DeckContext, provider: Option<String>}, ToolResult{v, id, ok, content: Vec<ContentPart>}, Stop{v}, OpenConversation{v, deck_key} }`; `enum ServerMsg { Ready{user, providers: Vec<ProviderInfo>, default_provider}, Conversation{id, title}, AssistantDelta{text}, ToolCall{id, name, input: Value}, TurnEnd{turn_id, summary, actions: Vec<String>, slides: Vec<usize>, status}, History{conversation_id: Option<String>, items: Vec<HistoryItem>}, Error{code, message} }` with `HistoryItem{role, text, turn: Option<TurnInfo>}``; `enum ContentPart { Text{text}, Image{media_type, data_base64} }`; `pub const MAX_FRAME: usize = 16 << 20; pub fn decode(s: &str) -> Result<ServerMsg, String>` (refuses > MAX_FRAME).
- `transport.rs` (in `agent/`): `pub trait AgentTransport { fn send(&mut self, m: &ClientMsg); fn poll(&mut self) -> Vec<ServerMsg>; fn status(&self) -> LinkStatus; fn login(&mut self, user: &str, password: &str); fn logout(&mut self); }`, `enum LinkStatus { LoggedOut, Connecting, Connected, Failed(String) }`.
- `state.rs`: `pub struct AgentState { pub open: bool, pub messages: Vec<ChatItem>, pub turn: TurnState, pub providers: Vec<ProviderInfo>, pub provider: Option<String>, pub conversation_id: Option<String>, pub input: String, pub login_user: String, pub login_password: String, pub turns: Vec<TurnCard> }`; `enum TurnState { Idle, Running { uid: u64, before: Arc<Presentation>, prompt: String } }`; `pub struct TurnCard { pub turn_id: String, pub label: String, pub actions: Vec<String>, pub slides: Vec<usize>, pub status: String, pub uid: u64, pub before: Option<Arc<Presentation>>, pub after: Option<Arc<Presentation>> }`; `enum ChatItem { User(String), Assistant(String), Tool{name, ok, summary}, Notice(String) }`.
- `tools.rs`: `pub const ALLOWED_TOOLS: &[&str]` (spec §3.3 list + `get_deck_json`, `patch_deck_json`); `pub fn advertised() -> Vec<Value>` (filter `mcp::tool_definitions()`); `pub fn check(name: &str, input: &Value) -> Result<(), String>` (allow-list; for `run_command`/`batch` refuse `file.*`, `app.*` and any `path` param, recursively in `batch.commands`); `pub fn execute(app: &mut SlideApp, ctx: &egui::Context, name: &str, input: &Value, vision: bool) -> (bool, Vec<ContentPart>)`.
- `live_backend.rs`: `pub struct LiveBackend<'a> { app: &'a mut SlideApp, ctx: &'a egui::Context }` implementing `deckcraft_mcp::Backend::call` via `control::handle` with a fresh `ControlRequest`; `Outcome::Screenshot` → `Err("not available")`; `has_ui` → `false` (so app-only tools refuse).

- [ ] **Step 1: Failing tests** (`agent/tests.rs`, native, using `SlideApp::new(Session::new(), Services::default())` and `egui::Context::default()`):

```rust
#[test] fn check_refuses_file_tools_and_paths() {
    assert!(tools::check("save_presentation", &json!({})).is_err());
    assert!(tools::check("run_command", &json!({"command": "file.save", "params": {}})).is_err());
    assert!(tools::check("run_command", &json!({"command": "insert.picture", "params": {"path": "/etc/passwd"}})).is_err());
    assert!(tools::check("batch", &json!({"commands": [{"command": "file.open", "params": {}}]})).is_err());
    assert!(tools::check("run_command", &json!({"command": "slide.new", "params": {}})).is_ok());
}
#[test] fn execute_runs_on_the_live_session() {
    let (mut app, ctx) = test_app_with_sample();
    let n = app.session.active().unwrap().doc.slides.len();
    let (ok, _) = tools::execute(&mut app, &ctx, "run_command", &json!({"command": "slide.new", "params": {"layout": "title"}}), false);
    assert!(ok);
    assert_eq!(app.session.active().unwrap().doc.slides.len(), n + 1);
}
#[test] fn execute_without_document_is_an_error_not_a_panic() {
    let mut app = SlideApp::new(Session::new(), Services::default());
    let (ok, _) = tools::execute(&mut app, &egui::Context::default(), "inspect_document", &json!({}), false);
    assert!(!ok);
}
#[test] fn decode_refuses_oversized_and_unknown() {
    assert!(protocol::decode(&"x".repeat(protocol::MAX_FRAME + 1)).is_err());
    assert!(protocol::decode(r#"{"type":"nope"}"#).is_err());
    assert!(matches!(protocol::decode(r#"{"type":"assistant_delta","text":"hi"}"#), Ok(ServerMsg::AssistantDelta{..})));
}
#[test] fn render_slide_image_only_with_vision() { /* execute render_slide with vision false → no Image part; true → Image part */ }
```

- [ ] **Step 2:** Run `cargo test -p deckcraft-ui-egui agent:: -q` → FAIL.
- [ ] **Step 3:** Implement the modules as specified.
- [ ] **Step 4:** Tests pass; clippy; wasm check.
- [ ] **Step 5:** Commit `ui: agent protocol, state and tool executor`.

### Task 6: ui-egui agent pane, turn lifecycle and input lock

**Files:**
- Create: `crates/ui-egui/src/agent/pane.rs`, `crates/ui-egui/src/agent/client.rs`
- Modify: `crates/ui-egui/src/lib.rs` (field `pub agent: Option<agent::client::AgentClient>`, `SlideApp::set_agent_transport(Box<dyn AgentTransport>)`; in `ui()`: call `agent::client::tick(self, &ctx)` before panels; show `agent::pane::show(self, ui)` as `egui::Panel::right("agent_pane")` before the outer `CentralPanel`; skip `self.keyboard(&ctx)` and draw the lock overlay when `agent_busy()`), `crates/ui-egui/src/ribbon.rs` (a "Agent" toggle button on the View tab and the title bar, running `ui.agent.toggle`), `UI_COMMANDS` (`ui.agent.toggle`).

**Interfaces:**
- `AgentClient { pub state: AgentState, transport: Box<dyn AgentTransport>, hello_sent: bool }`.
- `pub fn tick(app: &mut SlideApp, ctx: &egui::Context)`: takes the client out of `app.agent`, sends `Hello` once `Connected` and `OpenConversation{deck_key}` after `Ready` and whenever the active document changes, drains `poll()`; for `ToolCall` → `tools::check` then `tools::execute` (max one per frame; the rest stay queued in `state.pending`), sends `ToolResult`; for `TurnEnd` → `squash_undo_since(uid, &before, "AI: " + first line)`, push `TurnCard` with `after = active_doc_arc()`, set `Idle`; for transport `Failed`/disconnect while `Running` → squash with " (interrupted)" and `Idle`; requests repaint while running; puts the client back.
- `pub fn send_prompt(client, app)`: builds `DeckContext { deck_key, slides, current, selection, slide_size }`, stores `Running{uid, before, prompt}`, sends `UserMessage`.
- `pub fn revert(app, card_index)`: if newest undo entry's doc is `card.after` and `active_doc == card.after` → `edit.undo`; else `doc.replace` with `serde_json::to_value(&*card.before)` and label "Revert AI turn"; disabled when `before` is `None`.
- `pane::show`: header (title, provider combo, collapse ×), login form when `LoggedOut`, scrolling chat (user/assistant/tool/notice rows; turn cards with action list and Revert button), multiline input (Enter sends, Shift+Enter newline), Send/Stop button. Theme tokens only.
- `deck_key`: `format!("{}#{:016x}", title, hash)` where `hash` is from the file path + uid for opened files, random per uid for untitled (store in `AgentState.deck_keys: HashMap<u64, String>`).

- [ ] **Step 1: Failing tests** with a `FakeTransport` (records sent msgs, returns scripted `ServerMsg`s):
  - `turn_with_two_tool_calls_is_one_undo_step` — script `Ready`, then after `send_prompt`: `ToolCall slide.new`, `ToolCall shape.insert`, `TurnEnd`; tick until idle; assert two `ToolResult`s sent with `ok: true`, undo top label starts with `"AI: "`, one `edit.undo` restores the pre-turn doc.
  - `revert_latest_turn_undoes` and `revert_older_turn_replaces_document`.
  - `disconnect_mid_turn_unlocks_and_squashes_interrupted`.
  - `refused_tool_reports_error_and_changes_nothing`.
  - `tool_call_while_switched_document_fails_cleanly` (switch active doc mid-turn → result `ok: false`).
  - kittest snapshot `agent_pane_logged_out` and `agent_pane_running` (follow existing `egui_kittest` usage in `selpane_tests.rs`).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p deckcraft-ui-egui -q` (whole crate, to catch regressions), clippy, wasm check.
- [ ] **Step 5:** Commit `ui: agent pane, turn checkpoints and revert`.

### Task 7: Web transport (WebSocket + fetch) and wiring

**Files:**
- Create: `apps/deckcraft-web/src/agent_ws.rs`
- Modify: `apps/deckcraft-web/src/web.rs` (create `WebAgentTransport` and call `app.set_agent_transport`), `apps/deckcraft-web/Cargo.toml` (web-sys features `WebSocket`, `MessageEvent`, `CloseEvent`, `ErrorEvent`, `Request`, `RequestInit`, `RequestCredentials`, `Response`, `Headers`), `apps/deckcraft-web/index.html` (no change unless needed).

**Interfaces:**
- `WebAgentTransport { inner: Rc<RefCell<Inner>>, ctx: egui::Context }` implementing `AgentTransport`. On construction: `GET /api/me` → if 200 open `ws(s)://<host>/ws/agent`, else `LoggedOut`. `login` → `POST /api/login` JSON `{username, password}` with `credentials: same-origin`, then connect. Incoming text frames → `protocol::decode` into a queue; every event calls `ctx.request_repaint()`. Close → `Failed("disconnected")`, auto-reconnect after 2 s when previously connected. If the page was not served by the agent server (`/api/me` 404/network error) → `LinkStatus::Failed("no agent server")` and the pane says so.

- [ ] **Step 1:** Implement (wasm-only code; logic tested via Task 6 fakes and Task 10 end-to-end).
- [ ] **Step 2:** `cargo check -p deckcraft-web --target wasm32-unknown-unknown`, `cargo clippy --target wasm32-unknown-unknown -p deckcraft-web -- -D warnings`, `trunk build --release`.
- [ ] **Step 3:** Commit `web: agent WebSocket transport`.

### Task 8: Python server — config, store, auth, CLI

**Files:**
- Create: `server/pyproject.toml`, `server/deckcraft_server/{__init__,__main__,config,store,cli,app}.py`, `server/deckcraft_server/auth/{__init__,base,local}.py`, `server/config.example.toml`, `server/tests/{conftest,test_config,test_store,test_auth}.py`, `server/README.md`

**Interfaces:**
- `config.load(path: Path | None) -> Settings` (pydantic models `Settings{default_provider, providers: dict[str, ProviderCfg], limits: Limits, site_dir, db_path, session_secret, origin}`, `ProviderCfg{kind: Literal["anthropic","openai_compat"], base_url, api_key, model, vision: bool}`); `${VAR}` expansion; env overrides `LITELLM_BASE_URL`, `LITELLM_API_KEY`, `AGENT_DEFAULT_PROVIDER`, `DECKCRAFT_SITE_DIR`, `DECKCRAFT_DB`, `SESSION_SECRET`.
- `store.Store(path)`: `async init()`, users (`add_user`, `get_user`, `set_password`, `disable`), conversations (`create_conversation(user_id, deck_key, title)`, `list_conversations(user_id, deck_key)`, `get_conversation`), messages (`append_message(cid, role, content)`, `messages(cid)`), turns (`start_turn`, `finish_turn(turn_id, actions, slides, status, usage)`).
- `auth.base.AuthProvider` protocol: `async current_user(request) -> User | None`, `router() -> APIRouter`. `auth.local.LocalAuth(store, secret)`: argon2id, cookie `dc_session` signed with `itsdangerous.TimestampSigner` (max age 14 days), `POST /api/login`, `POST /api/logout`, `GET /api/me`; 5 failed logins per user+IP per 10 min → 429.
- `cli`: `users add NAME [--admin] [--password-stdin]`, `users passwd NAME`, `users disable NAME`, `serve [--host] [--port] [--config]`.

- [ ] **Step 1: Failing tests** — config env override and `${VAR}` expansion; store CRUD roundtrip on a temp DB; login good/bad password, cookie grants `/api/me`, rate limit returns 429 on the 6th failure, disabled user refused.
- [ ] **Step 2:** `cd server && uv run pytest -q` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Tests pass.
- [ ] **Step 5:** Commit `server: config, store, local auth, CLI`.

### Task 9: Python server — providers, agent loop, WebSocket

**Files:**
- Create: `server/deckcraft_server/providers/{__init__,base,anthropic_provider,openai_compat}.py`, `server/deckcraft_server/agent/{__init__,loop,prompt}.py`, `server/deckcraft_server/ws.py`, `server/tests/{test_providers,test_loop,test_ws}.py`
- Modify: `server/deckcraft_server/app.py` (mount WS, conversations REST, static site with `index.html` fallback)

**Interfaces:**
- Neutral format: `Msg = {"role": "user"|"assistant"|"tool", ...}` with blocks `TextBlock(text)`, `ToolUse(id, name, input)`, `ToolResultBlock(tool_use_id, ok, parts: list[Text|Image])`.
- `LLMProvider.stream(system: str, messages: list[Msg], tools: list[dict]) -> AsyncIterator[Event]` with `TextDelta(text)`, `ToolUseEvent(id, name, input)`, `Done(stop_reason, usage)`.
- `AnthropicProvider` — `AsyncAnthropic(base_url, api_key).messages.stream(model, max_tokens=16000, system=[{type:text, text, cache_control: ephemeral}], tools=[{name, description, input_schema}], messages=…, thinking={"type": "adaptive"}, output_config={"effort": "medium"})`; maps tool results to `tool_result` blocks with `is_error` and image blocks.
- `OpenAICompatProvider` — `AsyncOpenAI(base_url, api_key or "none").chat.completions.create(model, messages, tools=[{type: function, function:{name, description, parameters}}], stream=True)`; accumulates streamed tool-call deltas; images dropped when `vision=False`.
- `agent.loop.run_turn(provider, history, tools, browser: BrowserLink, limits, on_delta) -> TurnResult(actions, slides, status, usage)`; `BrowserLink.call_tool(id, name, input, timeout) -> (ok, parts)`; stop via `asyncio.Event`.
- `ws.py`: on `open_conversation {deck_key}` reply `history` with the user's latest conversation for that deck (messages as text + turn cards, Revert disabled client-side); authenticate cookie before accept, check `Origin` host matches `Host`, per-connection state (tools from `hello`, active turn task), routing per spec §4, frame cap 16 MiB, per-user turn rate limit.

- [ ] **Step 1: Failing tests** — `test_loop.py` with `ScriptedProvider` (yields scripted events per call) and `FakeBrowser`: (a) text-only turn ends `ok`; (b) two tool calls round-trip and results feed the next provider call; (c) unknown tool → error result, loop continues; (d) tool timeout → status `error`; (e) stop event → `stopped`; (f) `max_tool_calls` → `limit`. `test_providers.py`: anthropic and openai adapters against `httpx.MockTransport`-recorded SSE bodies (one text + one tool_use each). `test_ws.py`: FastAPI `TestClient` websocket: unauthenticated → close 4401; authenticated hello → ready with providers; a scripted turn delivers `tool_call` and accepts `tool_result`, ends with `turn_end`; history persisted.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `uv run pytest -q` all pass.
- [ ] **Step 5:** Commit `server: providers, agent loop, websocket`.

### Task 10: Integration, run script, end-to-end in the browser

**Files:**
- Create: `scripts/run-agent-dev.sh` (build site with trunk if stale, create `server/.env`/`config.toml` from example if missing, start uvicorn on `127.0.0.1:8770`), `server/config.toml` (local: providers `qwen-local` default + `claude` from env)
- Modify: `server/README.md` (setup, users, config, run)

- [ ] **Step 1:** `scripts/run-agent-dev.sh` runs; `curl -s 127.0.0.1:8770/api/me` → 401.
- [ ] **Step 2:** Create user `hannes` (password via `--password-stdin` from a generated value stored in `server/.dev-password`, gitignored).
- [ ] **Step 3:** Co-browse (Claude in Chrome): open `http://localhost:8770/`, log in, open the Agent pane, select `qwen-local`, ask "Add a slide titled 'Roadmap' with three bullet points"; verify slide appears live, turn card lists actions, Ctrl+Z removes the whole turn, Ctrl+Y restores, Revert works, a second turn edits the result ("make the title red"), and Stop works mid-turn. Record a GIF.
- [ ] **Step 4:** PowerPoint round trip: open a `.pptx` via File › Open (upload in co-browse), ask the agent to change it, Save As `.pptx`, reopen and confirm the change.
- [ ] **Step 5:** Fix anything found (with a regression test per fix), run `cargo xtask ci` (or the closest subset that works offline) and `uv run pytest -q`.
- [ ] **Step 6:** Commit `agent: dev run script and docs`.

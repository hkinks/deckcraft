# DeckCraft agent server

Serves the DeckCraft web editor and an AI agent that edits the open deck from a chat pane.
The deck stays in the browser; the server runs the agent loop and sends each tool call to the
editor over a WebSocket (`/ws/agent`), where it runs through the same commands as the ribbon.
Every agent request is one undo step, and each turn in the chat can be reverted.

Design: `docs/superpowers/specs/2026-10-23-llm-agent-slide-builder-design.md`.

## Run (development)

```sh
scripts/run-agent-dev.sh                       # builds dist/web with trunk if missing, serves :8770
cd server && uv run python -m deckcraft_server users add NAME --admin
```

Open http://127.0.0.1:8770/, sign in in the agent pane (★ in the title bar toggles it).
Building the web editor needs Rust with the `wasm32-unknown-unknown` target, `trunk` and
`wasm-bindgen-cli` 0.2.129 (`source scripts/dev-env.sh` on NixOS), and the fonts repo at
`../craft-fonts` (`git clone https://github.com/storytold/craft-fonts ../craft-fonts`).

## Configuration

`server/config.toml` (copied from `config.example.toml` on first run). Providers:

- `kind = "anthropic"`: Claude through LiteLLM's Anthropic-compatible `/v1/messages`
  (`LITELLM_BASE_URL`, `LITELLM_API_KEY`), default model `claude-sonnet-5-5`, vision on.
- `kind = "openai_compat"`: any OpenAI-compatible `/v1/chat/completions`, e.g. llama-swap
  serving Qwen at `http://127.0.0.1:8080/v1`.

The URLs and keys never reach the browser; it only sees provider ids, model names and whether a
model can look at images.

## Web search

The agent can search the web and read pages (`web_search`, `web_fetch`), run here on the server.
DuckDuckGo needs no key; set `[web]` in `config.toml` for SearXNG (`search = "searxng"`,
`base_url`), Brave or Tavily (`api_key`), or `enabled = false` to turn it off. `web_fetch` reads
public http(s) pages only: local and private addresses are refused, on every redirect too.

## Fonts on demand

`scripts/fetch-fonts.py` downloads 36 SIL OFL families (Inter, Montserrat, Lato, Merriweather…,
plus Carlito, Caladea, Arimo, Tinos and Cousine, metric-compatible with Calibri, Cambria, Arial,
Times New Roman and Courier New) into `server/fonts/`. The server serves them at `/fonts/`; the
web editor lists them in the font menu and loads a family when a deck uses it.

## Workspace folders

In the agent pane's Files tab (Chromium browsers), a presentation can get a workspace folder:
briefs, notes, data, pictures, draft decks and templates. The person can open decks, insert
pictures and apply a template's design from it; the agent lists and reads its files (PDFs are
turned into text here on the server), inserts its pictures and applies its templates. An
`AGENT.md` at the folder's root holds project instructions sent with every request. Each
presentation has its own folder (a new one starts without); files never leave the browser except
what the agent reads. Try `examples/workspace-demo/`.

## Users

```sh
uv run python -m deckcraft_server users add NAME [--admin] [--password-stdin]
uv run python -m deckcraft_server users passwd NAME
uv run python -m deckcraft_server users disable NAME
```

Passwords are argon2id hashes in SQLite (`data/deckcraft.db`); sign-in sets an HttpOnly session
cookie. Microsoft Entra ID can replace local accounts behind `auth/base.py`'s interface.

## Tests

```sh
uv run pytest -q
```

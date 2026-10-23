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

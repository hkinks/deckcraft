#!/usr/bin/env bash
# Run the DeckCraft web editor with the agent server on http://127.0.0.1:8770/.
#   scripts/run-agent-dev.sh            # build the site if needed, then serve
#   scripts/run-agent-dev.sh --rebuild  # force a fresh trunk build
# First user: (cd server && uv run python -m deckcraft_server users add NAME --admin)
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${1:-}" = "--rebuild" ] || [ ! -f "$root/dist/web/index.html" ]; then
  # shellcheck disable=SC1091
  source "$root/scripts/dev-env.sh"
  (cd "$root/apps/deckcraft-web" && trunk build --release)
fi
cd "$root/server"
[ -f config.toml ] || cp config.example.toml config.toml
# A stable session secret for development, so a restart doesn't sign everyone out.
[ -f .session-secret ] || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > .session-secret
export SESSION_SECRET="${SESSION_SECRET:-$(cat .session-secret)}"
exec uv run python -m deckcraft_server serve --host "${HOST:-127.0.0.1}" --port "${PORT:-8770}"

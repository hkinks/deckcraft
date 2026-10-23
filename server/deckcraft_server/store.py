"""SQLite persistence: users, conversations, messages and agent turns.

Each operation opens its own connection, so the store works from any event loop (uvicorn,
TestClient, CLI) and needs no shared connection state.
"""

from __future__ import annotations

import json
import time
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path

import aiosqlite

SCHEMA = """
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    username TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    admin INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,
    created REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS user_settings (
    user_id INTEGER PRIMARY KEY REFERENCES users(id),
    instructions TEXT
);
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    deck_key TEXT NOT NULL,
    title TEXT NOT NULL,
    created REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS conversations_by_deck ON conversations(user_id, deck_key, created);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    role TEXT NOT NULL,
    content_json TEXT NOT NULL,
    created REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    prompt TEXT NOT NULL,
    provider TEXT NOT NULL,
    actions_json TEXT NOT NULL DEFAULT '[]',
    slides_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'running',
    usage_json TEXT NOT NULL DEFAULT '{}',
    started REAL NOT NULL,
    ended REAL
);
"""


@dataclass
class User:
    id: int
    username: str
    password_hash: str
    admin: bool
    disabled: bool


def _new_id() -> str:
    import secrets

    return secrets.token_hex(12)


class Store:
    def __init__(self, path: Path | str):
        self.path = Path(path)

    @asynccontextmanager
    async def _db(self):
        async with aiosqlite.connect(self.path) as db:
            db.row_factory = aiosqlite.Row
            await db.execute("PRAGMA foreign_keys = ON")
            yield db

    async def init(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        async with self._db() as db:
            await db.executescript(SCHEMA)
            await db.commit()

    async def close(self) -> None:
        """Nothing to close: connections are per operation."""

    # Users

    async def add_user(self, username: str, password_hash: str, admin: bool = False) -> int:
        async with self._db() as db:
            cur = await db.execute(
                "INSERT INTO users(username, password_hash, admin, created) VALUES (?, ?, ?, ?)",
                (username, password_hash, int(admin), time.time()),
            )
            await db.commit()
            return int(cur.lastrowid or 0)

    async def get_user(self, username: str) -> User | None:
        async with self._db() as db:
            cur = await db.execute("SELECT * FROM users WHERE username = ?", (username,))
            row = await cur.fetchone()
        if row is None:
            return None
        return User(row["id"], row["username"], row["password_hash"], bool(row["admin"]), bool(row["disabled"]))

    async def set_password(self, username: str, password_hash: str) -> None:
        async with self._db() as db:
            await db.execute("UPDATE users SET password_hash = ? WHERE username = ?", (password_hash, username))
            await db.commit()

    async def disable(self, username: str) -> None:
        async with self._db() as db:
            await db.execute("UPDATE users SET disabled = 1 WHERE username = ?", (username,))
            await db.commit()

    # Conversations and messages

    async def get_instructions(self, user_id: int) -> str | None:
        async with self._db() as db:
            async with db.execute("SELECT instructions FROM user_settings WHERE user_id = ?", (user_id,)) as cur:
                row = await cur.fetchone()
        return row[0] if row else None

    async def set_instructions(self, user_id: int, text: str | None) -> None:
        async with self._db() as db:
            await db.execute(
                "INSERT INTO user_settings (user_id, instructions) VALUES (?, ?) "
                "ON CONFLICT(user_id) DO UPDATE SET instructions = excluded.instructions",
                (user_id, text),
            )
            await db.commit()

    async def create_conversation(self, user_id: int, deck_key: str, title: str) -> str:
        cid = _new_id()
        async with self._db() as db:
            await db.execute(
                "INSERT INTO conversations(id, user_id, deck_key, title, created) VALUES (?, ?, ?, ?, ?)",
                (cid, user_id, deck_key, title[:200], time.time()),
            )
            await db.commit()
        return cid

    async def list_conversations(self, user_id: int, deck_key: str) -> list[dict]:
        async with self._db() as db:
            cur = await db.execute(
                "SELECT id, title, created FROM conversations WHERE user_id = ? AND deck_key = ? ORDER BY created DESC",
                (user_id, deck_key),
            )
            return [dict(r) for r in await cur.fetchall()]

    async def get_conversation(self, cid: str) -> dict | None:
        async with self._db() as db:
            cur = await db.execute("SELECT * FROM conversations WHERE id = ?", (cid,))
            row = await cur.fetchone()
        return dict(row) if row else None

    async def append_message(self, cid: str, role: str, content: dict) -> None:
        async with self._db() as db:
            await db.execute(
                "INSERT INTO messages(conversation_id, role, content_json, created) VALUES (?, ?, ?, ?)",
                (cid, role, json.dumps(content), time.time()),
            )
            await db.commit()

    async def messages(self, cid: str) -> list[dict]:
        async with self._db() as db:
            cur = await db.execute(
                "SELECT role, content_json, created FROM messages WHERE conversation_id = ? ORDER BY id", (cid,)
            )
            rows = await cur.fetchall()
        return [{"role": r["role"], "content": json.loads(r["content_json"]), "created": r["created"]} for r in rows]

    # Turns

    async def start_turn(self, cid: str, prompt: str, provider: str) -> str:
        tid = _new_id()
        async with self._db() as db:
            await db.execute(
                "INSERT INTO turns(id, conversation_id, prompt, provider, started) VALUES (?, ?, ?, ?, ?)",
                (tid, cid, prompt, provider, time.time()),
            )
            await db.commit()
        return tid

    async def finish_turn(self, tid: str, actions: list[str], slides: list[int], status: str, usage: dict) -> None:
        async with self._db() as db:
            await db.execute(
                "UPDATE turns SET actions_json = ?, slides_json = ?, status = ?, usage_json = ?, ended = ? WHERE id = ?",
                (json.dumps(actions), json.dumps(slides), status, json.dumps(usage), time.time(), tid),
            )
            await db.commit()

    async def turns(self, cid: str) -> list[dict]:
        async with self._db() as db:
            cur = await db.execute("SELECT * FROM turns WHERE conversation_id = ? ORDER BY started", (cid,))
            rows = await cur.fetchall()
        return [
            {
                "id": r["id"],
                "prompt": r["prompt"],
                "provider": r["provider"],
                "actions": json.loads(r["actions_json"]),
                "slides": json.loads(r["slides_json"]),
                "status": r["status"],
                "usage": json.loads(r["usage_json"]),
                "started": r["started"],
                "ended": r["ended"],
            }
            for r in rows
        ]

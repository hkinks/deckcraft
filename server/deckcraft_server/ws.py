"""`/ws/agent`: one browser editor ↔ the agent loop. Protocol in the design spec §4."""

from __future__ import annotations

import asyncio
import json
import logging
import time
from collections import defaultdict, deque
from urllib.parse import urlparse

from fastapi import WebSocket, WebSocketDisconnect

from .agent.loop import flatten_history, run_turn
from .agent.prompt import SYSTEM_PROMPT, editor_context
from .config import Settings
from .providers.base import AssistantMsg, HistoryItem, UserMsg
from .store import Store, User

log = logging.getLogger("deckcraft.ws")

MAX_FRAME = 16 << 20
MAX_TOOLS = 256


class RateLimiter:
    def __init__(self):
        self.hits: dict[int, deque[float]] = defaultdict(deque)

    def allow(self, user_id: int, per_minute: int) -> bool:
        q = self.hits[user_id]
        now = time.monotonic()
        while q and now - q[0] > 60:
            q.popleft()
        if len(q) >= per_minute:
            return False
        q.append(now)
        return True


class Connection:
    # Tool calls time out only while the editor tab is visible: a hidden tab draws no frames,
    # so it runs nothing until it is shown again (bounded by the turn's time limit).
    manages_timeout = True

    def __init__(self, ws: WebSocket, user: User, settings: Settings, store: Store, providers: dict, limiter: RateLimiter):
        self.ws = ws
        self.user = user
        self.settings = settings
        self.store = store
        self.providers = providers
        self.limiter = limiter
        self.tools: list[dict] | None = None
        self.conversation_id: str | None = None
        self.deck_key: str | None = None
        self.history: list[HistoryItem] = []
        self.turn_task: asyncio.Task | None = None
        self.stop = asyncio.Event()
        self.pending: dict[str, asyncio.Future] = {}
        self.send_lock = asyncio.Lock()
        self.hidden = False
        self.closed = False

    async def send(self, msg: dict) -> None:
        if self.closed:
            return
        async with self.send_lock:
            try:
                await self.ws.send_text(json.dumps(msg))
            except (WebSocketDisconnect, RuntimeError, OSError):
                self.closed = True  # the editor went away; the turn still finishes and is stored

    async def error(self, code: str, message: str) -> None:
        await self.send({"type": "error", "code": code, "message": message})

    # BrowserLink
    async def call_tool(self, id: str, name: str, input: dict) -> tuple[bool, list[dict]]:
        fut: asyncio.Future = asyncio.get_running_loop().create_future()
        self.pending[id] = fut
        try:
            await self.send({"type": "tool_call", "id": id, "name": name, "input": input})
            limits = self.settings.limits
            visible_left = limits.tool_call_timeout_seconds
            hidden_left = limits.max_turn_seconds
            step = min(0.25, visible_left)
            while True:
                try:
                    return await asyncio.wait_for(asyncio.shield(fut), step)
                except asyncio.TimeoutError:
                    if self.hidden:
                        hidden_left -= step
                    else:
                        visible_left -= step
                    if visible_left <= 0 or hidden_left <= 0:
                        raise
        finally:
            self.pending.pop(id, None)

    async def handle(self, msg: dict) -> None:
        t = msg.get("type")
        if t == "hello":
            tools = msg.get("tools")
            if not isinstance(tools, list) or len(tools) > MAX_TOOLS or not all(isinstance(x, dict) and isinstance(x.get("name"), str) for x in tools):
                return await self.error("bad_message", "hello.tools must be a list of tool definitions")
            self.tools = tools
            await self.send({
                "type": "ready",
                "user": self.user.username,
                "providers": [{"id": k, "model": v.model, "vision": v.vision} for k, v in self.settings.providers.items()],
                "default_provider": self.settings.default_provider,
            })
        elif t == "open_conversation":
            await self.open_conversation(str(msg.get("deck_key", ""))[:300])
        elif t == "user_message":
            await self.user_message(msg)
        elif t == "tool_result":
            fut = self.pending.get(str(msg.get("id")))
            if fut and not fut.done():
                content = msg.get("content") if isinstance(msg.get("content"), list) else []
                parts = [p for p in content if isinstance(p, dict) and p.get("type") in ("text", "image")]
                fut.set_result((bool(msg.get("ok")), parts))
        elif t == "stop":
            self.stop.set()
        elif t == "new_conversation":
            if self.turn_task and not self.turn_task.done():
                return await self.error("busy", "a turn is running")
            self.conversation_id, self.history = None, []
        elif t == "visibility":
            self.hidden = bool(msg.get("hidden"))
        else:
            await self.error("bad_message", f"unknown message type {t!r}")

    async def open_conversation(self, deck_key: str) -> None:
        if self.turn_task and not self.turn_task.done():
            return await self.error("busy", "a turn is running")
        self.deck_key = deck_key
        convs = await self.store.list_conversations(self.user.id, deck_key)
        if not convs:
            self.conversation_id, self.history = None, []
            return await self.send({"type": "history", "conversation_id": None, "items": []})
        cid = convs[0]["id"]
        self.conversation_id = cid
        turns = {t["id"]: t for t in await self.store.turns(cid)}
        items, history = [], []
        for m in await self.store.messages(cid):
            text = m["content"].get("text", "")
            item = {"role": m["role"], "text": text}
            if m["role"] == "user":
                history.append(UserMsg(text))
            else:
                history.append(AssistantMsg(text or "(no reply)", [], None, "history"))
                if (tid := m["content"].get("turn_id")) in turns:
                    tr = turns[tid]
                    item["turn"] = {"turn_id": tid, "actions": tr["actions"], "slides": tr["slides"], "status": tr["status"], "prompt": tr["prompt"]}
            items.append(item)
        self.history = history
        await self.send({"type": "history", "conversation_id": cid, "items": items})

    async def user_message(self, msg: dict) -> None:
        if self.tools is None:
            return await self.error("no_hello", "send hello first")
        if self.turn_task and not self.turn_task.done():
            return await self.error("busy", "a turn is already running; press Stop or wait")
        text = msg.get("text")
        if not isinstance(text, str) or not text.strip():
            return await self.error("bad_message", "empty message")
        provider_id = msg.get("provider") or self.settings.default_provider
        if not isinstance(provider_id, str) or provider_id not in self.settings.providers:
            return await self.error("bad_provider", f"unknown provider {provider_id!r}")
        if not self.limiter.allow(self.user.id, self.settings.limits.turns_per_minute):
            return await self.error("rate_limited", "too many requests, wait a moment")
        deck = msg.get("deck") if isinstance(msg.get("deck"), dict) else {}
        deck_key = str(deck.get("deck_key") or self.deck_key or "untitled")[:300]
        if self.conversation_id is None or deck_key != self.deck_key:
            if deck_key != self.deck_key:
                self.history = []
            self.deck_key = deck_key
            self.conversation_id = await self.store.create_conversation(self.user.id, deck_key, text.strip()[:60])
            await self.send({"type": "conversation", "id": self.conversation_id, "title": text.strip()[:60]})
        else:
            await self.send({"type": "conversation", "id": self.conversation_id, "title": ""})
        self.stop = asyncio.Event()
        self.turn_task = asyncio.create_task(self.run(text.strip()[:20000], deck, provider_id))

    async def run(self, text: str, deck: dict, provider_id: str) -> None:
        cid = self.conversation_id or ""
        await self.store.append_message(cid, "user", {"text": text})
        tid = await self.store.start_turn(cid, text, provider_id)
        await self.send({"type": "turn_start", "turn_id": tid})
        ctx = editor_context(deck)
        self.history = flatten_history(self.history)
        self.history.append(UserMsg(f"{ctx}\n\n{text}" if ctx else text))
        provider = self.providers[provider_id]
        status, result = "error", None
        try:
            result = await run_turn(
                provider, SYSTEM_PROMPT, self.history, self.tools or [], self, self.settings.limits,
                lambda d: self.send({"type": "assistant_delta", "text": d}), self.stop,
            )
            status = result.status
            if result.status == "error":
                await self.error("turn_failed", result.error)
        except asyncio.CancelledError:
            await self.store.append_message(cid, "assistant", {"text": "(interrupted)", "turn_id": tid})
            await self.store.finish_turn(tid, [], [], "interrupted", {})
            raise
        except Exception as e:  # never let a turn failure kill the connection
            log.exception("turn failed")
            await self.error("turn_failed", f"{type(e).__name__}: {e}")
        actions = result.actions if result else []
        slides = result.slides if result else []
        usage = result.usage if result else {}
        reply = (result.text if result else "") or ("(stopped)" if status == "stopped" else "")
        await self.store.append_message(cid, "assistant", {"text": reply, "turn_id": tid})
        await self.store.finish_turn(tid, actions, slides, status, usage)
        summary = (reply.splitlines() or [""])[0][:200]
        await self.send({"type": "turn_end", "turn_id": tid, "summary": summary, "actions": actions, "slides": slides, "usage": usage, "status": status})


def origin_ok(ws: WebSocket) -> bool:
    origin = ws.headers.get("origin")
    if not origin:
        return True  # non-browser clients send none; the cookie still has to be valid
    return urlparse(origin).netloc == ws.headers.get("host", "")


async def agent_socket(ws: WebSocket, auth, settings: Settings, store: Store, providers: dict, limiter: RateLimiter) -> None:
    user = await auth.websocket_user(ws)
    if user is None:
        await ws.close(code=4401)
        return
    if not origin_ok(ws):
        await ws.close(code=4403)
        return
    await ws.accept()
    conn = Connection(ws, user, settings, store, providers, limiter)
    try:
        while True:
            raw = await ws.receive_text()
            if len(raw) > MAX_FRAME:
                await ws.close(code=1009)
                return
            try:
                msg = json.loads(raw)
                if not isinstance(msg, dict):
                    raise ValueError("not an object")
            except ValueError:
                await conn.error("bad_message", "messages are JSON objects")
                continue
            await conn.handle(msg)
    except WebSocketDisconnect:
        pass
    finally:
        conn.closed = True
        if conn.turn_task and not conn.turn_task.done():
            conn.turn_task.cancel()
            # Let it record the turn as interrupted; shielded so that cancelling this handler
            # (server shutdown, test clients) doesn't cut the turn's own cleanup short.
            try:
                await asyncio.wait_for(asyncio.shield(asyncio.gather(conn.turn_task, return_exceptions=True)), 5)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                pass
        for fut in conn.pending.values():
            if not fut.done():
                fut.set_exception(ConnectionError("editor disconnected"))

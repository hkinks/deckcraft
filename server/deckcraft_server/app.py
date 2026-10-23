"""FastAPI app: auth routes, conversations, the agent WebSocket and the web editor's static files."""

from __future__ import annotations

from typing import Callable

from fastapi import Depends, FastAPI, HTTPException, WebSocket
from fastapi.staticfiles import StaticFiles

from .auth.local import LocalAuth
from .config import ProviderCfg, Settings
from .providers import build_provider
from .store import Store, User
from .ws import RateLimiter, agent_socket


def create_app(settings: Settings, store: Store, provider_factory: Callable[[ProviderCfg], object] | None = None) -> FastAPI:
    app = FastAPI(title="DeckCraft agent server", docs_url=None, redoc_url=None, openapi_url=None)
    auth = LocalAuth(store, settings.session_secret, secure_cookie=settings.secure_cookie)
    factory = provider_factory or build_provider
    providers = {k: factory(v) for k, v in settings.providers.items()}
    limiter = RateLimiter()
    app.include_router(auth.router())

    @app.get("/api/conversations")
    async def conversations(deck: str, user: User = Depends(auth.require_user)):
        return await store.list_conversations(user.id, deck)

    @app.get("/api/conversations/{cid}")
    async def conversation(cid: str, user: User = Depends(auth.require_user)):
        c = await store.get_conversation(cid)
        if c is None or c["user_id"] != user.id:
            raise HTTPException(status_code=404)
        return {"conversation": c, "messages": await store.messages(cid), "turns": await store.turns(cid)}

    @app.websocket("/ws/agent")
    async def ws_agent(ws: WebSocket):
        await agent_socket(ws, auth, settings, store, providers, limiter)

    if settings.site_dir is not None and settings.site_dir.exists():
        app.mount("/", StaticFiles(directory=settings.site_dir, html=True), name="site")
    return app

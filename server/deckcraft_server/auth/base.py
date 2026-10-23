"""Authentication interface. `LocalAuth` implements it today; an Entra ID (OIDC) provider later."""

from __future__ import annotations

from typing import Protocol

from fastapi import APIRouter, Request, WebSocket

from ..store import User


class AuthProvider(Protocol):
    async def user_from_cookies(self, cookies: dict[str, str]) -> User | None: ...

    async def require_user(self, request: Request) -> User: ...

    async def websocket_user(self, ws: WebSocket) -> User | None: ...

    def router(self) -> APIRouter: ...

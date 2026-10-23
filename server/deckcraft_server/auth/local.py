"""Local username/password accounts: argon2id hashes, signed HttpOnly session cookie."""

from __future__ import annotations

import time
from collections import defaultdict, deque

from argon2 import PasswordHasher
from argon2.exceptions import InvalidHashError, VerificationError
from fastapi import APIRouter, HTTPException, Request, Response, WebSocket
from itsdangerous import BadSignature, TimestampSigner
from pydantic import BaseModel

from ..store import Store, User

COOKIE = "dc_session"
MAX_AGE = 14 * 24 * 3600
FAIL_LIMIT = 5
FAIL_WINDOW = 600.0

_hasher = PasswordHasher()


def hash_password(password: str) -> str:
    return _hasher.hash(password)


def verify_password(password_hash: str, password: str) -> bool:
    try:
        return _hasher.verify(password_hash, password)
    except (VerificationError, InvalidHashError):
        return False


class LoginBody(BaseModel):
    username: str
    password: str


class LocalAuth:
    def __init__(self, store: Store, secret: str, secure_cookie: bool = False):
        self.store = store
        self.signer = TimestampSigner(secret, salt="deckcraft-session")
        self.secure_cookie = secure_cookie
        self.failures: dict[tuple[str, str], deque[float]] = defaultdict(deque)
        # A dummy hash so unknown usernames take as long as wrong passwords.
        self._dummy = hash_password("not-a-real-password")

    async def user_from_cookies(self, cookies: dict[str, str]) -> User | None:
        token = cookies.get(COOKIE)
        if not token:
            return None
        try:
            username = self.signer.unsign(token, max_age=MAX_AGE).decode()
        except (BadSignature, UnicodeDecodeError):
            return None
        user = await self.store.get_user(username)
        if user is None or user.disabled:
            return None
        return user

    async def require_user(self, request: Request) -> User:
        user = await self.user_from_cookies(request.cookies)
        if user is None:
            raise HTTPException(status_code=401, detail="not logged in")
        return user

    async def websocket_user(self, ws: WebSocket) -> User | None:
        return await self.user_from_cookies(ws.cookies)

    def _limited(self, key: tuple[str, str]) -> bool:
        q = self.failures[key]
        now = time.monotonic()
        while q and now - q[0] > FAIL_WINDOW:
            q.popleft()
        return len(q) >= FAIL_LIMIT

    def router(self) -> APIRouter:
        r = APIRouter()

        @r.post("/api/login")
        async def login(body: LoginBody, request: Request, response: Response):
            ip = request.client.host if request.client else "?"
            key = (body.username, ip)
            if self._limited(key):
                raise HTTPException(status_code=429, detail="too many failed logins, try again later")
            user = await self.store.get_user(body.username)
            ok = verify_password(user.password_hash if user else self._dummy, body.password)
            if user is None or user.disabled or not ok:
                self.failures[key].append(time.monotonic())
                raise HTTPException(status_code=401, detail="wrong username or password")
            self.failures.pop(key, None)
            token = self.signer.sign(user.username).decode()
            response.set_cookie(
                COOKIE, token, max_age=MAX_AGE, httponly=True, samesite="strict", secure=self.secure_cookie, path="/"
            )
            return {"username": user.username, "admin": user.admin}

        @r.post("/api/logout")
        async def logout(response: Response):
            response.delete_cookie(COOKIE, path="/")
            return {"ok": True}

        @r.get("/api/me")
        async def me(request: Request):
            user = await self.require_user(request)
            return {"username": user.username, "admin": user.admin}

        return r

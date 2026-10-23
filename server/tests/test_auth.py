import pytest
from fastapi import Depends, FastAPI
from fastapi.testclient import TestClient

from deckcraft_server.auth.local import LocalAuth, hash_password
from deckcraft_server.store import Store


@pytest.fixture
def client(tmp_path):
    import asyncio

    store = Store(tmp_path / "a.db")
    asyncio.run(store.init())
    asyncio.run(store.add_user("ana", hash_password("pw-ana")))
    asyncio.run(store.add_user("old", hash_password("pw-old")))
    asyncio.run(store.disable("old"))
    auth = LocalAuth(store, secret="test-secret", secure_cookie=False)
    app = FastAPI()
    app.include_router(auth.router())

    @app.get("/private")
    async def private(user=Depends(auth.require_user)):
        return {"user": user.username}

    return TestClient(app)


def test_login_sets_cookie_and_me_works(client):
    assert client.get("/api/me").status_code == 401
    r = client.post("/api/login", json={"username": "ana", "password": "pw-ana"})
    assert r.status_code == 200
    assert client.get("/api/me").json()["username"] == "ana"
    assert client.get("/private").json() == {"user": "ana"}


def test_wrong_password_is_401(client):
    assert client.post("/api/login", json={"username": "ana", "password": "nope"}).status_code == 401
    assert client.post("/api/login", json={"username": "ghost", "password": "x"}).status_code == 401


def test_disabled_user_cannot_log_in(client):
    assert client.post("/api/login", json={"username": "old", "password": "pw-old"}).status_code == 401


def test_rate_limit_after_five_failures(client):
    for _ in range(5):
        assert client.post("/api/login", json={"username": "ana", "password": "bad"}).status_code == 401
    assert client.post("/api/login", json={"username": "ana", "password": "pw-ana"}).status_code == 429


def test_logout_clears_session(client):
    client.post("/api/login", json={"username": "ana", "password": "pw-ana"})
    client.post("/api/logout")
    assert client.get("/api/me").status_code == 401


def test_tampered_cookie_is_rejected(client):
    client.cookies.set("dc_session", "ana.forged.sig")
    assert client.get("/api/me").status_code == 401

import asyncio

import pytest
from fastapi.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

from deckcraft_server.app import create_app
from deckcraft_server.auth.local import hash_password
from deckcraft_server.config import Settings
from deckcraft_server.providers.base import AssistantMsg, Done, TextDelta, ToolUse
from deckcraft_server.store import Store


class Scripted:
    kind = "scripted"
    vision = False

    def __init__(self, responses):
        self.responses = responses

    async def stream(self, system, history, tools):
        text, uses = self.responses.pop(0) if self.responses else ("(out of script)", [])
        if text:
            yield TextDelta(text)
        yield Done("end_turn", {"input_tokens": 3, "output_tokens": 2}, AssistantMsg(text, uses, None, "scripted"))


TOOLS = [{"name": "run_command", "description": "Run a command", "inputSchema": {"type": "object"}}]


@pytest.fixture
def env(tmp_path):
    store = Store(tmp_path / "w.db")
    asyncio.run(store.init())
    asyncio.run(store.add_user("ana", hash_password("pw")))
    settings = Settings.model_validate({
        "default_provider": "fake",
        "providers": {"fake": {"kind": "openai_compat", "base_url": "http://unused/v1", "model": "fake-model"}},
        "session_secret": "x",
    })
    script = []
    app = create_app(settings, store, provider_factory=lambda cfg: Scripted(script))
    client = TestClient(app)
    return client, script, store


def login(client):
    assert client.post("/api/login", json={"username": "ana", "password": "pw"}).status_code == 200


def hello(ws):
    ws.send_json({"v": 1, "type": "hello", "tools": TOOLS, "client_version": "test"})
    ready = ws.receive_json()
    assert ready["type"] == "ready"
    return ready


def test_unauthenticated_socket_is_closed_4401(env):
    client, _, _ = env
    with pytest.raises(WebSocketDisconnect) as e:
        with client.websocket_connect("/ws/agent") as ws:
            ws.receive_json()
    assert e.value.code == 4401


def test_foreign_origin_is_refused(env):
    client, _, _ = env
    login(client)
    with pytest.raises(WebSocketDisconnect) as e:
        with client.websocket_connect("/ws/agent", headers={"origin": "https://evil.example"}) as ws:
            ws.receive_json()
    assert e.value.code == 4403


def test_hello_returns_providers_without_secrets(env):
    client, _, _ = env
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        ready = hello(ws)
    assert ready["providers"] == [{"id": "fake", "model": "fake-model", "vision": False}]
    assert ready["default_provider"] == "fake" and ready["user"] == "ana"
    assert "base_url" not in str(ready) and "api_key" not in str(ready)


def test_turn_round_trip_with_tool_call_and_history(env):
    client, script, store = env
    script += [("Adding.", [ToolUse("t1", "run_command", {"command": "slide.new", "params": {}})]), ("Done!", [])]
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "open_conversation", "deck_key": "deck#1"})
        assert ws.receive_json() == {"type": "history", "conversation_id": None, "items": []}
        ws.send_json({"v": 1, "type": "user_message", "text": "add a slide", "deck": {"deck_key": "deck#1", "slides": 1, "current": 0}})
        got = []
        while True:
            m = ws.receive_json()
            got.append(m)
            if m["type"] == "tool_call":
                assert m["name"] == "run_command" and m["input"]["command"] == "slide.new"
                ws.send_json({"v": 1, "type": "tool_result", "id": m["id"], "ok": True, "content": [{"type": "text", "text": "ok"}]})
            if m["type"] == "turn_end":
                break
        types = [m["type"] for m in got]
        assert types[0] == "conversation"
        assert "assistant_delta" in types
        end = got[-1]
        assert end["status"] == "ok" and end["actions"] == ["slide.new"]
        cid = got[0]["id"]
        ws.send_json({"v": 1, "type": "open_conversation", "deck_key": "deck#1"})
        h = ws.receive_json()
    assert h["conversation_id"] == cid
    assert [i["role"] for i in h["items"]] == ["user", "assistant"]
    assert h["items"][1]["turn"]["actions"] == ["slide.new"]
    turns = asyncio.run(store.turns(cid))
    assert turns[0]["status"] == "ok" and turns[0]["usage"]["input_tokens"] == 6


def test_second_message_while_running_is_busy(env):
    client, script, _ = env
    script += [("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("ok", [])]
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "one", "deck": {"deck_key": "d"}})
        while (m := ws.receive_json())["type"] != "tool_call":
            pass
        ws.send_json({"v": 1, "type": "user_message", "text": "two", "deck": {"deck_key": "d"}})
        err = ws.receive_json()
        assert err["type"] == "error" and err["code"] == "busy"
        ws.send_json({"v": 1, "type": "tool_result", "id": m["id"], "ok": True, "content": []})
        while ws.receive_json()["type"] != "turn_end":
            pass


def test_stop_ends_turn_as_stopped(env):
    client, script, _ = env
    script += [("", [ToolUse("t1", "run_command", {"command": "slide.new"}), ToolUse("t2", "run_command", {"command": "slide.new"})]), ("x", [])]
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "go", "deck": {"deck_key": "d"}})
        while (m := ws.receive_json())["type"] != "tool_call":
            pass
        ws.send_json({"v": 1, "type": "stop"})
        ws.send_json({"v": 1, "type": "tool_result", "id": m["id"], "ok": True, "content": []})
        while (m := ws.receive_json())["type"] != "turn_end":
            assert m["type"] != "tool_call" or m["id"] == "t1"
        assert m["status"] == "stopped"


def test_malformed_messages_get_errors_not_crashes(env):
    client, _, _ = env
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        ws.send_text("not json")
        assert ws.receive_json()["code"] == "bad_message"
        ws.send_json({"v": 1, "type": "teleport"})
        assert ws.receive_json()["code"] == "bad_message"
        ws.send_json({"v": 1, "type": "user_message", "text": "hi", "deck": {"deck_key": "d"}})
        assert ws.receive_json()["code"] == "no_hello"
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "hi", "deck": {"deck_key": "d"}, "provider": "nope"})
        assert ws.receive_json()["code"] == "bad_provider"


def test_index_served_when_site_dir_set(tmp_path, env):
    _, _, store = env
    site = tmp_path / "site"
    site.mkdir()
    (site / "index.html").write_text("<canvas id=deckcraft_canvas></canvas>")
    settings = Settings.model_validate({"default_provider": "f", "providers": {"f": {"kind": "openai_compat", "base_url": "http://x/v1", "model": "m"}},
                                        "site_dir": str(site), "session_secret": "x"})
    c = TestClient(create_app(settings, store))
    assert "deckcraft_canvas" in c.get("/").text


def test_hidden_tab_pauses_the_tool_timeout(env):
    client, script, _ = env
    client.app.state.settings.limits.tool_call_timeout_seconds = 0.3
    script += [("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("done", [])]
    login(client)
    import time
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "visibility", "hidden": True})
        ws.send_json({"v": 1, "type": "user_message", "text": "go", "deck": {"deck_key": "d"}})
        while (m := ws.receive_json())["type"] != "tool_call":
            pass
        time.sleep(1.0)  # longer than the timeout, but the tab is hidden
        ws.send_json({"v": 1, "type": "visibility", "hidden": False})
        ws.send_json({"v": 1, "type": "tool_result", "id": m["id"], "ok": True, "content": []})
        while (m := ws.receive_json())["type"] != "turn_end":
            pass
        assert m["status"] == "ok"


def test_visible_tab_still_times_out(env):
    client, script, _ = env
    client.app.state.settings.limits.tool_call_timeout_seconds = 0.3
    script += [("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("done", [])]
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "go", "deck": {"deck_key": "d"}})
        while (m := ws.receive_json())["type"] != "turn_end":
            pass
        assert m["status"] == "error"


def test_disconnect_mid_turn_is_stored_as_interrupted(env):
    client, script, store = env
    script += [("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("x", [])]
    import time

    with client:  # one event loop for the socket and the server's cleanup after it closes
        login(client)
        with client.websocket_connect("/ws/agent") as ws:
            hello(ws)
            ws.send_json({"v": 1, "type": "user_message", "text": "go", "deck": {"deck_key": "d"}})
            started = None
            while (m := ws.receive_json())["type"] != "tool_call":
                if m["type"] == "turn_start":
                    started = m["turn_id"]
            assert started
        turns = []
        for _ in range(50):
            convs = asyncio.run(store.list_conversations(1, "d"))
            turns = asyncio.run(store.turns(convs[0]["id"])) if convs else []
            if turns and turns[0]["status"] == "interrupted":
                break
            time.sleep(0.05)
    assert turns[0]["status"] == "interrupted" and turns[0]["id"] == started
    msgs = asyncio.run(store.messages(convs[0]["id"]))
    assert msgs[-1]["role"] == "assistant" and msgs[-1]["content"]["turn_id"] == started


def test_new_conversation_starts_fresh(env):
    client, script, _ = env
    script += [("first", []), ("second", [])]
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "a", "deck": {"deck_key": "d"}})
        first = ws.receive_json()["id"]
        while ws.receive_json()["type"] != "turn_end":
            pass
        ws.send_json({"v": 1, "type": "new_conversation"})
        ws.send_json({"v": 1, "type": "user_message", "text": "b", "deck": {"deck_key": "d"}})
        second = ws.receive_json()
        assert second["type"] == "conversation" and second["id"] != first
        while ws.receive_json()["type"] != "turn_end":
            pass


def test_bad_provider_type_is_an_error_not_a_dropped_connection(env):
    client, _, _ = env
    login(client)
    with client.websocket_connect("/ws/agent") as ws:
        hello(ws)
        ws.send_json({"v": 1, "type": "user_message", "text": "hi", "deck": {"deck_key": "d"}, "provider": ["x"]})
        assert ws.receive_json()["code"] == "bad_provider"

import asyncio

import pytest

from deckcraft_server.agent.loop import run_turn
from deckcraft_server.config import Limits
from deckcraft_server.providers.base import AssistantMsg, Done, TextDelta, ToolUse, UserMsg


class ScriptedProvider:
    """Yields one scripted response per stream() call and records what it was sent."""

    kind = "scripted"
    vision = False

    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    async def stream(self, system, history, tools):
        self.calls.append([h for h in history])
        text, tool_uses = self.responses.pop(0)
        if text:
            yield TextDelta(text)
        yield Done("tool_use" if tool_uses else "end_turn", {"input_tokens": 1, "output_tokens": 1},
                   AssistantMsg(text=text, tool_uses=tool_uses, raw=None, kind="scripted"))


class FakeBrowser:
    def __init__(self, answers=None, delay=0.0):
        self.answers = answers or {}
        self.delay = delay
        self.calls = []

    async def call_tool(self, id, name, input):
        self.calls.append((name, input))
        if self.delay:
            await asyncio.sleep(self.delay)
        ok, text = self.answers.get(name, (True, f"{name} done"))
        return ok, [{"type": "text", "text": text}]


TOOLS = [{"name": "run_command", "description": "", "inputSchema": {"type": "object"}},
         {"name": "inspect_slide", "description": "", "inputSchema": {"type": "object"}}]


async def collect(provider, browser, limits=None, stop=None, history=None):
    deltas = []

    async def on_delta(t):
        deltas.append(t)

    history = history if history is not None else [UserMsg("hi")]
    r = await run_turn(provider, "sys", history, TOOLS, browser, limits or Limits(), on_delta, stop or asyncio.Event())
    return r, deltas, history


async def test_text_only_turn_ends_ok():
    r, deltas, _ = await collect(ScriptedProvider([("Hello!", [])]), FakeBrowser())
    assert r.status == "ok" and deltas == ["Hello!"] and r.actions == []
    assert r.text == "Hello!"


async def test_tool_calls_round_trip_and_feed_next_call():
    p = ScriptedProvider([
        ("Adding a slide.", [ToolUse("t1", "run_command", {"command": "slide.new", "params": {"layout": "title"}}),
                             ToolUse("t2", "inspect_slide", {"slide": 1})]),
        ("Done.", []),
    ])
    b = FakeBrowser()
    r, _, history = await collect(p, b)
    assert r.status == "ok"
    assert [c[0] for c in b.calls] == ["run_command", "inspect_slide"]
    assert r.actions == ["slide.new"]  # inspect_slide only looked
    assert r.slides == []
    second_call_history = p.calls[1]
    results = second_call_history[-1]
    assert [x.tool_use_id for x in results.results] == ["t1", "t2"] and all(x.ok for x in results.results)
    assert r.usage["input_tokens"] == 2


async def test_unknown_tool_gets_an_error_result_and_the_loop_continues():
    p = ScriptedProvider([("", [ToolUse("t1", "format_disk", {})]), ("Sorry.", [])])
    b = FakeBrowser()
    r, _, history = await collect(p, b)
    assert r.status == "ok" and b.calls == []
    assert history[2].results[0].ok is False


async def test_tool_timeout_ends_the_turn_with_error():
    p = ScriptedProvider([("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("never", [])])
    r, _, history = await collect(p, FakeBrowser(delay=0.5), limits=Limits(tool_call_timeout_seconds=0.05))
    assert r.status == "error" and "timed out" in r.error
    assert history[-1].results[0].ok is False


async def test_stop_event_stops_before_running_tools():
    stop = asyncio.Event()
    stop.set()
    p = ScriptedProvider([("", [ToolUse("t1", "run_command", {"command": "slide.new"})])])
    b = FakeBrowser()
    r, _, history = await collect(p, b, stop=stop)
    assert r.status == "stopped" and b.calls == []


async def test_tool_call_limit():
    many = [ToolUse(f"t{i}", "run_command", {"command": "slide.new"}) for i in range(5)]
    p = ScriptedProvider([("", many), ("x", [])])
    b = FakeBrowser()
    r, _, history = await collect(p, b, limits=Limits(max_tool_calls_per_turn=3))
    assert r.status == "limit" and len(b.calls) == 3
    assert len(history[-1].results) == 5


async def test_provider_failure_is_an_error_status():
    class Broken:
        kind = "x"
        vision = False

        async def stream(self, system, history, tools):
            raise RuntimeError("upstream 500")
            yield  # pragma: no cover

    r, _, _ = await collect(Broken(), FakeBrowser())
    assert r.status == "error" and "upstream 500" in r.error


async def test_batch_actions_are_listed_per_command():
    p = ScriptedProvider([("", [ToolUse("t1", "batch", {"commands": [{"command": "slide.new"}, {"command": "format.bold"}]})]), ("", [])])
    tools = TOOLS + [{"name": "batch", "description": "", "inputSchema": {}}]
    deltas = []

    async def on_delta(t):
        deltas.append(t)

    r = await run_turn(p, "s", [UserMsg("x")], tools, FakeBrowser(), Limits(), on_delta, asyncio.Event())
    assert r.actions == ["slide.new", "format.bold"]


async def test_read_only_tools_are_not_counted_as_changes():
    p = ScriptedProvider([("", [ToolUse("t1", "inspect_slide", {"slide": 0}), ToolUse("t2", "run_command", {"command": "format.bold"})]), ("", [])])
    r, _, _ = await collect(p, FakeBrowser())
    assert r.actions == ["format.bold"]


async def test_browser_that_manages_timeouts_is_not_cut_off_by_the_loop():
    class Patient(FakeBrowser):
        manages_timeout = True

    p = ScriptedProvider([("", [ToolUse("t1", "run_command", {"command": "slide.new"})]), ("ok", [])])
    r, _, _ = await collect(p, Patient(delay=0.2), limits=Limits(tool_call_timeout_seconds=0.05))
    assert r.status == "ok"

"""The provider-neutral agent loop for one chat turn.

Each tool call is forwarded to the browser (`BrowserLink.call_tool`), which runs it on the live
deck. Every tool-use the model emitted always gets a tool result back in history (the APIs reject
a history with unanswered tool uses), even when the turn stops early.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Awaitable, Callable, Protocol

from ..config import Limits
from ..providers.base import Done, HistoryItem, LLMProvider, TextDelta, ToolResult, ToolResults


class BrowserLink(Protocol):
    async def call_tool(self, id: str, name: str, input: dict) -> tuple[bool, list[dict]]: ...


@dataclass
class TurnResult:
    status: str  # ok | stopped | limit | error
    text: str = ""
    actions: list[str] = field(default_factory=list)
    slides: list[int] = field(default_factory=list)
    usage: dict = field(default_factory=lambda: {"input_tokens": 0, "output_tokens": 0})
    error: str = ""


def describe(name: str, input: dict) -> list[str]:
    if name == "run_command":
        return [str(input.get("command", "run_command"))]
    if name == "batch":
        cmds = input.get("commands")
        if isinstance(cmds, list):
            return [str(c.get("command", "?")) for c in cmds if isinstance(c, dict)] or ["batch"]
    return [name]


def slide_of(input: dict) -> int | None:
    for k in ("slide", "index"):
        v = input.get(k)
        if isinstance(v, int) and not isinstance(v, bool) and 0 <= v < 100_000:
            return v
    return None


def _err(id: str, text: str) -> ToolResult:
    return ToolResult(id, False, [{"type": "text", "text": text}])


async def run_turn(
    provider: LLMProvider,
    system: str,
    history: list[HistoryItem],
    tools: list[dict],
    browser: BrowserLink,
    limits: Limits,
    on_delta: Callable[[str], Awaitable[None]],
    stop: asyncio.Event,
) -> TurnResult:
    names = {t["name"] for t in tools}
    result = TurnResult(status="ok")
    slides: set[int] = set()
    calls = 0
    deadline = time.monotonic() + limits.max_turn_seconds
    texts: list[str] = []
    while True:
        if stop.is_set():
            result.status = "stopped"
            break
        done: Done | None = None
        try:
            async for ev in provider.stream(system, history, tools):
                if isinstance(ev, TextDelta):
                    await on_delta(ev.text)
                elif isinstance(ev, Done):
                    done = ev
        except asyncio.CancelledError:
            raise
        except Exception as e:  # provider/network failure after the SDK's own retries
            result.status, result.error = "error", f"{type(e).__name__}: {e}"
            break
        if done is None:
            result.status, result.error = "error", "the model returned no response"
            break
        for k in ("input_tokens", "output_tokens"):
            result.usage[k] = result.usage.get(k, 0) + int(done.usage.get(k, 0) or 0)
        msg = done.assistant
        history.append(msg)
        if msg.text:
            texts.append(msg.text)
        if not msg.tool_uses:
            break
        results = ToolResults()
        halt: str | None = None
        for tu in msg.tool_uses:
            if halt:
                results.results.append(_err(tu.id, f"not run: turn ended ({halt})"))
                continue
            if stop.is_set():
                halt = "stopped"
                results.results.append(_err(tu.id, "not run: stopped by the user"))
                continue
            if time.monotonic() > deadline:
                halt = "limit"
                result.error = f"turn time limit ({limits.max_turn_seconds:g} s) reached"
                results.results.append(_err(tu.id, "not run: " + result.error))
                continue
            if calls >= limits.max_tool_calls_per_turn:
                halt = "limit"
                result.error = f"tool call limit ({limits.max_tool_calls_per_turn}) reached"
                results.results.append(_err(tu.id, "not run: " + result.error))
                continue
            if tu.name not in names:
                results.results.append(_err(tu.id, f"unknown tool {tu.name!r}; available: {', '.join(sorted(names))}"))
                continue
            if "__invalid_json__" in tu.input:
                results.results.append(_err(tu.id, "tool input was not valid JSON; send a JSON object matching the tool's schema"))
                continue
            calls += 1
            try:
                ok, parts = await asyncio.wait_for(browser.call_tool(tu.id, tu.name, tu.input), limits.tool_call_timeout_seconds)
            except asyncio.TimeoutError:
                halt = "error"
                result.error = f"the editor did not answer {tu.name} (timed out after {limits.tool_call_timeout_seconds:g} s)"
                results.results.append(_err(tu.id, result.error))
                continue
            except ConnectionError as e:
                halt = "error"
                result.error = f"editor disconnected: {e}"
                results.results.append(_err(tu.id, result.error))
                continue
            results.results.append(ToolResult(tu.id, ok, parts))
            if ok:
                result.actions += describe(tu.name, tu.input)
                if (s := slide_of(tu.input)) is not None:
                    slides.add(s)
        history.append(results)
        if halt:
            result.status = halt
            break
    result.text = "\n\n".join(texts)
    result.slides = sorted(slides)
    return result

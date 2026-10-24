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
from ..providers.base import AssistantMsg, Done, HistoryItem, LLMProvider, TextDelta, ToolResult, ToolResults, UserMsg


class BrowserLink(Protocol):
    async def call_tool(self, id: str, name: str, input: dict) -> tuple[bool, list[dict]]: ...


class ServerTools(Protocol):
    """Tools the server runs itself (web search and fetch), next to the browser's."""

    def definitions(self) -> list[dict]: ...

    async def call(self, name: str, input: dict) -> tuple[bool, list[dict]]: ...


@dataclass
class TurnResult:
    status: str  # ok | stopped | limit | error
    text: str = ""
    actions: list[str] = field(default_factory=list)
    slides: list[int] = field(default_factory=list)
    usage: dict = field(default_factory=lambda: {"input_tokens": 0, "output_tokens": 0})
    error: str = ""


# Tools that only look at the deck (or move the view); not counted as changes.
READ_ONLY = {"list_commands", "inspect_document", "inspect_slide", "render_slide", "get_deck_json", "go_to_slide", "select", "web_search", "web_fetch"}


# Shortcut tools, logged as the editor command they run (so the pane shows its menu label).
TOOL_COMMANDS = {
    "add_slide": "slide.new",
    "add_shape": "shape.insert",
    "add_text_box": "insert.textBox",
    "set_text": "text.set",
    "insert_table": "insert.table",
    "insert_chart": "insert.chart",
    "patch_deck_json": "doc.patch",
}


def describe(name: str, input: dict) -> list[str]:
    if name in READ_ONLY:
        return []
    if name in TOOL_COMMANDS:
        return [TOOL_COMMANDS[name]]
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


def flatten_history(history: list[HistoryItem], max_chars: int = 60_000) -> list[HistoryItem]:
    """Earlier turns as plain text for the next turn: each user message and the agent's final
    reply, without tool calls, tool results or thinking. Old thinking blocks are never replayed
    (so no history is edited under them) and the context stays small. The oldest turns are
    dropped first once the text passes `max_chars`."""
    turns: list[list[HistoryItem]] = []
    for h in history:
        if isinstance(h, UserMsg):
            turns.append([UserMsg(h.text)])
        elif isinstance(h, AssistantMsg) and turns and h.text:
            last = turns[-1]
            if len(last) == 2:
                last[1] = AssistantMsg(last[1].text + "\n\n" + h.text, [], None, "history")
            else:
                last.append(AssistantMsg(h.text, [], None, "history"))
    for t in turns:
        if len(t) == 1:
            t.append(AssistantMsg("(no reply)", [], None, "history"))
    kept: list[list[HistoryItem]] = []
    total = 0
    for t in reversed(turns):
        size = sum(len(x.text) for x in t)
        if total + size > max_chars:
            break
        kept.append(t)
        total += size
    return [x for t in reversed(kept) for x in t]


def _err(id: str, text: str) -> ToolResult:
    return ToolResult(id, False, [{"type": "text", "text": text}])


class _Executor:
    """Runs one tool call with the turn's checks (stop, time and call limits, unknown tools) and
    records what it changed. After a check ends the turn (`halt`), later calls only get errors."""

    def __init__(self, browser, limits: Limits, stop: asyncio.Event, deadline: float, names: set[str], server_tools, on_server: set[str], result: TurnResult):
        self.browser, self.limits, self.stop, self.deadline = browser, limits, stop, deadline
        self.names, self.server_tools, self.on_server, self.result = names, server_tools, on_server, result
        self.calls = 0
        self.slides: set[int] = set()
        self.halt: str | None = None

    async def __call__(self, id: str, name: str, input: dict) -> ToolResult:
        limits, result = self.limits, self.result
        if self.halt:
            return _err(id, f"not run: turn ended ({self.halt})")
        if self.stop.is_set():
            self.halt = "stopped"
            return _err(id, "not run: stopped by the user")
        if time.monotonic() > self.deadline:
            self.halt = "limit"
            result.error = f"turn time limit ({limits.max_turn_seconds:g} s) reached"
            return _err(id, "not run: " + result.error)
        if self.calls >= limits.max_tool_calls_per_turn:
            self.halt = "limit"
            result.error = f"tool call limit ({limits.max_tool_calls_per_turn}) reached"
            return _err(id, "not run: " + result.error)
        if name not in self.names:
            return _err(id, f"unknown tool {name!r}; available: {', '.join(sorted(self.names))}")
        if "__invalid_json__" in input:
            return _err(id, "tool input was not valid JSON; send a JSON object matching the tool's schema")
        self.calls += 1
        if self.server_tools and name in self.on_server:
            try:
                ok, parts = await asyncio.wait_for(self.server_tools.call(name, input), limits.tool_call_timeout_seconds)
            except asyncio.TimeoutError:
                ok, parts = False, [{"type": "text", "text": f"{name} timed out"}]
            # The pane lists these steps too, though the browser doesn't run them.
            if report := getattr(self.browser, "server_activity", None):
                await report(name, input, ok, parts)
            return ToolResult(id, ok, parts)
        # A browser link that knows when its tab is hidden applies the tool timeout itself.
        timeout = None if getattr(self.browser, "manages_timeout", False) else limits.tool_call_timeout_seconds
        try:
            ok, parts = await asyncio.wait_for(self.browser.call_tool(id, name, input), timeout)
        except asyncio.TimeoutError:
            self.halt = "error"
            result.error = f"the editor did not answer {name} (timed out after {limits.tool_call_timeout_seconds:g} s)"
            return _err(id, result.error)
        except ConnectionError as e:
            self.halt = "error"
            result.error = f"editor disconnected: {e}"
            return _err(id, result.error)
        if ok and (changes := describe(name, input)):
            result.actions += changes
            if (s := slide_of(input)) is not None:
                self.slides.add(s)
        return ToolResult(id, ok, parts)


async def run_turn(
    provider: LLMProvider,
    system: str,
    history: list[HistoryItem],
    tools: list[dict],
    browser: BrowserLink,
    limits: Limits,
    on_delta: Callable[[str], Awaitable[None]],
    stop: asyncio.Event,
    server_tools: ServerTools | None = None,
) -> TurnResult:
    on_server = {d["name"] for d in server_tools.definitions()} if server_tools else set()
    tools = [t for t in tools if t["name"] not in on_server] + (server_tools.definitions() if server_tools else [])
    names = {t["name"] for t in tools}
    result = TurnResult(status="ok")
    deadline = time.monotonic() + limits.max_turn_seconds
    execute = _Executor(browser, limits, stop, deadline, names, server_tools, on_server, result)
    if drive := getattr(provider, "drive", None):
        await _run_driven(drive, system, history, tools, execute, on_delta, stop, deadline, result)
    else:
        await _run_steps(provider, system, history, tools, execute, on_delta, stop, deadline, result)
    result.slides = sorted(execute.slides)
    return result


async def _race(work: Awaitable, stop: asyncio.Event, deadline: float) -> asyncio.Task | None:
    """Runs `work` against Stop and the turn's time limit (a hung provider can't hold the editor
    locked). Returns the finished task, or None when Stop or the limit came first."""
    call = asyncio.ensure_future(work)
    stopped = asyncio.create_task(stop.wait())
    try:
        finished, _ = await asyncio.wait({call, stopped}, timeout=max(0.0, deadline - time.monotonic()), return_when=asyncio.FIRST_COMPLETED)
    finally:
        stopped.cancel()
    if call not in finished:
        call.cancel()
        return None
    return call


def _cut_off(result: TurnResult, stop: asyncio.Event, limits_seconds: float) -> None:
    if stop.is_set():
        result.status = "stopped"
    else:
        result.status, result.error = "limit", f"turn time limit ({limits_seconds:g} s) reached"


async def _run_driven(drive, system, history, tools, execute: _Executor, on_delta, stop, deadline, result: TurnResult) -> None:
    """A provider that runs its own tool loop (Claude Code) and calls each tool through `execute`.
    It watches Stop itself so it can interrupt cleanly; the turn's time limit still applies here."""
    call = await _race(drive(system, history, tools, execute, on_delta, stop), asyncio.Event(), deadline)
    if call is None:
        _cut_off(result, stop, execute.limits.max_turn_seconds)
        return
    try:
        text, usage = call.result()
    except asyncio.CancelledError:
        raise
    except Exception as e:
        result.status, result.error = "error", f"{type(e).__name__}: {e}"
        return
    result.text = text
    result.usage = {k: int(usage.get(k, 0) or 0) for k in ("input_tokens", "output_tokens")}
    history.append(AssistantMsg(text, [], None, "history"))
    if execute.halt:
        result.status = execute.halt
    elif stop.is_set():
        result.status = "stopped"


async def _run_steps(provider, system, history, tools, execute: _Executor, on_delta, stop, deadline, result: TurnResult) -> None:
    """The loop for providers that answer one model step at a time (`stream`)."""
    texts: list[str] = []
    while True:
        if stop.is_set():
            result.status = "stopped"
            break

        async def consume() -> Done | None:
            last = None
            async for ev in provider.stream(system, history, tools):
                if isinstance(ev, TextDelta):
                    await on_delta(ev.text)
                elif isinstance(ev, Done):
                    last = ev
            return last

        call = await _race(consume(), stop, deadline)
        if call is None:
            _cut_off(result, stop, execute.limits.max_turn_seconds)
            break
        try:
            done = call.result()
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
        results = ToolResults([await execute(tu.id, tu.name, tu.input) for tu in msg.tool_uses])
        history.append(results)
        if execute.halt:
            result.status = execute.halt
            break
    result.text = "\n\n".join(texts)

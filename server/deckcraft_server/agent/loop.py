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


@dataclass
class TurnResult:
    status: str  # ok | stopped | limit | error
    text: str = ""
    actions: list[str] = field(default_factory=list)
    slides: list[int] = field(default_factory=list)
    usage: dict = field(default_factory=lambda: {"input_tokens": 0, "output_tokens": 0})
    error: str = ""


# Tools that only look at the deck (or move the view); not counted as changes.
READ_ONLY = {"list_commands", "inspect_document", "inspect_slide", "render_slide", "get_deck_json", "go_to_slide", "select"}


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

        async def consume() -> Done | None:
            last = None
            async for ev in provider.stream(system, history, tools):
                if isinstance(ev, TextDelta):
                    await on_delta(ev.text)
                elif isinstance(ev, Done):
                    last = ev
            return last

        # The model call races Stop and the turn's time limit: a hung provider can't hold the
        # editor locked.
        call = asyncio.create_task(consume())
        stopped = asyncio.create_task(stop.wait())
        try:
            finished, _ = await asyncio.wait({call, stopped}, timeout=max(0.0, deadline - time.monotonic()), return_when=asyncio.FIRST_COMPLETED)
        finally:
            stopped.cancel()
        if call not in finished:
            call.cancel()
            if stop.is_set():
                result.status = "stopped"
            else:
                result.status, result.error = "limit", f"turn time limit ({limits.max_turn_seconds:g} s) reached"
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
            # A browser link that knows when its tab is hidden applies the tool timeout itself.
            timeout = None if getattr(browser, "manages_timeout", False) else limits.tool_call_timeout_seconds
            try:
                ok, parts = await asyncio.wait_for(browser.call_tool(tu.id, tu.name, tu.input), timeout)
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
            if ok and (changes := describe(tu.name, tu.input)):
                result.actions += changes
                if (s := slide_of(tu.input)) is not None:
                    slides.add(s)
        history.append(results)
        if halt:
            result.status = halt
            break
    result.text = "\n\n".join(texts)
    result.slides = sorted(slides)
    return result

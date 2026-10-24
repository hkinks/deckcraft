"""Claude through the Claude Code CLI on this machine, with the Claude Agent SDK.

The CLI uses the person's own Claude sign-in (`claude login`), so no API key is needed. Claude
Code runs the tool loop itself: the deck's tools are offered to it as an in-process MCP server
whose handlers call back into the agent loop's `execute`, which forwards them to the editor
with the same checks as every other provider. Claude Code's own tools (files, shell) and the
machine's Claude Code settings are switched off.
"""

from __future__ import annotations

import asyncio
import shutil
import tempfile
from typing import Awaitable, Callable

from claude_agent_sdk import (
    AssistantMessage,
    ClaudeAgentOptions,
    ClaudeSDKClient,
    ResultMessage,
    SdkMcpTool,
    StreamEvent,
    TextBlock,
    create_sdk_mcp_server,
)

from ..config import ProviderCfg
from .base import AssistantMsg, HistoryItem, ToolResult, UserMsg

SERVER = "deck"


def _content(r: ToolResult) -> dict:
    out: list[dict] = []
    for p in r.parts:
        if p.get("type") == "text":
            out.append({"type": "text", "text": p.get("text", "") or "(empty)"})
        elif p.get("type") == "image":
            out.append({"type": "image", "data": p["data"], "mimeType": p["media_type"]})
    res: dict = {"content": out or [{"type": "text", "text": "(no output)"}]}
    if not r.ok:
        res["is_error"] = True
    return res


def _prompt(history: list[HistoryItem]) -> str:
    """Earlier turns (already flattened to text by the loop) and the new request, as one prompt."""
    *earlier, last = history
    lines = []
    for h in earlier:
        if isinstance(h, UserMsg):
            lines.append(f"Person: {h.text}")
        elif isinstance(h, AssistantMsg) and h.text:
            lines.append(f"You: {h.text}")
    text = last.text if isinstance(last, UserMsg) else ""
    if not lines:
        return text
    return "Earlier in this conversation:\n\n" + "\n\n".join(lines) + "\n\n---\n\n" + text


def _default_client(options: ClaudeAgentOptions, tools: list[SdkMcpTool]) -> ClaudeSDKClient:
    return ClaudeSDKClient(options=options)


class ClaudeCodeProvider:
    kind = "claude_code"

    def __init__(self, cfg: ProviderCfg, client_factory: Callable | None = None):
        self.cfg = cfg
        self.vision = True
        self.client_factory = client_factory or _default_client

    def _options(self, system: str, tools: list[SdkMcpTool]) -> ClaudeAgentOptions:
        kw: dict = {}
        if self.cfg.effort:
            kw["effort"] = self.cfg.effort
        return ClaudeAgentOptions(
            # NixOS and similar can't run the SDK's bundled binary; the installed `claude` works everywhere.
            cli_path=shutil.which("claude"),
            model=self.cfg.model,
            system_prompt=system,
            mcp_servers={SERVER: create_sdk_mcp_server(SERVER, tools=tools)},
            strict_mcp_config=True,
            tools=[],
            allowed_tools=[f"mcp__{SERVER}__{t.name}" for t in tools],
            setting_sources=[],
            include_partial_messages=True,
            # The loop's own tool-call limit ends the turn; this only bounds a runaway session.
            max_turns=200,
            cwd=tempfile.gettempdir(),
            extra_args={"no-session-persistence": None},
            **kw,
        )

    async def drive(self, system: str, history: list[HistoryItem], tools: list[dict], execute: Callable[[str, str, dict], Awaitable[ToolResult]], on_delta, stop: asyncio.Event):
        counter = 0

        def make(t: dict) -> SdkMcpTool:
            async def handler(args: dict) -> dict:
                nonlocal counter
                counter += 1
                return _content(await execute(f"cc{counter}", t["name"], args if isinstance(args, dict) else {}))

            return SdkMcpTool(t["name"], t.get("description", ""), t.get("inputSchema") or {"type": "object"}, handler)

        sdk_tools = [make(t) for t in tools]
        texts: list[str] = []
        usage = {"input_tokens": 0, "output_tokens": 0}
        streamed = False
        async with self.client_factory(self._options(system, sdk_tools), sdk_tools) as client:
            watcher = asyncio.create_task(self._interrupt_on(stop, client))
            try:
                await client.query(_prompt(history))
                async for m in client.receive_response():
                    if isinstance(m, StreamEvent):
                        ev = m.event
                        if ev.get("type") == "content_block_delta" and ev.get("delta", {}).get("type") == "text_delta":
                            streamed = True
                            await on_delta(ev["delta"]["text"])
                    elif isinstance(m, AssistantMessage):
                        text = "".join(b.text for b in m.content if isinstance(b, TextBlock))
                        if text:
                            texts.append(text)
                            if not streamed:
                                await on_delta(text)
                        streamed = False
                    elif isinstance(m, ResultMessage):
                        u = m.usage or {}
                        usage = {
                            "input_tokens": sum(int(u.get(k, 0) or 0) for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")),
                            "output_tokens": int(u.get("output_tokens", 0) or 0),
                        }
            finally:
                watcher.cancel()
        return "\n\n".join(texts), usage

    @staticmethod
    async def _interrupt_on(stop: asyncio.Event, client) -> None:
        await stop.wait()
        await client.interrupt()

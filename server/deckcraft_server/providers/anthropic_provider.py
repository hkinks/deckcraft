"""Claude through LiteLLM's Anthropic-compatible `/v1/messages`, with the official `anthropic` SDK."""

from __future__ import annotations

import json

import anthropic

from ..config import ProviderCfg
from .base import AssistantMsg, Done, HistoryItem, TextDelta, ToolResults, ToolUse, UserMsg


def _tool(t: dict) -> dict:
    return {"name": t["name"], "description": t.get("description", ""), "input_schema": t.get("inputSchema") or {"type": "object"}}


def _result_content(parts: list[dict], vision: bool) -> list[dict]:
    out: list[dict] = []
    for p in parts:
        if p.get("type") == "text":
            out.append({"type": "text", "text": p.get("text", "") or "(empty)"})
        elif p.get("type") == "image" and vision:
            out.append({"type": "image", "source": {"type": "base64", "media_type": p["media_type"], "data": p["data"]}})
    return out or [{"type": "text", "text": "(no output)"}]


class AnthropicProvider:
    kind = "anthropic"

    def __init__(self, cfg: ProviderCfg, http_client=None):
        self.cfg = cfg
        self.vision = cfg.vision
        self.client = anthropic.AsyncAnthropic(
            base_url=cfg.base_url, api_key=cfg.api_key or "none", http_client=http_client, max_retries=2
        )

    def _messages(self, history: list[HistoryItem]) -> list[dict]:
        msgs: list[dict] = []
        for h in history:
            if isinstance(h, UserMsg):
                msgs.append({"role": "user", "content": [{"type": "text", "text": h.text}]})
            elif isinstance(h, AssistantMsg):
                if h.kind == self.kind and h.raw:
                    content = h.raw
                else:
                    content = ([{"type": "text", "text": h.text}] if h.text else []) + [
                        {"type": "tool_use", "id": t.id, "name": t.name, "input": t.input} for t in h.tool_uses
                    ]
                msgs.append({"role": "assistant", "content": content or [{"type": "text", "text": "(no text)"}]})
            elif isinstance(h, ToolResults):
                msgs.append({
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": r.tool_use_id, "is_error": not r.ok, "content": _result_content(r.parts, self.vision)}
                        for r in h.results
                    ],
                })
        return msgs

    async def stream(self, system: str, history: list[HistoryItem], tools: list[dict]):
        kwargs: dict = {
            "model": self.cfg.model,
            "max_tokens": self.cfg.max_tokens,
            "system": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
            "tools": [_tool(t) for t in tools],
            "messages": self._messages(history),
        }
        if self.cfg.thinking:
            kwargs["thinking"] = {"type": "adaptive"}
        if self.cfg.effort:
            kwargs["output_config"] = {"effort": self.cfg.effort}
        async with self.client.messages.stream(**kwargs) as stream:
            async for event in stream:
                if event.type == "text":
                    yield TextDelta(event.text)
            final = await stream.get_final_message()
        raw = [b.model_dump(exclude_none=True) for b in final.content]
        text = "".join(b.text for b in final.content if b.type == "text")
        uses = [ToolUse(b.id, b.name, b.input if isinstance(b.input, dict) else {"__invalid_json__": json.dumps(b.input)})
                for b in final.content if b.type == "tool_use"]
        usage = {"input_tokens": final.usage.input_tokens, "output_tokens": final.usage.output_tokens}
        yield Done(final.stop_reason or "end_turn", usage, AssistantMsg(text=text, tool_uses=uses, raw=raw, kind=self.kind))

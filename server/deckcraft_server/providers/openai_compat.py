"""OpenAI-compatible chat completions (llama-swap / llama.cpp serving Qwen), with the `openai` SDK."""

from __future__ import annotations

import json

import httpx
import openai

from ..config import ProviderCfg
from .base import AssistantMsg, Done, HistoryItem, TextDelta, ToolResults, ToolUse, UserMsg, text_of


class OpenAICompatProvider:
    kind = "openai_compat"

    def __init__(self, cfg: ProviderCfg, http_client: httpx.AsyncClient | None = None):
        self.cfg = cfg
        self.vision = cfg.vision
        self.client = openai.AsyncOpenAI(base_url=cfg.base_url, api_key=cfg.api_key or "none", http_client=http_client, max_retries=2)

    def _messages(self, system: str, history: list[HistoryItem]) -> list[dict]:
        msgs: list[dict] = [{"role": "system", "content": system}]
        for h in history:
            if isinstance(h, UserMsg):
                msgs.append({"role": "user", "content": h.text})
            elif isinstance(h, AssistantMsg):
                m: dict = {"role": "assistant", "content": h.text or None}
                if h.tool_uses:
                    m["tool_calls"] = [
                        {"id": t.id, "type": "function", "function": {"name": t.name, "arguments": json.dumps(t.input)}} for t in h.tool_uses
                    ]
                msgs.append(m)
            elif isinstance(h, ToolResults):
                images = []
                for r in h.results:
                    body = text_of(r.parts) or "(no output)"
                    msgs.append({"role": "tool", "tool_call_id": r.tool_use_id, "content": body if r.ok else f"ERROR: {body}"})
                    images += [p for p in r.parts if p.get("type") == "image"]
                if images and self.vision:
                    msgs.append({"role": "user", "content": [
                        {"type": "text", "text": "Rendered slide image(s) from the tool results above:"},
                        *[{"type": "image_url", "image_url": {"url": f"data:{p['media_type']};base64,{p['data']}"}} for p in images],
                    ]})
        return msgs

    async def stream(self, system: str, history: list[HistoryItem], tools: list[dict]):
        req_tools = [
            {"type": "function", "function": {"name": t["name"], "description": t.get("description", ""), "parameters": t.get("inputSchema") or {"type": "object"}}}
            for t in tools
        ]
        stream = await self.client.chat.completions.create(
            model=self.cfg.model,
            messages=self._messages(system, history),
            tools=req_tools,
            stream=True,
            stream_options={"include_usage": True},
            max_tokens=self.cfg.max_tokens,
        )
        text = ""
        calls: dict[int, dict] = {}
        finish = "stop"
        usage = {"input_tokens": 0, "output_tokens": 0}
        async for chunk in stream:
            if chunk.usage:
                usage = {"input_tokens": chunk.usage.prompt_tokens or 0, "output_tokens": chunk.usage.completion_tokens or 0}
            for choice in chunk.choices:
                d = choice.delta
                if d and d.content:
                    text += d.content
                    yield TextDelta(d.content)
                for tc in (d.tool_calls or []) if d else []:
                    c = calls.setdefault(tc.index, {"id": "", "name": "", "args": ""})
                    if tc.id:
                        c["id"] = tc.id
                    if tc.function and tc.function.name:
                        c["name"] += tc.function.name
                    if tc.function and tc.function.arguments:
                        c["args"] += tc.function.arguments
                if choice.finish_reason:
                    finish = choice.finish_reason
        uses = []
        for i in sorted(calls):
            c = calls[i]
            try:
                args = json.loads(c["args"]) if c["args"].strip() else {}
                if not isinstance(args, dict):
                    args = {"__invalid_json__": c["args"]}
            except json.JSONDecodeError:
                args = {"__invalid_json__": c["args"]}
            uses.append(ToolUse(c["id"] or f"call_{i}", c["name"], args))
        yield Done(finish, usage, AssistantMsg(text=text, tool_uses=uses, raw=None, kind=self.kind))

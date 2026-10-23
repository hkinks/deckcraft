import json

import httpx

from deckcraft_server.config import ProviderCfg
from deckcraft_server.providers import build_provider
from deckcraft_server.providers.base import AssistantMsg, Done, TextDelta, ToolResult, ToolResults, ToolUse, UserMsg

TOOLS = [{"name": "run_command", "description": "Run a command", "inputSchema": {"type": "object", "properties": {"command": {"type": "string"}}}}]


def sse(events):
    out = ""
    for name, data in events:
        if name:
            out += f"event: {name}\n"
        out += f"data: {json.dumps(data) if not isinstance(data, str) else data}\n\n"
    return out.encode()


async def run(provider, history):
    events = []
    async for e in provider.stream("system prompt", history, TOOLS):
        events.append(e)
    return events


async def test_openai_compat_streams_text_and_tool_calls():
    seen = {}

    def handler(request: httpx.Request):
        seen["body"] = json.loads(request.content)
        chunks = [
            {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Sure, "}}]},
            {"choices": [{"index": 0, "delta": {"content": "adding."}}]},
            {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "run_command", "arguments": "{\"comm"}}]}}]},
            {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "and\": \"slide.new\"}"}}]}}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 7, "completion_tokens": 3}},
        ]
        body = sse([(None, c) for c in chunks] + [(None, "[DONE]")])
        return httpx.Response(200, content=body, headers={"content-type": "text/event-stream"})

    cfg = ProviderCfg(kind="openai_compat", base_url="http://llm.test/v1", model="qwen", vision=False)
    p = build_provider(cfg, http_client=httpx.AsyncClient(transport=httpx.MockTransport(handler)))
    history = [UserMsg("add a slide"),
               AssistantMsg(text="", tool_uses=[ToolUse("old", "run_command", {"command": "x"})], raw=None, kind="openai_compat"),
               ToolResults([ToolResult("old", True, [{"type": "text", "text": "ok"}, {"type": "image", "media_type": "image/png", "data": "AAA"}])])]
    events = await run(p, history)
    assert [e.text for e in events if isinstance(e, TextDelta)] == ["Sure, ", "adding."]
    done = events[-1]
    assert isinstance(done, Done)
    assert done.assistant.tool_uses == [ToolUse("call_1", "run_command", {"command": "slide.new"})]
    assert done.usage == {"input_tokens": 7, "output_tokens": 3}
    body = seen["body"]
    assert body["model"] == "qwen" and body["stream"] is True
    assert body["messages"][0] == {"role": "system", "content": "system prompt"}
    assert body["tools"][0]["function"]["name"] == "run_command"
    assert body["messages"][3] == {"role": "tool", "tool_call_id": "old", "content": "ok"}
    assert "image" not in json.dumps(body["messages"])  # vision off: images dropped


async def test_openai_compat_bad_tool_json_becomes_error_input():
    def handler(request):
        chunks = [{"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "c", "function": {"name": "run_command", "arguments": "{not json"}}]}, "finish_reason": "tool_calls"}]}]
        return httpx.Response(200, content=sse([(None, c) for c in chunks] + [(None, "[DONE]")]), headers={"content-type": "text/event-stream"})

    cfg = ProviderCfg(kind="openai_compat", base_url="http://llm.test/v1", model="q")
    p = build_provider(cfg, http_client=httpx.AsyncClient(transport=httpx.MockTransport(handler)))
    done = (await run(p, [UserMsg("x")]))[-1]
    assert done.assistant.tool_uses[0].input == {"__invalid_json__": "{not json"}


async def test_anthropic_streams_and_keeps_raw_blocks():
    seen = {}

    import httpx2

    def handler(request):
        seen["body"] = json.loads(request.content)
        seen["path"] = request.url.path
        msg = {"id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5", "content": [],
               "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 11, "output_tokens": 0}}
        events = [
            ("message_start", {"type": "message_start", "message": msg}),
            ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "On it."}}),
            ("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("content_block_start", {"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "run_command", "input": {}}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"slide.new\"}"}}),
            ("content_block_stop", {"type": "content_block_stop", "index": 1}),
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": None}, "usage": {"output_tokens": 9}}),
            ("message_stop", {"type": "message_stop"}),
        ]
        return httpx2.Response(200, content=sse(events), headers={"content-type": "text/event-stream"})

    cfg = ProviderCfg(kind="anthropic", base_url="http://litellm.test", api_key="sk", model="claude-sonnet-5-5", vision=True)
    p = build_provider(cfg, http_client=httpx2.AsyncClient(transport=httpx2.MockTransport(handler)))
    history = [UserMsg("add"), AssistantMsg(text="", tool_uses=[ToolUse("t0", "run_command", {})],
                                            raw=[{"type": "thinking", "thinking": "", "signature": "sig"}, {"type": "tool_use", "id": "t0", "name": "run_command", "input": {}}], kind="anthropic"),
               ToolResults([ToolResult("t0", False, [{"type": "text", "text": "bad"}, {"type": "image", "media_type": "image/png", "data": "AAA"}])])]
    events = await run(p, history)
    assert [e.text for e in events if isinstance(e, TextDelta)] == ["On it."]
    done = events[-1]
    assert done.assistant.tool_uses == [ToolUse("toolu_1", "run_command", {"command": "slide.new"})]
    assert done.assistant.raw[1]["type"] == "tool_use"
    assert done.usage["input_tokens"] == 11 and done.usage["output_tokens"] == 9
    body = seen["body"]
    assert seen["path"].endswith("/v1/messages")
    assert body["model"] == "claude-sonnet-5-5"
    assert body["tools"][0]["input_schema"]["type"] == "object"
    assert body["messages"][1]["content"][0]["type"] == "thinking"  # raw replayed unchanged
    tr = body["messages"][2]["content"][0]
    assert tr["type"] == "tool_result" and tr["is_error"] is True and tr["content"][1]["type"] == "image"

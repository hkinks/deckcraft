"""The Claude Code provider (Claude through the person's Claude subscription, via the Agent SDK)."""

import asyncio

from claude_agent_sdk import AssistantMessage, ResultMessage, StreamEvent, TextBlock

from deckcraft_server.config import ProviderCfg
from deckcraft_server.providers import build_provider
from deckcraft_server.providers.base import AssistantMsg, ToolResult, UserMsg
from deckcraft_server.providers.claude_code import ClaudeCodeProvider

TOOLS = [{"name": "add_slide", "description": "Add a slide", "inputSchema": {"type": "object", "properties": {"title": {"type": "string"}}}},
         {"name": "render_slide", "description": "Render", "inputSchema": {"type": "object"}}]


def result_message(text):
    return ResultMessage(subtype="success", duration_ms=1, duration_api_ms=1, is_error=False, num_turns=1, session_id="s",
                         result=text, usage={"input_tokens": 3, "cache_read_input_tokens": 10, "output_tokens": 4})


class FakeClient:
    """Stands in for ClaudeSDKClient: calls the deck tools the way Claude Code would."""

    def __init__(self, options, tools, script):
        self.options, self.tools, self.script = options, {t.name: t for t in tools}, script
        self.prompt = None
        self.interrupted = False
        self._interrupt = asyncio.Event()
        self.tool_outputs = []

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        return False

    async def query(self, prompt):
        self.prompt = prompt

    async def interrupt(self):
        self.interrupted = True
        self._interrupt.set()

    async def receive_response(self):
        for step in self.script:
            if step[0] == "tool":
                self.tool_outputs.append(await self.tools[step[1]].handler(step[2]))
            elif step[0] == "text":
                yield StreamEvent(uuid="u", session_id="s", event={"type": "content_block_delta", "delta": {"type": "text_delta", "text": step[1]}})
                yield AssistantMessage(content=[TextBlock(step[1])], model="m")
            elif step[0] == "wait":
                # Like Claude Code, an interrupt ends the response early.
                try:
                    await asyncio.wait_for(self._interrupt.wait(), step[1])
                except asyncio.TimeoutError:
                    pass
        yield result_message("".join(s[1] for s in self.script if s[0] == "text"))


def provider(script):
    made = []

    def factory(options, tools):
        made.append(FakeClient(options, tools, script))
        return made[-1]

    cfg = ProviderCfg(kind="claude_code", model="claude-sonnet-5-5")
    return ClaudeCodeProvider(cfg, client_factory=factory), made


async def run(p, history, execute=None, stop=None):
    deltas = []

    async def on_delta(t):
        deltas.append(t)

    async def default_execute(id, name, input):
        return ToolResult(id, True, [{"type": "text", "text": f"{name} ok"}, {"type": "image", "media_type": "image/png", "data": "QUJD"}])

    out = await p.drive("SYSTEM", history, TOOLS, execute or default_execute, on_delta, stop or asyncio.Event())
    return out, deltas


def test_config_needs_no_base_url_for_claude_code():
    assert build_provider(ProviderCfg(kind="claude_code", model="claude-sonnet-5-5")).kind == "claude_code"


async def test_tools_reach_the_editor_and_the_reply_streams_back():
    p, made = provider([("tool", "add_slide", {"title": "Roadmap"}), ("text", "Added it.")])
    (text, usage), deltas = await run(p, [UserMsg("add a roadmap slide")])
    c = made[0]
    assert c.tool_outputs[0]["content"][0] == {"type": "text", "text": "add_slide ok"}
    assert c.tool_outputs[0]["content"][1] == {"type": "image", "data": "QUJD", "mimeType": "image/png"}
    assert not c.tool_outputs[0].get("is_error")
    assert text == "Added it." and deltas == ["Added it."]
    assert usage == {"input_tokens": 13, "output_tokens": 4}
    assert c.options.system_prompt == "SYSTEM" and c.options.model == "claude-sonnet-5-5"
    # Only the deck's tools: none of Claude Code's own (files, shell) and no local settings.
    assert c.options.tools == [] and c.options.setting_sources == []
    assert sorted(c.options.allowed_tools) == ["mcp__deck__add_slide", "mcp__deck__render_slide"]


async def test_a_failed_tool_is_reported_as_an_error():
    async def failing(id, name, input):
        return ToolResult(id, False, [{"type": "text", "text": "no such slide"}])

    p, made = provider([("tool", "render_slide", {"slide": 9}), ("text", "Sorry.")])
    await run(p, [UserMsg("x")], execute=failing)
    assert made[0].tool_outputs[0]["is_error"] is True


async def test_earlier_turns_go_into_the_prompt():
    p, made = provider([("text", "ok")])
    history = [UserMsg("make a deck"), AssistantMsg("Made 3 slides.", [], None, "history"), UserMsg("now add one")]
    await run(p, history)
    prompt = made[0].prompt
    assert "make a deck" in prompt and "Made 3 slides." in prompt
    assert prompt.rstrip().endswith("now add one")


async def test_stop_interrupts_claude():
    stop = asyncio.Event()
    p, made = provider([("wait", 5)])
    task = asyncio.create_task(run(p, [UserMsg("x")], stop=stop))
    await asyncio.sleep(0.05)
    stop.set()
    await asyncio.wait_for(task, 2)
    assert made[0].interrupted

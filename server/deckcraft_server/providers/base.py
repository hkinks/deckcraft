"""Provider-neutral conversation format and the `LLMProvider` interface.

The agent loop only sees these types; each provider converts them to and from its wire format.
An assistant message keeps the provider's raw content (`raw`) so a provider can replay its own
turns unchanged (Claude's thinking blocks must be sent back exactly as received).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Protocol, Union


@dataclass
class UserMsg:
    text: str


@dataclass
class ToolUse:
    id: str
    name: str
    input: dict


@dataclass
class AssistantMsg:
    text: str
    tool_uses: list[ToolUse]
    raw: Any
    kind: str


@dataclass
class ToolResult:
    tool_use_id: str
    ok: bool
    # [{"type": "text", "text": ...} | {"type": "image", "media_type": ..., "data": base64}]
    parts: list[dict]


@dataclass
class ToolResults:
    results: list[ToolResult] = field(default_factory=list)


HistoryItem = Union[UserMsg, AssistantMsg, ToolResults]


@dataclass
class TextDelta:
    text: str


@dataclass
class Done:
    stop_reason: str
    usage: dict
    assistant: AssistantMsg


Event = Union[TextDelta, Done]


class LLMProvider(Protocol):
    kind: str
    vision: bool

    def stream(self, system: str, history: list[HistoryItem], tools: list[dict]) -> AsyncIterator[Event]: ...


def text_of(parts: list[dict]) -> str:
    return "\n".join(p.get("text", "") for p in parts if p.get("type") == "text")

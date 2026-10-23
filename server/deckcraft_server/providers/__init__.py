from __future__ import annotations

from ..config import ProviderCfg
from .base import LLMProvider


def build_provider(cfg: ProviderCfg, http_client=None) -> LLMProvider:
    if cfg.kind == "anthropic":
        from .anthropic_provider import AnthropicProvider

        return AnthropicProvider(cfg, http_client)
    from .openai_compat import OpenAICompatProvider

    return OpenAICompatProvider(cfg, http_client)

"""Server settings: `config.toml` with `${ENV}` references, overridden by environment variables.

LLM base URLs and keys live only here; nothing in this module is ever sent to the browser except
each provider's id, model name and vision flag.
"""

from __future__ import annotations

import os
import re
import tomllib
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, Field

_REF = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


class ProviderCfg(BaseModel):
    kind: Literal["anthropic", "openai_compat"]
    base_url: str
    api_key: str = ""
    model: str
    vision: bool = False
    # Shown in the agent pane's model picker; the provider id when empty.
    label: str = ""
    max_tokens: int = 16000
    # Anthropic only: adaptive thinking and effort (Sonnet 5.5 defaults to high; medium suits agent loops).
    thinking: bool = True
    effort: str | None = "medium"


class Limits(BaseModel):
    turns_per_minute: int = 6
    max_tool_calls_per_turn: int = 60
    max_turn_seconds: float = 300
    tool_call_timeout_seconds: float = 30


class Settings(BaseModel):
    default_provider: str
    providers: dict[str, ProviderCfg]
    limits: Limits = Field(default_factory=Limits)
    site_dir: Path | None = None
    db_path: Path = Path("data/deckcraft.db")
    session_secret: str = ""
    secure_cookie: bool = False


def _expand(value):
    if isinstance(value, str):
        return _REF.sub(lambda m: os.environ.get(m.group(1), ""), value)
    if isinstance(value, dict):
        return {k: _expand(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_expand(v) for v in value]
    return value


def load(path: Path | None) -> Settings:
    raw: dict = {}
    if path is not None and path.exists():
        raw = tomllib.loads(path.read_text())
    raw = _expand(raw)
    env = os.environ
    if v := env.get("AGENT_DEFAULT_PROVIDER"):
        raw["default_provider"] = v
    if v := env.get("DECKCRAFT_SITE_DIR"):
        raw["site_dir"] = v
    if v := env.get("DECKCRAFT_DB"):
        raw["db_path"] = v
    if v := env.get("SESSION_SECRET"):
        raw["session_secret"] = v
    providers = raw.setdefault("providers", {})
    for p in providers.values():
        if p.get("kind") == "anthropic":
            if v := env.get("LITELLM_BASE_URL"):
                p["base_url"] = v
            if v := env.get("LITELLM_API_KEY"):
                p["api_key"] = v
    settings = Settings.model_validate(raw)
    for pid, p in settings.providers.items():
        if not p.label:
            p.label = pid
    if settings.default_provider not in settings.providers:
        raise ValueError(f"default_provider {settings.default_provider!r} is not a configured provider")
    if not settings.session_secret:
        import secrets

        settings.session_secret = secrets.token_hex(32)
    return settings

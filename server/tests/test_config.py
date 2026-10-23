from deckcraft_server.config import load

TOML = """
default_provider = "claude"
session_secret = "s3cret"

[providers.claude]
kind = "anthropic"
base_url = "${LITELLM_BASE_URL}"
api_key = "${LITELLM_API_KEY}"
model = "claude-sonnet-5-5"
vision = true

[providers.qwen-local]
kind = "openai_compat"
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-27b-q5"

[limits]
max_tool_calls_per_turn = 10
"""


def test_expands_env_references(tmp_path, monkeypatch):
    monkeypatch.setenv("LITELLM_BASE_URL", "https://llm.example/")
    monkeypatch.setenv("LITELLM_API_KEY", "sk-test")
    p = tmp_path / "config.toml"
    p.write_text(TOML)
    s = load(p)
    assert s.providers["claude"].base_url == "https://llm.example/"
    assert s.providers["claude"].api_key == "sk-test"
    assert s.providers["qwen-local"].vision is False
    assert s.limits.max_tool_calls_per_turn == 10
    assert s.limits.turns_per_minute == 6


def test_env_overrides_default_provider_and_db(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENT_DEFAULT_PROVIDER", "qwen-local")
    monkeypatch.setenv("DECKCRAFT_DB", str(tmp_path / "x.db"))
    p = tmp_path / "config.toml"
    p.write_text(TOML)
    s = load(p)
    assert s.default_provider == "qwen-local"
    assert s.db_path == tmp_path / "x.db"


def test_unknown_default_provider_is_rejected(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENT_DEFAULT_PROVIDER", "nope")
    p = tmp_path / "config.toml"
    p.write_text(TOML)
    import pytest

    with pytest.raises(ValueError):
        load(p)


def test_missing_env_reference_becomes_empty(tmp_path, monkeypatch):
    monkeypatch.delenv("LITELLM_API_KEY", raising=False)
    p = tmp_path / "config.toml"
    p.write_text(TOML)
    assert load(p).providers["claude"].api_key == ""

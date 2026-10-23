import asyncio
import io

from deckcraft_server.auth.local import verify_password
from deckcraft_server.cli import main
from deckcraft_server.store import Store


def test_users_add_passwd_disable(tmp_path, monkeypatch):
    db = tmp_path / "c.db"
    monkeypatch.setenv("DECKCRAFT_DB", str(db))
    cfg = tmp_path / "config.toml"
    cfg.write_text('default_provider = "q"\n[providers.q]\nkind = "openai_compat"\nbase_url = "http://x/v1"\nmodel = "m"\n')
    monkeypatch.setattr("sys.stdin", io.StringIO("first-pw\n"))
    assert main(["--config", str(cfg), "users", "add", "ana", "--admin", "--password-stdin"]) == 0
    u = asyncio.run(Store(db).get_user("ana"))
    assert u.admin and verify_password(u.password_hash, "first-pw")
    monkeypatch.setattr("sys.stdin", io.StringIO("second-pw\n"))
    assert main(["--config", str(cfg), "users", "passwd", "ana", "--password-stdin"]) == 0
    assert verify_password(asyncio.run(Store(db).get_user("ana")).password_hash, "second-pw")
    assert main(["--config", str(cfg), "users", "disable", "ana"]) == 0
    assert asyncio.run(Store(db).get_user("ana")).disabled


def test_adding_existing_user_fails(tmp_path, monkeypatch):
    monkeypatch.setenv("DECKCRAFT_DB", str(tmp_path / "c.db"))
    cfg = tmp_path / "config.toml"
    cfg.write_text('default_provider = "q"\n[providers.q]\nkind = "openai_compat"\nbase_url = "http://x/v1"\nmodel = "m"\n')
    monkeypatch.setattr("sys.stdin", io.StringIO("pw\npw\n"))
    assert main(["--config", str(cfg), "users", "add", "bo", "--password-stdin"]) == 0
    monkeypatch.setattr("sys.stdin", io.StringIO("pw\n"))
    assert main(["--config", str(cfg), "users", "add", "bo", "--password-stdin"]) == 1

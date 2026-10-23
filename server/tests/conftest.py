import pytest

from deckcraft_server.store import Store


@pytest.fixture
async def store(tmp_path):
    s = Store(tmp_path / "test.db")
    await s.init()
    yield s
    await s.close()

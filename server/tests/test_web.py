import asyncio

import httpx
import pytest

from deckcraft_server.agent import web
from deckcraft_server.agent.web import WebCfg, WebTools

DDG_HTML = """
<html><body>
<div class="result"><h2 class="result__title">
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Faurora&amp;rut=x">Aurora &amp; you</a></h2>
<a class="result__snippet" href="#">The <b>aurora</b> borealis is a natural light display.</a></div>
<div class="result"><h2 class="result__title">
<a class="result__a" href="https://second.example/page">Second</a></h2>
<a class="result__snippet">Another snippet.</a></div>
</body></html>
"""

PAGE = """<html><head><title>Northern lights</title><style>p{color:red}</style>
<script>var x = 1;</script></head><body><nav>Menu Home About</nav>
<h1>Aurora</h1><p>Seen near the poles.</p><p>Best in   winter.</p></body></html>"""


def tools(handler, **cfg):
    # Public addresses only matter for fetch; tests resolve every name to a public IP.
    return WebTools(WebCfg(**cfg), transport=httpx.MockTransport(handler), resolve=lambda host: ["93.184.216.34"])


def test_duckduckgo_results_are_parsed_and_redirects_unwrapped():
    t = tools(lambda req: httpx.Response(200, text=DDG_HTML))
    res = asyncio.run(t.search("aurora", 5))
    assert res == [
        {"title": "Aurora & you", "url": "https://example.org/aurora", "snippet": "The aurora borealis is a natural light display."},
        {"title": "Second", "url": "https://second.example/page", "snippet": "Another snippet."},
    ]


def test_searxng_backend_uses_its_json_api():
    seen = {}

    def handler(req):
        seen["url"] = str(req.url)
        return httpx.Response(200, json={"results": [{"title": "T", "url": "https://a.example", "content": "C"}]})

    t = tools(handler, search="searxng", base_url="http://searx.local:8080")
    assert asyncio.run(t.search("q", 3)) == [{"title": "T", "url": "https://a.example", "snippet": "C"}]
    assert seen["url"].startswith("http://searx.local:8080/search?") and "format=json" in seen["url"]


def test_fetch_returns_readable_text_without_scripts_or_navigation():
    t = tools(lambda req: httpx.Response(200, text=PAGE, headers={"content-type": "text/html; charset=utf-8"}))
    text = asyncio.run(t.fetch("https://example.org/aurora", 10_000))
    assert text.startswith("Northern lights\n")
    assert "Aurora" in text and "Seen near the poles." in text and "Best in winter." in text
    assert "var x" not in text and "color:red" not in text and "Menu Home" not in text


def test_fetch_is_cut_to_the_limit():
    t = tools(lambda req: httpx.Response(200, text="<p>" + "word " * 5000 + "</p>", headers={"content-type": "text/html"}))
    text = asyncio.run(t.fetch("https://example.org/", 500))
    assert len(text) <= 600 and text.endswith("[truncated]")


@pytest.mark.parametrize("url", ["http://127.0.0.1/", "http://localhost:8770/api/me", "http://10.0.0.5/", "http://[::1]/", "file:///etc/passwd", "ftp://x.example/"])
def test_fetch_refuses_local_and_private_addresses(url):
    t = WebTools(WebCfg(), transport=httpx.MockTransport(lambda r: httpx.Response(200, text="secret")))
    with pytest.raises(web.WebError):
        asyncio.run(t.fetch(url, 1000))


def test_redirect_to_a_private_address_is_refused():
    def handler(req):
        if req.url.host == "example.org":
            return httpx.Response(302, headers={"location": "http://192.168.1.1/admin"})
        return httpx.Response(200, text="router")

    t = WebTools(WebCfg(), transport=httpx.MockTransport(handler), resolve=lambda host: ["192.168.1.1"] if host.startswith("192.") else ["93.184.216.34"])
    with pytest.raises(web.WebError):
        asyncio.run(t.fetch("https://example.org/", 1000))


def test_tool_definitions_and_call_results():
    t = tools(lambda req: httpx.Response(200, text=DDG_HTML))
    names = [d["name"] for d in t.definitions()]
    assert names == ["web_search", "web_fetch"]
    ok, parts = asyncio.run(t.call("web_search", {"query": "aurora"}))
    assert ok and "https://example.org/aurora" in parts[0]["text"]
    ok, parts = asyncio.run(t.call("web_search", {}))
    assert not ok and "query" in parts[0]["text"]
    ok, parts = asyncio.run(t.call("web_fetch", {"url": "http://127.0.0.1/"}))
    assert not ok

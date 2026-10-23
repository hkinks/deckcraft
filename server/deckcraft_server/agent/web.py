"""Web search and page fetching for the agent, run on the server (not round-tripped to the
browser).

Search backends: DuckDuckGo's HTML endpoint (no key; the default), SearXNG (self-hosted JSON
API), Brave Search and Tavily (API keys). Fetching follows redirects itself and checks every hop:
only http(s) to public addresses, so the agent can't be pointed at the server's own network.
"""

from __future__ import annotations

import html
import ipaddress
import json
import re
import socket
from html.parser import HTMLParser
from typing import Callable, Literal
from urllib.parse import parse_qs, urljoin, urlparse

import httpx
from pydantic import BaseModel

USER_AGENT = "Mozilla/5.0 (compatible; DeckCraft-agent/0.1; +https://github.com/storytold/deckcraft)"
MAX_BYTES = 3 * 1024 * 1024
MAX_REDIRECTS = 5


class WebCfg(BaseModel):
    enabled: bool = True
    search: Literal["duckduckgo", "searxng", "brave", "tavily"] = "duckduckgo"
    api_key: str = ""
    # SearXNG's address, e.g. http://localhost:8888
    base_url: str = ""
    max_results: int = 6
    fetch_max_chars: int = 20_000
    timeout_seconds: float = 15.0


class WebError(Exception):
    pass


def _public(ip: str) -> bool:
    try:
        a = ipaddress.ip_address(ip)
    except ValueError:
        return False
    return a.is_global and not (a.is_private or a.is_loopback or a.is_link_local or a.is_multicast or a.is_reserved)


def _resolve(host: str) -> list[str]:
    try:
        return sorted({info[4][0] for info in socket.getaddrinfo(host, None)})
    except OSError as e:
        raise WebError(f"can't resolve {host}: {e}") from e


class _Text(HTMLParser):
    """Readable text of a page: title, headings and paragraphs; no scripts, styles or menus."""

    SKIP = {"script", "style", "noscript", "nav", "footer", "header", "svg", "form", "iframe", "template"}
    BLOCK = {"p", "div", "section", "article", "li", "tr", "br", "h1", "h2", "h3", "h4", "h5", "h6", "table", "ul", "ol", "blockquote", "pre"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.out: list[str] = []
        self.title: list[str] = []
        self.skip = 0
        self.in_title = False

    def handle_starttag(self, tag, attrs):
        if tag in self.SKIP:
            self.skip += 1
        elif tag == "title":
            self.in_title = True
        elif tag in self.BLOCK:
            self.out.append("\n")

    def handle_endtag(self, tag):
        if tag in self.SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag == "title":
            self.in_title = False
        elif tag in self.BLOCK:
            self.out.append("\n")

    def handle_data(self, data):
        if self.in_title:
            self.title.append(data)
        elif not self.skip:
            self.out.append(data)

    def text(self) -> str:
        body = "".join(self.out)
        lines = [re.sub(r"[ \t\r\f\v]+", " ", line).strip() for line in body.split("\n")]
        body = "\n".join(line for line in lines if line)
        title = " ".join("".join(self.title).split())
        return f"{title}\n\n{body}" if title else body


def page_text(markup: str) -> str:
    p = _Text()
    p.feed(markup)
    p.close()
    return p.text()


def _strip_tags(s: str) -> str:
    return " ".join(html.unescape(re.sub(r"<[^>]+>", "", s)).split())


def _ddg_url(href: str) -> str:
    href = html.unescape(href)
    if href.startswith("//"):
        href = "https:" + href
    u = urlparse(href)
    if u.netloc.endswith("duckduckgo.com") and u.path.startswith("/l/"):
        target = parse_qs(u.query).get("uddg")
        if target:
            return target[0]
    return href


def parse_duckduckgo(markup: str, count: int) -> list[dict]:
    out = []
    links = re.finditer(r'<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>', markup, re.S)
    snippets = [m.group(1) for m in re.finditer(r'<a[^>]*class="result__snippet"[^>]*>(.*?)</a>', markup, re.S)]
    for i, m in enumerate(links):
        url = _ddg_url(m.group(1))
        if not url.startswith(("http://", "https://")):
            continue
        out.append({"title": _strip_tags(m.group(2)), "url": url, "snippet": _strip_tags(snippets[i]) if i < len(snippets) else ""})
        if len(out) >= count:
            break
    return out


class WebTools:
    def __init__(self, cfg: WebCfg, transport: httpx.AsyncBaseTransport | None = None, resolve: Callable[[str], list[str]] | None = None):
        self.cfg = cfg
        self.transport = transport
        self.resolve = resolve or _resolve

    def _client(self) -> httpx.AsyncClient:
        return httpx.AsyncClient(
            transport=self.transport,
            timeout=self.cfg.timeout_seconds,
            follow_redirects=False,
            headers={"user-agent": USER_AGENT, "accept-language": "en"},
        )

    def definitions(self) -> list[dict]:
        return [
            {
                "name": "web_search",
                "description": "Search the web. Returns titles, URLs and snippets. Use it for facts, figures, current events or examples the deck needs; then web_fetch a result to read it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "What to search for"},
                        "count": {"type": "integer", "description": f"Number of results (default {self.cfg.max_results}, at most 10)"},
                    },
                    "required": ["query"],
                },
            },
            {
                "name": "web_fetch",
                "description": "Read a web page (http/https) as plain text: title, headings and paragraphs, cut to maxChars.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "The page's address"},
                        "maxChars": {"type": "integer", "description": f"Longest text returned (default {self.cfg.fetch_max_chars})"},
                    },
                    "required": ["url"],
                },
            },
        ]

    async def call(self, name: str, input: dict) -> tuple[bool, list[dict]]:
        try:
            if name == "web_search":
                q = input.get("query")
                if not isinstance(q, str) or not q.strip():
                    return False, [{"type": "text", "text": "web_search needs a `query`"}]
                count = input.get("count")
                count = count if isinstance(count, int) and 0 < count <= 10 else self.cfg.max_results
                results = await self.search(q.strip()[:400], count)
                if not results:
                    return True, [{"type": "text", "text": "No results."}]
                lines = [f"{i + 1}. {r['title']}\n   {r['url']}\n   {r['snippet']}" for i, r in enumerate(results)]
                return True, [{"type": "text", "text": "\n".join(lines)}]
            if name == "web_fetch":
                url = input.get("url")
                if not isinstance(url, str) or not url.strip():
                    return False, [{"type": "text", "text": "web_fetch needs a `url`"}]
                n = input.get("maxChars")
                n = n if isinstance(n, int) and 200 <= n <= 100_000 else self.cfg.fetch_max_chars
                return True, [{"type": "text", "text": await self.fetch(url.strip(), n)}]
        except WebError as e:
            return False, [{"type": "text", "text": str(e)}]
        except httpx.HTTPError as e:
            return False, [{"type": "text", "text": f"{name} failed: {type(e).__name__}: {e}"}]
        return False, [{"type": "text", "text": f"unknown web tool {name!r}"}]

    async def search(self, query: str, count: int) -> list[dict]:
        c = self.cfg
        async with self._client() as client:
            if c.search == "duckduckgo":
                r = await client.post("https://html.duckduckgo.com/html/", data={"q": query})
                r.raise_for_status()
                return parse_duckduckgo(r.text, count)
            if c.search == "searxng":
                if not c.base_url:
                    raise WebError("web search: set web.base_url to the SearXNG address")
                r = await client.get(c.base_url.rstrip("/") + "/search", params={"q": query, "format": "json"})
                r.raise_for_status()
                items = r.json().get("results", [])
                return [{"title": i.get("title", ""), "url": i.get("url", ""), "snippet": i.get("content", "")} for i in items[:count]]
            if not c.api_key:
                raise WebError(f"web search: set web.api_key for {c.search}")
            if c.search == "brave":
                r = await client.get(
                    "https://api.search.brave.com/res/v1/web/search",
                    params={"q": query, "count": count},
                    headers={"x-subscription-token": c.api_key, "accept": "application/json"},
                )
                r.raise_for_status()
                items = r.json().get("web", {}).get("results", [])
                return [{"title": i.get("title", ""), "url": i.get("url", ""), "snippet": _strip_tags(i.get("description", ""))} for i in items[:count]]
            r = await client.post("https://api.tavily.com/search", json={"api_key": c.api_key, "query": query, "max_results": count})
            r.raise_for_status()
            items = r.json().get("results", [])
            return [{"title": i.get("title", ""), "url": i.get("url", ""), "snippet": i.get("content", "")} for i in items[:count]]

    def _check(self, url: str) -> None:
        u = urlparse(url)
        if u.scheme not in ("http", "https") or not u.hostname:
            raise WebError(f"web_fetch only reads http(s) pages, not {url!r}")
        host = u.hostname
        addrs = [host] if _is_ip(host) else self.resolve(host)
        if not addrs or not all(_public(a) for a in addrs):
            raise WebError(f"web_fetch won't read {host}: it is a local or private address")

    async def fetch(self, url: str, max_chars: int) -> str:
        async with self._client() as client:
            for _ in range(MAX_REDIRECTS + 1):
                self._check(url)
                async with client.stream("GET", url) as r:
                    if r.is_redirect and (loc := r.headers.get("location")):
                        url = urljoin(url, loc)
                        continue
                    r.raise_for_status()
                    kind = r.headers.get("content-type", "").split(";")[0].strip().lower()
                    if kind and not (kind.startswith("text/") or kind in ("application/json", "application/xhtml+xml", "application/xml")):
                        raise WebError(f"web_fetch reads text pages; {url} is {kind}")
                    body = bytearray()
                    async for chunk in r.aiter_bytes():
                        body += chunk
                        if len(body) > MAX_BYTES:
                            break
                    text = bytes(body).decode(r.encoding or "utf-8", errors="replace")
                    if kind == "application/json":
                        try:
                            text = json.dumps(json.loads(text), indent=1, ensure_ascii=False)
                        except ValueError:
                            pass
                    elif "html" in kind or "<html" in text[:2000].lower():
                        text = page_text(text)
                    if len(text) > max_chars:
                        text = text[:max_chars].rstrip() + "\n[truncated]"
                    return text
            raise WebError(f"web_fetch: too many redirects from {url}")


def _is_ip(host: str) -> bool:
    try:
        ipaddress.ip_address(host.strip("[]"))
        return True
    except ValueError:
        return False

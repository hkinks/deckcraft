"""Files the editor sends in tool results (a workspace PDF), turned into text for the model."""

from __future__ import annotations

import base64
import binascii
import io

MAX_CHARS = 60_000


def file_part_to_text(part: dict) -> dict:
    name = str(part.get("name") or "file")[:200]
    try:
        data = base64.b64decode(str(part.get("data", "")), validate=True)
    except (binascii.Error, ValueError):
        return {"type": "text", "text": f"{name}: couldn't read the file (bad data)"}
    kind = str(part.get("media_type", ""))
    if kind == "application/pdf" or name.lower().endswith(".pdf"):
        return {"type": "text", "text": _pdf_text(name, data)}
    return {"type": "text", "text": f"{name}: can't read files of type {kind or 'unknown'}"}


def _pdf_text(name: str, data: bytes) -> str:
    try:
        from pypdf import PdfReader

        reader = PdfReader(io.BytesIO(data))
        pages = []
        total = 0
        for i, page in enumerate(reader.pages):
            t = (page.extract_text() or "").strip()
            pages.append(f"[page {i + 1}]\n{t}")
            total += len(t)
            if total > MAX_CHARS:
                pages.append(f"[cut: {len(reader.pages) - i - 1} more page(s)]")
                break
        text = "\n\n".join(pages)
        return f"{name} ({len(reader.pages)} pages)\n\n{text[:MAX_CHARS]}"
    except Exception as e:  # noqa: BLE001 - any parse failure is the file's problem, not the turn's
        return f"{name}: couldn't read the PDF ({type(e).__name__}: {e})"

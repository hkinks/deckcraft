import base64

from deckcraft_server.agent.files import file_part_to_text
from deckcraft_server.agent.prompt import editor_context, system_prompt


def tiny_pdf(text: str) -> bytes:
    """A one-page PDF with `text` in Helvetica (hand-built, valid enough for pypdf)."""
    stream = f"BT /F1 12 Tf 72 720 Td ({text}) Tj ET".encode()
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + o + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


def test_a_pdf_becomes_its_text():
    part = {"type": "file", "name": "report.pdf", "media_type": "application/pdf", "data": base64.b64encode(tiny_pdf("Revenue grew 12 percent")).decode()}
    out = file_part_to_text(part)
    assert out["type"] == "text"
    assert "report.pdf" in out["text"] and "Revenue grew 12 percent" in out["text"]


def test_a_broken_file_says_so():
    out = file_part_to_text({"type": "file", "name": "x.pdf", "media_type": "application/pdf", "data": base64.b64encode(b"not a pdf").decode()})
    assert out["type"] == "text" and "couldn't read" in out["text"]


def test_workspace_is_described_in_the_editor_context():
    ctx = editor_context({"slides": 3, "workspace": {"name": "Launch", "file_count": 2, "files": ["brief.md", "img/logo.png"]}})
    assert "Workspace folder “Launch”" in ctx and "brief.md" in ctx


def test_project_instructions_join_the_system_prompt():
    s = system_prompt(None, project="Audience: the board. Keep to 10 slides.")
    assert "Project instructions" in s and "Keep to 10 slides." in s
    assert "Project instructions" not in system_prompt(None)

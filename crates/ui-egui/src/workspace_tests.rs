//! Workspace files: kinds, and the text the agent reads from them.

use deckcraft_engine::Session;
use serde_json::json;

use crate::workspace::{Kind, WsEntry, instructions_file, kind_of, size_label, text_of};

#[test]
fn files_are_sorted_into_kinds() {
    assert_eq!(kind_of("brief.md"), Kind::Text);
    assert_eq!(kind_of("data/Q3.CSV"), Kind::Text);
    assert_eq!(kind_of("img/logo.PNG"), Kind::Image);
    assert_eq!(kind_of("drafts/v2.pptx"), Kind::Deck);
    assert_eq!(kind_of("brand/Corporate.potx"), Kind::Deck);
    assert_eq!(kind_of("brand/colors.thmx"), Kind::Theme);
    assert_eq!(kind_of("notes.docx"), Kind::Word);
    assert_eq!(kind_of("report.pdf"), Kind::Pdf);
    assert_eq!(kind_of("archive.zip"), Kind::Other);
}

#[test]
fn a_deck_reads_as_an_outline() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "titleAndContent", "title": "Goals", "body": "Grow revenue\n\tEMEA first"})).unwrap();
    let bytes = deckcraft_format::save(&s.active().unwrap().doc).unwrap();
    let text = text_of("drafts/plan.deckcraft", &bytes).unwrap().unwrap();
    assert!(text.contains("Slide 2: Goals"), "{text}");
    assert!(text.contains("Grow revenue") && text.contains("EMEA first"), "{text}");
}

#[test]
fn a_word_document_reads_as_paragraphs() {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(br#"<w:document><w:body><w:p><w:r><w:t>Launch brief</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Budget: </w:t></w:r><w:r><w:t>&lt;50k &amp; on time</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        z.finish().unwrap();
    }
    let text = text_of("brief.docx", buf.get_ref()).unwrap().unwrap();
    assert_eq!(text, "Launch brief\nBudget: <50k & on time");
}

#[test]
fn pictures_have_no_text() {
    assert!(text_of("a.png", b"\x89PNG").is_none());
}

#[test]
fn agent_md_at_the_root_is_the_project_instructions() {
    let files = vec![WsEntry { path: "notes/AGENT.md".into(), size: 1 }, WsEntry { path: "AGENT.md".into(), size: 2 }];
    assert_eq!(instructions_file(&files).map(|f| f.size), Some(2));
    assert!(instructions_file(&files[..1]).is_none(), "only at the root");
}

#[test]
fn sizes_read_like_a_file_manager() {
    assert_eq!(size_label(800), "800 B");
    assert_eq!(size_label(12 * 1024), "12 KB");
    assert_eq!(size_label(3 * 1_048_576 + 100_000), "3.1 MB");
}

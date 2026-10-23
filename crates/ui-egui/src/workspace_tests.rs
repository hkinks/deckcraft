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

mod per_deck {
    use std::cell::RefCell;
    use std::rc::Rc;

    use deckcraft_engine::Session;
    use serde_json::json;

    use crate::workspace::{UserOp, WorkspaceHost};
    use crate::workspace_fake::FakeHost;
    use crate::{Services, SlideApp};

    /// A factory whose hosts start without a folder, recording the keys asked for.
    fn app_with_factory() -> (SlideApp, Rc<RefCell<Vec<String>>>) {
        let mut app = SlideApp::new(Session::with_new(), Services::default());
        let keys: Rc<RefCell<Vec<String>>> = Rc::default();
        let k = keys.clone();
        app.set_workspace_factory(Box::new(move |key: &str| {
            k.borrow_mut().push(key.to_string());
            Box::new(FakeHost::default()) as Box<dyn WorkspaceHost>
        }));
        app.workspace_tick();
        (app, keys)
    }

    #[test]
    fn a_new_presentation_starts_without_the_other_decks_folder() {
        let (mut app, _) = app_with_factory();
        app.set_workspace_host(Box::new(FakeHost::with(&[("brief.md", b"x")])));
        assert_eq!(app.workspace().and_then(|w| w.folder_name()).as_deref(), Some("Launch"));
        app.run("file.new", json!({})).unwrap();
        app.workspace_tick();
        assert!(app.workspace().is_some_and(|w| w.folder_name().is_none()), "the new deck has no folder");
        // Back on the first deck, its folder is still there.
        app.session.set_active(0);
        app.workspace_tick();
        assert_eq!(app.workspace().and_then(|w| w.folder_name()).as_deref(), Some("Launch"));
    }

    #[test]
    fn a_deck_opened_from_the_workspace_joins_it() {
        let (mut app, _) = app_with_factory();
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"title": "Draft"})).unwrap();
        let bytes = deckcraft_format::save(&s.active().unwrap().doc).unwrap();
        let host = FakeHost::with(&[("drafts/v1.deckcraft", &bytes)]);
        host.0.borrow_mut().instant = true;
        app.set_workspace_host(Box::new(host));
        let first = app.session.active().unwrap().uid;
        app.workspace_action("drafts/v1.deckcraft", UserOp::Open);
        app.workspace_tick();
        app.workspace_tick();
        let now = app.session.active().unwrap().uid;
        assert_ne!(now, first, "the draft opened");
        assert_eq!(app.workspace().and_then(|w| w.folder_name()).as_deref(), Some("Launch"));
    }

    #[test]
    fn closed_decks_drop_their_workspace() {
        let (mut app, _) = app_with_factory();
        app.run("file.new", json!({})).unwrap();
        app.workspace_tick();
        assert_eq!(app.workspaces.len(), 2);
        app.run("file.close", json!({})).ok();
        app.workspace_tick();
        assert_eq!(app.workspaces.len(), app.session.documents().len());
    }
}

mod rename {
    use deckcraft_engine::Session;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use crate::{Services, SlideApp};

    #[test]
    fn clicking_the_title_renames_the_presentation() {
        let app = SlideApp::new(Session::with_new(), Services::default());
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
            |ui, app: &mut SlideApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(3);
        h.get_by_label("Presentation name").click();
        h.run_steps(2);
        h.get_by_label("Presentation name").type_text("Q4 kickoff");
        h.run_steps(1);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(h.state().session.active().unwrap().title(), "Q4 kickoff");
        // Esc keeps the name.
        h.get_by_label("Presentation name").click();
        h.run_steps(2);
        h.get_by_label("Presentation name").type_text("nope");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert_eq!(h.state().session.active().unwrap().title(), "Q4 kickoff");
    }
}

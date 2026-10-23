//! Autosave and recovery against an in-memory store.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_engine::Session;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::autosave::{Recovered, RecoveryStore};
use crate::{Services, SlideApp};

#[derive(Default)]
struct Mem {
    entries: Vec<Recovered>,
    puts: usize,
    loaded: bool,
}

#[derive(Clone, Default)]
struct Store(Rc<RefCell<Mem>>);

impl RecoveryStore for Store {
    fn put(&mut self, e: Recovered) {
        let mut m = self.0.borrow_mut();
        m.puts += 1;
        m.entries.retain(|x| x.key != e.key);
        m.entries.push(e);
    }
    fn delete(&mut self, key: &str) {
        self.0.borrow_mut().entries.retain(|x| x.key != key);
    }
    fn earlier(&mut self) -> Option<Vec<Recovered>> {
        let mut m = self.0.borrow_mut();
        if m.loaded {
            return None;
        }
        m.loaded = true;
        Some(m.entries.clone())
    }
}

fn app(store: &Store) -> SlideApp {
    let mut app = SlideApp::new(Session::with_new(), Services::default());
    app.set_recovery_store(Box::new(store.clone()));
    if let Some(a) = app.autosave.as_mut() {
        a.interval_ms = 0.0;
    }
    app
}

#[test]
fn unsaved_decks_are_autosaved_and_saved_ones_removed() {
    let store = Store::default();
    let mut app = app(&store);
    app.autosave_tick();
    assert!(store.0.borrow().entries.is_empty(), "a fresh deck has nothing to save");
    app.run("slide.new", json!({"layout": "title"})).unwrap();
    app.autosave_tick();
    assert_eq!(store.0.borrow().entries.len(), 1);
    let bytes = store.0.borrow().entries[0].bytes.clone();
    assert!(deckcraft_format::load(&bytes).unwrap().slides.len() >= 2, "a full .deckcraft copy");
    app.autosave_tick();
    assert_eq!(store.0.borrow().puts, 1, "unchanged decks aren't written again");
    // Saved: the copy goes.
    if let Some(st) = app.session.active_mut() {
        st.saved_doc = st.doc.clone();
    }
    app.autosave_tick();
    assert!(store.0.borrow().entries.is_empty());
}

#[test]
fn any_unsaved_reports_dirty_documents() {
    let store = Store::default();
    let mut app = app(&store);
    assert!(!app.any_unsaved());
    app.run("slide.new", json!({})).unwrap();
    assert!(app.any_unsaved());
}

fn earlier_deck() -> Recovered {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "title", "title": "Rescued"})).unwrap();
    s.execute("slide.new", &json!({})).unwrap();
    let bytes = deckcraft_format::save(&s.active().unwrap().doc).unwrap();
    Recovered { key: "deck:earlier".into(), title: "Lost deck".into(), saved_at: 0.0, bytes }
}

fn harness(store: &Store) -> Harness<'static, SlideApp> {
    let app = app(store);
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    h
}

#[test]
fn earlier_unsaved_work_is_offered_and_restored() {
    let store = Store::default();
    store.0.borrow_mut().entries.push(earlier_deck());
    let mut h = harness(&store);
    h.get_by_label("Restore unsaved work?");
    h.get_by_label("Restore").click();
    h.run_steps(3);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.doc.slides.len(), 3);
    assert!(st.is_dirty(), "the restored deck still needs saving");
    assert!(store.0.borrow().entries.iter().all(|e| e.key != "deck:earlier"));
    assert!(h.query_by_label("Restore unsaved work?").is_none());
}

#[test]
fn earlier_unsaved_work_can_be_discarded() {
    let store = Store::default();
    store.0.borrow_mut().entries.push(earlier_deck());
    let mut h = harness(&store);
    let docs = h.state().session.documents().len();
    h.get_by_label("Discard").click();
    h.run_steps(3);
    assert_eq!(h.state().session.documents().len(), docs);
    assert!(store.0.borrow().entries.is_empty());
}

#[test]
fn offered_decks_say_when_they_were_saved() {
    use crate::autosave::saved_ago;
    let now = 10_000_000_000.0;
    assert_eq!(saved_ago(now, now - 20_000.0), "Autosaved just now");
    assert_eq!(saved_ago(now, now - 60_000.0), "Autosaved 1 min ago");
    assert_eq!(saved_ago(now, now - 5.0 * 60_000.0), "Autosaved 5 min ago");
    assert_eq!(saved_ago(now, now - 3.0 * 3_600_000.0), "Autosaved 3 h ago");
    assert_eq!(saved_ago(now, now - 2.0 * 86_400_000.0), "Autosaved 2 days ago");
    assert_eq!(saved_ago(now, 0.0), "Autosaved earlier");
}

#[test]
fn several_offered_decks_can_be_restored_at_once() {
    let store = Store::default();
    let mut second = earlier_deck();
    second.key = "deck:second".into();
    second.title = "Other deck".into();
    store.0.borrow_mut().entries.extend([earlier_deck(), second]);
    let mut h = harness(&store);
    let docs = h.state().session.documents().len();
    h.get_by_label("Restore all").click();
    h.run_steps(3);
    assert_eq!(h.state().session.documents().len(), docs + 2);
    assert!(store.0.borrow().entries.iter().all(|e| e.key != "deck:earlier" && e.key != "deck:second"));
    assert!(h.query_by_label("Restore unsaved work?").is_none());
}

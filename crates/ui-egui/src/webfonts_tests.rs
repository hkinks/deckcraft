//! Fonts on demand: the families a deck needs are fetched from the server's catalog once, added to
//! the font database, and the slides redrawn.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_engine::Session;
use serde_json::json;

use crate::webfonts::{CatalogFamily, FontFetcher};
use crate::{Services, SlideApp};

#[derive(Default)]
struct Wire {
    asked: Vec<String>,
    deliver: Vec<(String, Vec<Vec<u8>>)>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Wire>>);

impl FontFetcher for Fake {
    fn fetch(&mut self, family: &CatalogFamily) {
        self.0.borrow_mut().asked.push(family.family.clone());
    }
    fn poll(&mut self) -> Vec<(String, Vec<Vec<u8>>)> {
        std::mem::take(&mut self.0.borrow_mut().deliver)
    }
}

fn catalog() -> Vec<CatalogFamily> {
    ["Carlito", "Montserrat", "Zzz Test Sans", "Lora"].iter().map(|f| CatalogFamily { family: f.to_string(), files: vec![format!("{f}.ttf")] }).collect()
}

fn app_with(wire: &Fake) -> SlideApp {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "title", "title": "Hello"})).unwrap();
    let mut app = SlideApp::new(s, Services::default());
    app.set_font_catalog(catalog(), Box::new(wire.clone()));
    app
}

#[test]
fn a_family_the_deck_uses_is_fetched_once() {
    let wire = Fake::default();
    let mut app = app_with(&wire);
    app.run("design.fonts", json!({"name": "Talk", "major": "Montserrat", "minor": "Lora"})).unwrap();
    app.fonts_tick();
    app.fonts_tick();
    let asked = wire.0.borrow().asked.clone();
    assert_eq!(asked.iter().filter(|f| *f == "Montserrat").count(), 1, "{asked:?}");
    assert!(asked.contains(&"Lora".to_string()), "{asked:?}");
}

#[test]
fn an_office_font_brings_its_metric_twin() {
    let wire = Fake::default();
    let mut app = app_with(&wire);
    app.run("design.fonts", json!({"name": "Office", "major": "Calibri", "minor": "Calibri"})).unwrap();
    app.fonts_tick();
    assert!(wire.0.borrow().asked.contains(&"Carlito".to_string()), "{:?}", wire.0.borrow().asked);
}

#[test]
fn the_font_menu_offers_the_whole_catalog() {
    let wire = Fake::default();
    let app = app_with(&wire);
    let fams = crate::ribbon::font_families(&app);
    assert!(fams.iter().any(|f| f == "Lora") && fams.iter().any(|f| f == "Montserrat"), "{fams:?}");
}

#[test]
fn arriving_fonts_are_added_and_the_slides_redrawn() {
    let wire = Fake::default();
    let mut app = app_with(&wire);
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../fonts/tests/fixtures/legacy-regular.ttf")).unwrap();
    let before = app.fonts_generation();
    wire.0.borrow_mut().deliver.push(("Zzz Test Sans".into(), vec![bytes]));
    app.fonts_tick();
    assert!(app.fonts_generation() > before, "the canvas and thumbnails are drawn again");
}

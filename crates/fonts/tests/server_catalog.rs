//! The agent server's font catalog (`scripts/fetch-fonts.py` → `server/fonts/catalog.json`) names
//! each family as its font files do, so a family the web editor loads on demand is found by that
//! name. Skipped when the fonts haven't been fetched.

use std::path::PathBuf;

use deckcraft_fonts::FontDb;

fn catalog_families(json: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = json;
    while let Some(i) = rest.find("\"family\": \"") {
        rest = &rest[i + 11..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn catalog_family_names_match_the_font_files() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../server/fonts");
    let Ok(json) = std::fs::read_to_string(dir.join("catalog.json")) else {
        eprintln!("server/fonts not fetched; skipping");
        return;
    };
    let families = catalog_families(&json);
    assert!(families.len() > 20, "{families:?}");
    let db = FontDb::with_font_dirs(vec![]);
    for entry in std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().is_dir()) {
        for f in std::fs::read_dir(entry.path()).unwrap().flatten() {
            if f.path().extension().is_some_and(|e| e == "ttf") {
                assert!(db.add_font(std::fs::read(f.path()).unwrap()) > 0, "{} adds no faces", f.path().display());
            }
        }
    }
    for family in &families {
        assert!(db.has_family(family), "catalog family {family:?} isn't what its files call themselves");
        let face = db.face(family, "Bold");
        assert!(face.family.eq_ignore_ascii_case(family), "{family} Bold resolves to {}", face.family);
    }
}

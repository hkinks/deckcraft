//! Fonts on demand (the web editor): the agent server offers a catalog of open fonts
//! (`scripts/fetch-fonts.py`), too many to download up front. The families a deck's text uses, or
//! the stand-in for one it lacks (Carlito for Calibri…), are fetched the first time they're
//! needed, added to the font database, and the slides drawn again. The font menu lists the whole
//! catalog, so picking a family is enough to load it.

use std::collections::HashSet;

use deckcraft_fonts::FontDb;

use crate::SlideApp;

/// One family the server offers: its files, relative to `/fonts/`.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogFamily {
    pub family: String,
    pub files: Vec<String>,
}

/// Fetches a family's files (the browser's `fetch`); results arrive later through `poll`.
pub trait FontFetcher {
    fn fetch(&mut self, family: &CatalogFamily);
    /// Families whose files arrived since the last call: (family, file bytes).
    fn poll(&mut self) -> Vec<(String, Vec<Vec<u8>>)>;
}

pub struct WebFonts {
    catalog: Vec<CatalogFamily>,
    fetcher: Box<dyn FontFetcher>,
    /// Families asked for (lowercase), so each is fetched once.
    requested: HashSet<String>,
    /// The document state last checked: (uid, revision).
    seen: Option<(u64, u64)>,
    /// Bumped whenever fonts were added (the slides were drawn again).
    generation: u64,
}

impl WebFonts {
    fn offers(&self, family: &str) -> Option<&CatalogFamily> {
        self.catalog.iter().find(|c| c.family.eq_ignore_ascii_case(family))
    }

    /// The catalog families a deck using `used` needs: each used family the catalog has, or for
    /// one this browser lacks, the first stand-in the catalog has.
    fn needed(&self, used: &[String]) -> Vec<CatalogFamily> {
        let db = FontDb::global();
        let mut out: Vec<CatalogFamily> = vec![];
        let mut want = |c: &CatalogFamily| {
            if !out.iter().any(|o| o.family == c.family) {
                out.push(c.clone());
            }
        };
        for f in used {
            if let Some(c) = self.offers(f) {
                if !db.has_family(f) {
                    want(c);
                }
                continue;
            }
            if db.has_family(f) {
                continue;
            }
            for s in deckcraft_fonts::substitutes(f) {
                if db.has_family(s) {
                    break;
                }
                if let Some(c) = self.offers(s) {
                    want(c);
                    break;
                }
            }
        }
        out
    }
}

impl SlideApp {
    /// Offer `catalog` (the agent server's fonts) through `fetcher`.
    pub fn set_font_catalog(&mut self, catalog: Vec<CatalogFamily>, fetcher: Box<dyn FontFetcher>) {
        self.webfonts = Some(WebFonts { catalog, fetcher, requested: HashSet::new(), seen: None, generation: 0 });
    }

    /// Families the server offers on demand (for the font menu).
    pub fn catalog_families(&self) -> Vec<String> {
        self.webfonts.as_ref().map(|w| w.catalog.iter().map(|c| c.family.clone()).collect()).unwrap_or_default()
    }

    /// How many times arriving fonts made the slides redraw.
    pub fn fonts_generation(&self) -> u64 {
        self.webfonts.as_ref().map_or(0, |w| w.generation)
    }

    /// Add fonts that arrived; when the deck changed, fetch the families it now needs.
    pub fn fonts_tick(&mut self) {
        let Some(mut w) = self.webfonts.take() else { return };
        let mut added = 0;
        for (family, files) in w.fetcher.poll() {
            let mut n = 0;
            for bytes in files {
                n += FontDb::global().add_font(bytes);
            }
            if n == 0 {
                log::warn!("font {family}: no usable faces");
            }
            added += n;
        }
        if added > 0 {
            w.generation += 1;
            self.textures.invalidate();
        }
        let state = self.session.active().map(|d| (d.uid, d.revision));
        if state.is_some() && state != w.seen {
            w.seen = state;
            if let Some(d) = self.session.active() {
                // The theme's fonts too: placeholder prompts and new text are drawn in them.
                let mut used = deckcraft_engine::fonts::used_fonts(&d.doc);
                for m in &d.doc.masters {
                    used.push(m.theme.fonts.major.latin.clone());
                    used.push(m.theme.fonts.minor.latin.clone());
                }
                for c in w.needed(&used) {
                    if w.requested.insert(c.family.to_lowercase()) {
                        w.fetcher.fetch(&c);
                    }
                }
            }
        }
        self.webfonts = Some(w);
    }
}

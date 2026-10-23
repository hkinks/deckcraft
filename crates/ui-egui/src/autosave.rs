//! Autosave and recovery for hosts without a file system (the browser).
//!
//! Every few seconds, each presentation with unsaved changes is written to the host's
//! [`RecoveryStore`] as a complete `.deckcraft` file (pictures included); a presentation that is
//! saved again, or closed, loses its entry. When the app starts and the store holds entries from
//! an earlier visit (the tab was closed or crashed with unsaved work), the app offers to restore
//! them. The desktop app has its own AutoRecover (`deckcraft_engine::recovery`).

use std::collections::HashMap;

use serde_json::json;

use crate::SlideApp;

/// One saved copy.
#[derive(Clone, Debug, PartialEq)]
pub struct Recovered {
    pub key: String,
    pub title: String,
    /// When it was written (ms since the epoch).
    pub saved_at: f64,
    /// A `.deckcraft` file.
    pub bytes: Vec<u8>,
}

/// Where copies live (IndexedDB in the browser). Writes may finish later; that's fine.
pub trait RecoveryStore {
    fn put(&mut self, e: Recovered);
    fn delete(&mut self, key: &str);
    /// Entries left by an earlier visit, once: `None` until they are loaded, and after that.
    fn earlier(&mut self) -> Option<Vec<Recovered>>;
}

pub struct Autosave {
    store: Box<dyn RecoveryStore>,
    /// How often unsaved decks are written (ms).
    pub interval_ms: f64,
    last_run: f64,
    /// Document uid → revision last written.
    written: HashMap<u64, u64>,
    /// Earlier work waiting for the user's answer.
    pub offered: Vec<Recovered>,
}

impl Autosave {
    pub fn new(store: Box<dyn RecoveryStore>) -> Self {
        Self { store, interval_ms: 3000.0, last_run: f64::NEG_INFINITY, written: HashMap::new(), offered: vec![] }
    }
}

fn key(uid: u64) -> String {
    format!("deck:{uid:016x}")
}

impl SlideApp {
    pub fn set_recovery_store(&mut self, store: Box<dyn RecoveryStore>) {
        self.autosave = Some(Autosave::new(store));
    }

    /// Any open presentation has changes that aren't saved.
    pub fn any_unsaved(&self) -> bool {
        self.session.documents().iter().any(|d| d.is_dirty())
    }

    /// Write unsaved decks (at most every `interval_ms`), drop entries of saved or closed ones,
    /// and pick up earlier work to offer.
    pub fn autosave_tick(&mut self) {
        let Some(mut a) = self.autosave.take() else { return };
        if let Some(earlier) = a.store.earlier() {
            a.offered = earlier;
        }
        let now = crate::now_ms();
        if now - a.last_run >= a.interval_ms {
            a.last_run = now;
            let mut live = std::collections::HashSet::new();
            for d in self.session.documents() {
                live.insert(d.uid);
                if d.is_dirty() {
                    if a.written.get(&d.uid) != Some(&d.revision) {
                        match deckcraft_format::save(&d.doc) {
                            Ok(bytes) => {
                                a.store.put(Recovered { key: key(d.uid), title: d.title(), saved_at: now, bytes });
                                a.written.insert(d.uid, d.revision);
                            }
                            Err(e) => log::warn!("autosave of {}: {e}", d.title()),
                        }
                    }
                } else if a.written.remove(&d.uid).is_some() {
                    a.store.delete(&key(d.uid));
                }
            }
            let gone: Vec<u64> = a.written.keys().filter(|u| !live.contains(u)).copied().collect();
            for u in gone {
                a.written.remove(&u);
                a.store.delete(&key(u));
            }
        }
        self.autosave = Some(a);
    }

    /// Open an earlier copy as an unsaved presentation and forget it.
    fn restore_recovered(&mut self, e: &Recovered) {
        let data = deckcraft_engine::cmd::base64_encode(&e.bytes);
        match self.run("file.openBytes", json!({"name": format!("{}.deckcraft", e.title), "data": data})) {
            Ok(_) => {
                if let Some(st) = self.session.active_mut() {
                    // Still unsaved: Save is what makes it safe.
                    st.saved_doc = std::sync::Arc::new(deckcraft_model::Presentation::default());
                    st.path = None;
                    st.title = e.title.clone();
                }
            }
            Err(err) => self.set_status(format!("Couldn't restore {}: {err}", e.title)),
        }
    }
}

/// The "Restore unsaved work?" prompt, while earlier copies wait for an answer.
pub fn prompt(app: &mut SlideApp, ctx: &egui::Context) {
    let Some(a) = app.autosave.as_ref() else { return };
    if a.offered.is_empty() {
        return;
    }
    let offered = a.offered.clone();
    let mut restore: Vec<Recovered> = vec![];
    let mut discard: Vec<String> = vec![];
    egui::Modal::new(egui::Id::new("autosave_restore")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.heading("Restore unsaved work?");
        ui.label("These presentations had changes that weren't saved when the page was closed.");
        ui.add_space(6.0);
        for e in &offered {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&e.title).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Discard").clicked() {
                        discard.push(e.key.clone());
                    }
                    if ui.button("Restore").clicked() {
                        restore.push(e.clone());
                    }
                });
            });
        }
    });
    for e in &restore {
        app.restore_recovered(e);
    }
    if let Some(a) = app.autosave.as_mut() {
        for k in restore.iter().map(|e| e.key.clone()).chain(discard) {
            a.offered.retain(|e| e.key != k);
            a.store.delete(&k);
        }
    }
}

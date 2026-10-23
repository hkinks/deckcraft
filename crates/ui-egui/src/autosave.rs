//! Autosave and recovery for hosts without a file system (the browser).
//!
//! Every few seconds, each presentation with unsaved changes is written to the host's
//! [`RecoveryStore`] as a complete `.deckcraft` file (pictures included); a presentation that is
//! saved again, or closed, loses its entry. When the app starts and the store holds entries from
//! an earlier visit (the tab was closed or crashed with unsaved work), the app offers to restore
//! them. The desktop app has its own AutoRecover (`deckcraft_engine::recovery`).

use std::collections::HashMap;

use serde_json::json;

use egui::vec2;

use crate::SlideApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// The dialog's primary button: the accent fill the app's other dialogs use for OK.
fn primary(ui: &mut egui::Ui, t: &Tokens, label: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(label).color(t.accent_text)).fill(t.accent).min_size(vec2(72.0, 26.0)))
}

/// When a copy was written, for the restore prompt ("Autosaved 5 min ago").
pub fn saved_ago(now_ms: f64, saved_at: f64) -> String {
    if saved_at <= 0.0 || saved_at > now_ms + 60_000.0 {
        return "Autosaved earlier".into();
    }
    let min = ((now_ms - saved_at) / 60_000.0).floor() as i64;
    match min {
        ..1 => "Autosaved just now".into(),
        1..60 => format!("Autosaved {min} min ago"),
        60..1440 => format!("Autosaved {} h ago", min / 60),
        1440..2880 => "Autosaved yesterday".into(),
        _ => format!("Autosaved {} days ago", min / 1440),
    }
}

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
    let t = Tokens::get(ctx);
    let frame = egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.card_border))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(20))
        .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: t.shadow });
    let now = crate::now_ms();
    egui::Modal::new(egui::Id::new("autosave_restore")).frame(frame).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 18.0, t.accent.gamma_multiply(0.14));
            icons::paint(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(20.0, 20.0)), Icon::Save, t.accent, false);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Restore unsaved work?").size(17.0).strong().color(t.text));
                ui.label(
                    egui::RichText::new("These presentations had unsaved changes when the page was closed. DeckCraft kept a copy.")
                        .color(t.text_dim),
                );
            });
        });
        ui.add_space(10.0);
        for e in &offered {
            egui::Frame::new()
                .fill(t.chrome)
                .stroke(egui::Stroke::new(1.0, t.card_border))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Slide, t.text, false);
                        ui.add_space(2.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(egui::Label::new(egui::RichText::new(&e.title).strong().color(t.text)).truncate());
                            ui.label(egui::RichText::new(saved_ago(now, e.saved_at)).small().color(t.text_faint));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary(ui, &t, "Restore").clicked() {
                                restore.push(e.clone());
                            }
                            if ui.add(egui::Button::new("Discard").min_size(vec2(72.0, 26.0))).clicked() {
                                discard.push(e.key.clone());
                            }
                        });
                    });
                });
        }
        if offered.len() > 1 {
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, &t, "Restore all").clicked() {
                    restore = offered.clone();
                    discard.clear();
                }
                if ui.add(egui::Button::new("Discard all").min_size(vec2(72.0, 26.0))).clicked() {
                    restore.clear();
                    discard = offered.iter().map(|e| e.key.clone()).collect();
                }
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

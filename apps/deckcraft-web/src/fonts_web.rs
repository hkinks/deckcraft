//! Fonts on demand from the agent server (`/fonts/catalog.json`, see `scripts/fetch-fonts.py`):
//! the catalog is read once at start; a family's files are fetched when the app asks for them
//! (`deckcraft_ui_egui::webfonts`). Without an agent server there is no catalog and nothing
//! changes.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_ui_egui::webfonts::{CatalogFamily, FontFetcher};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;

type Arrived = Rc<RefCell<Vec<(String, Vec<Vec<u8>>)>>>;

pub struct WebFontFetcher {
    arrived: Arrived,
    ctx: egui::Context,
}

impl FontFetcher for WebFontFetcher {
    fn fetch(&mut self, family: &CatalogFamily) {
        let family = family.clone();
        let arrived = self.arrived.clone();
        let ctx = self.ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut files = vec![];
            for f in &family.files {
                match get_bytes(&format!("/fonts/{}", encode_path(f))).await {
                    Ok(b) => files.push(b),
                    Err(e) => log::warn!("font {}: {f}: {e}", family.family),
                }
            }
            if !files.is_empty() {
                arrived.borrow_mut().push((family.family.clone(), files));
                ctx.request_repaint();
            }
        });
    }

    fn poll(&mut self) -> Vec<(String, Vec<Vec<u8>>)> {
        std::mem::take(&mut self.arrived.borrow_mut())
    }
}

/// Read the server's catalog; `on_ready` gets it (not called without an agent server).
pub fn load_catalog(ctx: egui::Context, slot: Rc<RefCell<Option<(Vec<CatalogFamily>, WebFontFetcher)>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let bytes = match get_bytes("/fonts/catalog.json").await {
            Ok(b) => b,
            Err(e) => {
                log::info!("no font catalog: {e}");
                return;
            }
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            log::warn!("font catalog isn't JSON");
            return;
        };
        let families: Vec<CatalogFamily> = v
            .get("families")
            .and_then(|f| f.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let family = f.get("family")?.as_str()?.to_string();
                        let files = f.get("files")?.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>();
                        (!files.is_empty()).then_some(CatalogFamily { family, files })
                    })
                    .collect()
            })
            .unwrap_or_default();
        log::info!("font catalog: {} families on demand", families.len());
        *slot.borrow_mut() = Some((families, WebFontFetcher { arrived: Rc::default(), ctx: ctx.clone() }));
        ctx.request_repaint();
    });
}

/// Font file names have brackets and spaces (`Inter[opsz,wght].ttf`).
fn encode_path(p: &str) -> String {
    p.split('/').map(|seg| js_sys::encode_uri_component(seg).as_string().unwrap_or_default()).collect::<Vec<_>>().join("/")
}

async fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let resp = JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a response")?;
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

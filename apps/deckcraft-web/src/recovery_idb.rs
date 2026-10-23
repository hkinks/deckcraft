//! [`RecoveryStore`] over IndexedDB, through the small `window.deckcraftRecovery` helper in
//! `index.html` (put / del / list). Writes are fire-and-forget; entries from an earlier visit are
//! read once at start.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_ui_egui::autosave::{Recovered, RecoveryStore};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

pub struct IdbStore {
    earlier: Rc<RefCell<Option<Vec<Recovered>>>>,
    delivered: bool,
}

fn helper() -> Option<JsValue> {
    let w = web_sys::window()?;
    js_sys::Reflect::get(&w, &JsValue::from_str("deckcraftRecovery")).ok().filter(|v| !v.is_undefined())
}

fn call(method: &str, args: &[JsValue]) -> Option<js_sys::Promise> {
    let h = helper()?;
    let f = js_sys::Reflect::get(&h, &JsValue::from_str(method)).ok()?.dyn_into::<js_sys::Function>().ok()?;
    let arr = js_sys::Array::new();
    for a in args {
        arr.push(a);
    }
    f.apply(&h, &arr).ok()?.dyn_into::<js_sys::Promise>().ok()
}

fn field(o: &JsValue, k: &str) -> JsValue {
    js_sys::Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

impl IdbStore {
    pub fn new(ctx: egui::Context) -> Self {
        let earlier = Rc::new(RefCell::new(None));
        let slot = earlier.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut out = vec![];
            if let Some(p) = call("list", &[])
                && let Ok(list) = JsFuture::from(p).await
            {
                for v in js_sys::Array::from(&list).iter() {
                    let bytes = js_sys::Uint8Array::new(&field(&v, "bytes")).to_vec();
                    out.push(Recovered {
                        key: field(&v, "key").as_string().unwrap_or_default(),
                        title: field(&v, "title").as_string().unwrap_or_else(|| "Presentation".into()),
                        saved_at: field(&v, "savedAt").as_f64().unwrap_or(0.0),
                        bytes,
                    });
                }
            }
            *slot.borrow_mut() = Some(out);
            ctx.request_repaint();
        });
        Self { earlier, delivered: false }
    }
}

impl RecoveryStore for IdbStore {
    fn put(&mut self, e: Recovered) {
        let bytes = js_sys::Uint8Array::from(e.bytes.as_slice());
        if let Some(p) = call("put", &[JsValue::from_str(&e.key), JsValue::from_str(&e.title), JsValue::from_f64(e.saved_at), bytes.into()]) {
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(err) = JsFuture::from(p).await {
                    log::warn!("autosave write failed: {err:?}");
                }
            });
        }
    }

    fn delete(&mut self, key: &str) {
        if let Some(p) = call("del", &[JsValue::from_str(key)]) {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(p).await;
            });
        }
    }

    fn earlier(&mut self) -> Option<Vec<Recovered>> {
        if self.delivered {
            return None;
        }
        let v = self.earlier.borrow_mut().take()?;
        self.delivered = true;
        Some(v)
    }
}

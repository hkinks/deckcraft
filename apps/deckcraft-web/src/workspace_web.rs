//! [`WorkspaceHost`] over the browser's File System Access API, through the
//! `window.deckcraftWorkspace` helper in `index.html`. Chromium only: elsewhere there is no
//! host and the agent pane has no Files tab.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_ui_egui::workspace::{WorkspaceHost, WsEntry, WsStatus};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

#[derive(Default)]
struct Inner {
    status: Option<WsStatus>,
    files: Vec<WsEntry>,
    reads: Vec<(String, Result<Vec<u8>, String>)>,
}

#[derive(Clone)]
pub struct WebWorkspace {
    inner: Rc<RefCell<Inner>>,
    ctx: egui::Context,
}

fn helper() -> Option<JsValue> {
    let w = web_sys::window()?;
    js_sys::Reflect::get(&w, &JsValue::from_str("deckcraftWorkspace")).ok().filter(|v| !v.is_undefined())
}

fn field(o: &JsValue, k: &str) -> JsValue {
    js_sys::Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

async fn call(method: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let h = helper().ok_or("no workspace helper")?;
    let f = field(&h, method).dyn_into::<js_sys::Function>().map_err(|_| format!("no {method}"))?;
    let arr = js_sys::Array::new();
    for a in args {
        arr.push(a);
    }
    let p = f.apply(&h, &arr).map_err(|e| format!("{e:?}"))?.dyn_into::<js_sys::Promise>().map_err(|_| "not a promise".to_string())?;
    JsFuture::from(p).await.map_err(|e| {
        // DOMException: its message, e.g. "The user aborted a request."
        field(&e, "message").as_string().unwrap_or_else(|| format!("{e:?}"))
    })
}

impl WebWorkspace {
    /// None where the browser can't open folders.
    pub fn new(ctx: egui::Context) -> Option<Self> {
        if !helper().is_some_and(|h| field(&h, "supported").as_bool() == Some(true)) {
            return None;
        }
        let ws = Self { inner: Rc::new(RefCell::new(Inner { status: Some(WsStatus::None), ..Default::default() })), ctx };
        let me = ws.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match call("remembered", &[]).await {
                Ok(v) if !v.is_null() && !v.is_undefined() => {
                    let name = field(&v, "name").as_string().unwrap_or_default();
                    if field(&v, "granted").as_bool() == Some(true) {
                        me.list(name).await;
                    } else {
                        me.set(WsStatus::NeedsPermission { name });
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("workspace: {e}"),
            }
        });
        Some(ws)
    }

    fn set(&self, s: WsStatus) {
        self.inner.borrow_mut().status = Some(s);
        self.ctx.request_repaint();
    }

    async fn list(&self, name: String) {
        self.set(WsStatus::Loading { name: name.clone() });
        match call("list", &[]).await {
            Ok(v) => {
                let files = js_sys::Array::from(&v)
                    .iter()
                    .map(|e| WsEntry { path: field(&e, "path").as_string().unwrap_or_default(), size: field(&e, "size").as_f64().unwrap_or(0.0) as u64 })
                    .filter(|e| !e.path.is_empty())
                    .collect();
                self.inner.borrow_mut().files = files;
                self.set(WsStatus::Ready { name });
            }
            Err(e) => self.set(WsStatus::Failed(format!("Couldn't read “{name}”: {e}"))),
        }
    }

    fn name(&self) -> Option<String> {
        match self.inner.borrow().status.clone()? {
            WsStatus::Ready { name } | WsStatus::Loading { name } | WsStatus::NeedsPermission { name } => Some(name),
            _ => None,
        }
    }
}

impl WorkspaceHost for WebWorkspace {
    fn status(&self) -> WsStatus {
        self.inner.borrow().status.clone().unwrap_or(WsStatus::None)
    }

    fn pick(&mut self) {
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match call("pick", &[]).await {
                Ok(v) => me.list(v.as_string().unwrap_or_default()).await,
                // Dismissing the picker isn't an error.
                Err(e) if e.contains("abort") => {}
                Err(e) => me.set(WsStatus::Failed(e)),
            }
        });
    }

    fn reconnect(&mut self) {
        let me = self.clone();
        let name = self.name().unwrap_or_default();
        wasm_bindgen_futures::spawn_local(async move {
            match call("reconnect", &[]).await {
                Ok(v) if v.as_bool() == Some(true) => me.list(name).await,
                Ok(_) => me.set(WsStatus::NeedsPermission { name }),
                Err(e) => me.set(WsStatus::Failed(e)),
            }
        });
    }

    fn forget(&mut self) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.files.clear();
            inner.status = Some(WsStatus::None);
        }
        wasm_bindgen_futures::spawn_local(async move {
            let _ = call("forget", &[]).await;
        });
        self.ctx.request_repaint();
    }

    fn refresh(&mut self) {
        if let Some(name) = self.name() {
            let me = self.clone();
            wasm_bindgen_futures::spawn_local(async move { me.list(name).await });
        }
    }

    fn files(&self) -> Vec<WsEntry> {
        self.inner.borrow().files.clone()
    }

    fn read(&mut self, path: &str) {
        let me = self.clone();
        let path = path.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let r = call("read", &[JsValue::from_str(&path)]).await.map(|v| js_sys::Uint8Array::new(&v).to_vec());
            me.inner.borrow_mut().reads.push((path, r));
            me.ctx.request_repaint();
        });
    }

    fn poll_reads(&mut self) -> Vec<(String, Result<Vec<u8>, String>)> {
        std::mem::take(&mut self.inner.borrow_mut().reads)
    }
}

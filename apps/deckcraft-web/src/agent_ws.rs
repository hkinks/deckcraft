//! The agent pane's link to the DeckCraft agent server that served this page: `fetch` for
//! sign-in (an HttpOnly session cookie) and a WebSocket at `/ws/agent` for the conversation.
//! Served from plain static hosting (no `/api/me`), it reports [`WebAgentTransport::no_server`]
//! and the app drops the agent pane.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_ui_egui::agent::protocol::{self, ClientMsg, ServerMsg};
use deckcraft_ui_egui::agent::transport::{AgentTransport, LinkStatus};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

/// Close code the server uses for "not signed in".
const CLOSE_UNAUTHORIZED: u16 = 4401;
const RECONNECT_MS: i32 = 2000;

#[derive(Default)]
struct Inner {
    status: Option<LinkStatus>,
    inbox: Vec<ServerMsg>,
    ws: Option<web_sys::WebSocket>,
    no_server: bool,
    was_connected: bool,
    /// The socket's callbacks, dropped with it.
    callbacks: Vec<Closure<dyn FnMut(JsValue)>>,
}

#[derive(Clone)]
pub struct WebAgentTransport {
    inner: Rc<RefCell<Inner>>,
    ctx: egui::Context,
}

impl WebAgentTransport {
    pub fn new(ctx: egui::Context) -> Self {
        let t = Self { inner: Rc::new(RefCell::new(Inner { status: Some(LinkStatus::Connecting), ..Default::default() })), ctx };
        t.check_session();
        t.watch_visibility();
        t
    }

    /// The page wasn't served by an agent server: there is no agent here.
    pub fn no_server(&self) -> bool {
        self.inner.borrow().no_server
    }

    fn set(&self, s: LinkStatus) {
        self.inner.borrow_mut().status = Some(s);
        self.ctx.request_repaint();
    }

    fn check_session(&self) {
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match fetch("GET", "/api/me", None).await {
                Ok((200, _)) => me.connect(),
                Ok((401, _)) => me.set(LinkStatus::LoggedOut),
                Ok((status, _)) => {
                    log::info!("no agent server here (/api/me answered {status})");
                    me.inner.borrow_mut().no_server = true;
                    me.set(LinkStatus::Failed("No agent server at this address.".into()));
                }
                Err(e) => {
                    log::info!("no agent server here: {e}");
                    me.inner.borrow_mut().no_server = true;
                    me.set(LinkStatus::Failed("No agent server at this address.".into()));
                }
            }
        });
    }

    fn connect(&self) {
        let Some(window) = web_sys::window() else { return };
        let loc = window.location();
        let scheme = if loc.protocol().ok().as_deref() == Some("https:") { "wss" } else { "ws" };
        let url = format!("{scheme}://{}/ws/agent", loc.host().unwrap_or_default());
        let ws = match web_sys::WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(e) => {
                self.set(LinkStatus::Failed(format!("Couldn't open the agent connection: {e:?}")));
                return;
            }
        };
        self.set(LinkStatus::Connecting);
        let mut callbacks: Vec<Closure<dyn FnMut(JsValue)>> = vec![];

        let me = self.clone();
        let on_open = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            me.inner.borrow_mut().was_connected = true;
            me.set(LinkStatus::Connected);
            me.send_visibility();
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        callbacks.push(on_open);

        let me = self.clone();
        let on_message = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let Ok(e) = e.dyn_into::<web_sys::MessageEvent>() else { return };
            let Some(text) = e.data().as_string() else { return };
            match protocol::decode(&text) {
                Ok(m) => me.inner.borrow_mut().inbox.push(m),
                Err(err) => log::warn!("{err}"),
            }
            me.ctx.request_repaint();
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        callbacks.push(on_message);

        let me = self.clone();
        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let code = e.dyn_into::<web_sys::CloseEvent>().map(|c| c.code()).unwrap_or(0);
            let reconnect = {
                let mut inner = me.inner.borrow_mut();
                inner.ws = None;
                std::mem::replace(&mut inner.was_connected, false)
            };
            if code == CLOSE_UNAUTHORIZED {
                me.set(LinkStatus::LoggedOut);
            } else {
                me.set(LinkStatus::Failed("Disconnected from the agent server.".into()));
                if reconnect {
                    me.reconnect_later();
                }
            }
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        callbacks.push(on_close);

        let mut inner = self.inner.borrow_mut();
        if let Some(old) = inner.ws.take() {
            let _ = old.close();
        }
        inner.ws = Some(ws);
        inner.callbacks = callbacks;
    }

    fn send_visibility(&self) {
        let hidden = web_sys::window().and_then(|w| w.document()).is_some_and(|d| d.hidden());
        let mut me = self.clone();
        me.send(&ClientMsg::Visibility { v: protocol::VERSION, hidden });
    }

    /// Tell the server whenever the tab is hidden or shown, for as long as the page lives.
    fn watch_visibility(&self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let me = self.clone();
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |_| me.send_visibility());
        if doc.add_event_listener_with_callback("visibilitychange", cb.as_ref().unchecked_ref()).is_ok() {
            cb.forget();
        }
    }

    fn reconnect_later(&self) {
        let me = self.clone();
        let cb = Closure::once_into_js(move || me.check_session());
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), RECONNECT_MS);
        }
    }
}

impl AgentTransport for WebAgentTransport {
    fn send(&mut self, m: &ClientMsg) {
        let inner = self.inner.borrow();
        if let Some(ws) = inner.ws.as_ref().filter(|ws| ws.ready_state() == web_sys::WebSocket::OPEN)
            && let Err(e) = ws.send_with_str(&protocol::encode(m))
        {
            log::warn!("agent send failed: {e:?}");
        }
    }

    fn poll(&mut self) -> Vec<ServerMsg> {
        std::mem::take(&mut self.inner.borrow_mut().inbox)
    }

    fn status(&self) -> LinkStatus {
        self.inner.borrow().status.clone().unwrap_or(LinkStatus::LoggedOut)
    }

    fn login(&mut self, user: &str, password: &str) {
        self.set(LinkStatus::Connecting);
        let body = serde_json::json!({"username": user, "password": password}).to_string();
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match fetch("POST", "/api/login", Some(body)).await {
                Ok((200, _)) => me.connect(),
                Ok((429, _)) => me.set(LinkStatus::Failed("sign-in: too many attempts, try again in a few minutes".into())),
                Ok((401, _)) => me.set(LinkStatus::Failed("sign-in: wrong username or password".into())),
                Ok((s, _)) => me.set(LinkStatus::Failed(format!("sign-in: the server answered {s}"))),
                Err(e) => me.set(LinkStatus::Failed(format!("sign-in: {e}"))),
            }
        });
    }

    fn logout(&mut self) {
        if let Some(ws) = self.inner.borrow_mut().ws.take() {
            let _ = ws.close();
        }
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = fetch("POST", "/api/logout", None).await;
            me.set(LinkStatus::LoggedOut);
        });
    }

    fn retry(&mut self) {
        self.set(LinkStatus::Connecting);
        self.check_session();
    }
}

/// A same-origin request with the session cookie: (status, body text).
async fn fetch(method: &str, url: &str, json_body: Option<String>) -> Result<(u16, String), String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    if let Some(b) = json_body {
        let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
        headers.set("content-type", "application/json").map_err(|e| format!("{e:?}"))?;
        init.set_headers(&headers);
        init.set_body(&JsValue::from_str(&b));
    }
    let req = web_sys::Request::new_with_str_and_init(url, &init).map_err(|e| format!("{e:?}"))?;
    let resp = JsFuture::from(window.fetch_with_request(&req)).await.map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a response")?;
    let status = resp.status();
    let text = match resp.text() {
        Ok(p) => JsFuture::from(p).await.ok().and_then(|v| v.as_string()).unwrap_or_default(),
        Err(_) => String::new(),
    };
    Ok((status, text))
}

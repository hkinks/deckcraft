//! The agent chat's state and turn lifecycle.
//!
//! A turn starts with the user's prompt: the deck's current snapshot is remembered, tool calls
//! from the server run on the live session one per frame, and when the turn ends (or the link
//! drops) every undo step it made is merged into one ("AI: <prompt>"). Turn cards keep the
//! before/after snapshots so any turn can be reverted while this page lives.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use deckcraft_model::Presentation;
use serde_json::{Value, json};

use super::protocol::{ClientMsg, ContentPart, DeckContext, ProviderInfo, ServerMsg, VERSION};
use super::tools;
use super::transport::{AgentTransport, LinkStatus};
use crate::SlideApp;

#[derive(Clone, Debug, PartialEq)]
pub enum ChatItem {
    User(String),
    Assistant(String),
    Tool { name: String, ok: bool, summary: String },
    Notice(String),
    /// Index into [`AgentState::turns`].
    Turn(usize),
}

#[derive(Clone, Debug)]
pub struct TurnCard {
    pub turn_id: String,
    pub label: String,
    pub actions: Vec<String>,
    pub status: String,
    pub uid: u64,
    /// The deck before and after the turn; `None` for turns restored from history.
    pub before: Option<Arc<Presentation>>,
    pub after: Option<Arc<Presentation>>,
}

#[derive(Clone, Debug)]
enum Pending {
    Tool { id: String, name: String, input: Value },
    End { turn_id: String, actions: Vec<String>, status: String },
}

#[derive(Clone, Debug)]
pub enum TurnState {
    Idle,
    Running { uid: u64, before: Arc<Presentation>, label: String, stopping: bool },
}

#[derive(Debug)]
pub struct AgentState {
    pub open: bool,
    pub items: Vec<ChatItem>,
    pub turns: Vec<TurnCard>,
    pub turn: TurnState,
    pub providers: Vec<ProviderInfo>,
    pub provider: Option<String>,
    pub user: String,
    pub input: String,
    pub login_user: String,
    pub login_password: String,
    /// Turn card whose Revert asked for confirmation.
    pub confirm_revert: Option<usize>,
    /// Tool calls (and a turn end that arrived behind them), run one per frame in order.
    pending: VecDeque<Pending>,
    deck_keys: HashMap<u64, String>,
    /// The document the open conversation belongs to.
    conversation_uid: Option<u64>,
    ready: bool,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            open: true,
            items: vec![],
            turns: vec![],
            turn: TurnState::Idle,
            providers: vec![],
            provider: None,
            user: String::new(),
            input: String::new(),
            login_user: String::new(),
            login_password: String::new(),
            confirm_revert: None,
            pending: VecDeque::new(),
            deck_keys: HashMap::new(),
            conversation_uid: None,
            ready: false,
        }
    }
}

pub struct AgentClient {
    pub state: AgentState,
    pub transport: Box<dyn AgentTransport>,
    hello_sent: bool,
}

impl AgentClient {
    pub fn new(transport: Box<dyn AgentTransport>) -> Self {
        Self { state: AgentState::default(), transport, hello_sent: false }
    }
    pub fn vision(&self) -> bool {
        let id = self.state.provider.as_deref().unwrap_or_default();
        self.state.providers.iter().any(|p| p.id == id && p.vision)
    }
    fn send(&mut self, m: ClientMsg) {
        self.transport.send(&m);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevertOutcome {
    Undone,
    Replaced,
    NeedsConfirm,
    Unavailable,
}

pub fn busy(app: &SlideApp) -> bool {
    app.agent.as_ref().is_some_and(|a| matches!(a.state.turn, TurnState::Running { .. }))
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or_default().trim();
    let mut out: String = l.chars().take(60).collect();
    if l.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// A stable key for the active deck, so its conversation is found again: the file name for
/// opened files, a per-document id for untitled ones.
fn deck_key(app: &SlideApp, keys: &mut HashMap<u64, String>) -> Option<(u64, String)> {
    let d = app.session.active()?;
    let key = keys
        .entry(d.uid)
        .or_insert_with(|| match &d.path {
            Some(p) => format!("file:{p}"),
            None => format!("untitled:{:016x}", d.uid ^ (crate::now_ms() as u64)),
        })
        .clone();
    Some((d.uid, key))
}

fn deck_context(app: &SlideApp, key: String) -> DeckContext {
    let Some(d) = app.session.active() else { return DeckContext { deck_key: key, ..Default::default() } };
    DeckContext {
        deck_key: key,
        slides: d.doc.slides.len(),
        current: d.selection.slide,
        selection: d.selection.shapes.iter().map(|s| s.0).collect(),
        slide_size: [d.doc.slide_size.width, d.doc.slide_size.height],
    }
}

/// Close the running turn: merge its undo steps and record a turn card.
fn finish_turn(app: &mut SlideApp, client: &mut AgentClient, turn_id: String, actions: Vec<String>, status: String) {
    let TurnState::Running { uid, before, label, .. } = std::mem::replace(&mut client.state.turn, TurnState::Idle) else { return };
    client.state.pending.clear();
    let label = if status == "ok" { label } else { format!("{label} ({status})") };
    let changed = app.session.squash_undo_since(uid, &before, &label);
    let after = app.session.documents().iter().find(|d| d.uid == uid).map(|d| d.doc.clone());
    let changed = changed || after.as_ref().is_some_and(|a| !Arc::ptr_eq(a, &before));
    let card = TurnCard {
        turn_id,
        label,
        actions,
        status,
        uid,
        before: changed.then_some(before),
        after: if changed { after } else { None },
    };
    client.state.turns.push(card);
    client.state.items.push(ChatItem::Turn(client.state.turns.len() - 1));
}

fn open_conversation(app: &SlideApp, client: &mut AgentClient) {
    let Some((uid, key)) = deck_key(app, &mut client.state.deck_keys) else { return };
    client.state.conversation_uid = Some(uid);
    client.state.items.clear();
    client.state.turns.clear();
    client.send(ClientMsg::OpenConversation { v: VERSION, deck_key: key });
}

fn run_tool(app: &mut SlideApp, ctx: &egui::Context, client: &mut AgentClient, id: String, name: String, input: Value) {
    let TurnState::Running { uid, .. } = &client.state.turn else {
        client.send(ClientMsg::ToolResult { v: VERSION, id, ok: false, content: vec![ContentPart::Text { text: "no turn is running".into() }] });
        return;
    };
    let (ok, content) = if app.session.active().map(|d| d.uid) != Some(*uid) {
        (false, vec![ContentPart::Text { text: "the deck this turn was editing is no longer the active document".into() }])
    } else {
        tools::execute(app, ctx, &name, &input, client.vision())
    };
    let summary = content
        .iter()
        .find_map(|c| match c {
            ContentPart::Text { text } => Some(text.chars().take(160).collect::<String>()),
            ContentPart::Image { .. } => None,
        })
        .unwrap_or_default();
    let label = match name.as_str() {
        "run_command" => input.get("command").and_then(Value::as_str).unwrap_or("run_command").to_string(),
        _ => name,
    };
    client.state.items.push(ChatItem::Tool { name: label, ok, summary });
    client.send(ClientMsg::ToolResult { v: VERSION, id, ok, content });
}

fn handle(app: &mut SlideApp, client: &mut AgentClient, m: ServerMsg) {
    match m {
        ServerMsg::Ready { user, providers, default_provider } => {
            client.state.user = user;
            if client.state.provider.as_ref().is_none_or(|p| !providers.iter().any(|x| &x.id == p)) {
                client.state.provider = Some(default_provider);
            }
            client.state.providers = providers;
            client.state.ready = true;
            open_conversation(app, client);
        }
        ServerMsg::History { items, .. } => {
            client.state.items.clear();
            client.state.turns.clear();
            for it in items {
                if it.role == "user" {
                    client.state.items.push(ChatItem::User(it.text));
                } else {
                    if !it.text.is_empty() {
                        client.state.items.push(ChatItem::Assistant(it.text));
                    }
                    if let Some(t) = it.turn {
                        client.state.turns.push(TurnCard {
                            turn_id: t.turn_id,
                            label: String::new(),
                            actions: t.actions,
                            status: t.status,
                            uid: 0,
                            before: None,
                            after: None,
                        });
                        client.state.items.push(ChatItem::Turn(client.state.turns.len() - 1));
                    }
                }
            }
        }
        ServerMsg::Conversation { .. } => {}
        ServerMsg::AssistantDelta { text } => match client.state.items.last_mut() {
            Some(ChatItem::Assistant(s)) => s.push_str(&text),
            _ => client.state.items.push(ChatItem::Assistant(text)),
        },
        ServerMsg::ToolCall { id, name, input } => client.state.pending.push_back(Pending::Tool { id, name, input }),
        ServerMsg::TurnEnd { turn_id, actions, status, .. } => {
            if client.state.pending.is_empty() {
                finish_turn(app, client, turn_id, actions, status);
            } else {
                client.state.pending.push_back(Pending::End { turn_id, actions, status });
            }
        }
        ServerMsg::Error { code, message } => {
            client.state.items.push(ChatItem::Notice(message));
            // A refused request never started a turn on the server.
            if matches!(code.as_str(), "busy" | "rate_limited" | "bad_provider" | "no_hello" | "bad_message")
                && matches!(client.state.turn, TurnState::Running { .. })
                && client.state.pending.is_empty()
            {
                finish_turn(app, client, String::new(), vec![], "error".into());
            }
        }
    }
}

/// Once per frame: talk to the server and run at most one queued tool call.
pub fn tick(app: &mut SlideApp, ctx: &egui::Context) {
    let Some(mut client) = app.agent.take() else { return };
    let status = client.transport.status();
    if status != LinkStatus::Connected {
        client.hello_sent = false;
        client.state.ready = false;
        if matches!(client.state.turn, TurnState::Running { .. }) {
            client.state.items.push(ChatItem::Notice("Lost the connection to the agent server; the turn was interrupted.".into()));
            finish_turn(app, &mut client, String::new(), vec![], "interrupted".into());
        }
    } else if !client.hello_sent {
        client.hello_sent = true;
        client.send(ClientMsg::Hello { v: VERSION, tools: tools::advertised(), client_version: env!("CARGO_PKG_VERSION").into() });
    }
    for m in client.transport.poll() {
        handle(app, &mut client, m);
    }
    // Another deck became active: show its conversation (not while a turn runs).
    if client.state.ready
        && matches!(client.state.turn, TurnState::Idle)
        && app.session.active().map(|d| d.uid) != client.state.conversation_uid
        && app.session.active().is_some()
    {
        open_conversation(app, &mut client);
    }
    match client.state.pending.pop_front() {
        Some(Pending::Tool { id, name, input }) => run_tool(app, ctx, &mut client, id, name, input),
        Some(Pending::End { turn_id, actions, status }) => finish_turn(app, &mut client, turn_id, actions, status),
        None => {}
    }
    if matches!(client.state.turn, TurnState::Running { .. }) || status == LinkStatus::Connecting {
        ctx.request_repaint();
    }
    app.agent = Some(client);
}

pub fn send_prompt(app: &mut SlideApp, text: &str) {
    let Some(mut client) = app.agent.take() else { return };
    let text = text.trim().to_string();
    if text.is_empty() || matches!(client.state.turn, TurnState::Running { .. }) || app.session.active().is_none() {
        app.agent = Some(client);
        return;
    }
    if let Some((uid, key)) = deck_key(app, &mut client.state.deck_keys)
        && let Some(d) = app.session.active()
    {
        let before = d.doc.clone();
        let deck = deck_context(app, key);
        client.state.items.push(ChatItem::User(text.clone()));
        client.state.turn = TurnState::Running { uid, before, label: format!("AI: {}", first_line(&text)), stopping: false };
        client.state.conversation_uid = Some(uid);
        let provider = client.state.provider.clone();
        client.send(ClientMsg::UserMessage { v: VERSION, text, deck, provider });
    }
    app.agent = Some(client);
}

pub fn stop(app: &mut SlideApp) {
    let Some(client) = app.agent.as_mut() else { return };
    if let TurnState::Running { stopping, .. } = &mut client.state.turn {
        *stopping = true;
        client.transport.send(&ClientMsg::Stop { v: VERSION });
    }
}

/// Revert turn `i`: Undo when it is the latest change, otherwise (after `confirmed`) put the deck
/// back as it was before the turn, as one new undoable step.
pub fn revert(app: &mut SlideApp, i: usize, confirmed: bool) -> RevertOutcome {
    let Some(card) = app.agent.as_ref().and_then(|a| a.state.turns.get(i)).cloned() else { return RevertOutcome::Unavailable };
    let (Some(before), Some(after)) = (card.before, card.after) else { return RevertOutcome::Unavailable };
    if busy(app) {
        return RevertOutcome::Unavailable;
    }
    let Some(d) = app.session.active().filter(|d| d.uid == card.uid) else { return RevertOutcome::Unavailable };
    let latest = Arc::ptr_eq(&d.doc, &after) && d.history.undo.last().is_some_and(|e| Arc::ptr_eq(&e.doc, &before));
    if latest {
        return match app.run("edit.undo", json!({})) {
            Ok(_) => RevertOutcome::Undone,
            Err(_) => RevertOutcome::Unavailable,
        };
    }
    if !confirmed {
        if let Some(a) = app.agent.as_mut() {
            a.state.confirm_revert = Some(i);
        }
        return RevertOutcome::NeedsConfirm;
    }
    let Ok(v) = serde_json::to_value(&*before) else { return RevertOutcome::Unavailable };
    let r = app.run("doc.replace", json!({"json": v}));
    if let Some(a) = app.agent.as_mut() {
        a.state.confirm_revert = None;
    }
    match r {
        Ok(_) => {
            if let Some(st) = app.session.active_mut()
                && let Some(e) = st.history.undo.last_mut()
            {
                e.label = format!("Revert {}", card.label);
            }
            RevertOutcome::Replaced
        }
        Err(_) => RevertOutcome::Unavailable,
    }
}

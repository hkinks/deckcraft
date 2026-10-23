//! The agent chat's state and turn lifecycle.
//!
//! A turn starts with the user's prompt: the deck's current snapshot is remembered, tool calls
//! from the server run on the live session one per frame, and when the turn ends (or the link
//! drops) every undo step it made is merged into one ("AI: <prompt>"). Turn cards keep the
//! before/after snapshots so any turn can be reverted while this page lives.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use deckcraft_engine::Selection;
use deckcraft_model::{Presentation, SlideId};
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
    /// The user's request.
    pub prompt: String,
    /// Command ids of the edits (read-only lookups excluded by the server).
    pub actions: Vec<String>,
    pub status: String,
    pub uid: u64,
    /// When the turn ended (ms since the epoch); `None` for turns from an earlier page.
    pub time: Option<f64>,
    /// Slides the turn added or changed, 0-based positions in the deck after it.
    pub slides: Vec<usize>,
    pub added: usize,
    pub removed: usize,
    /// Times the user edited the deck while the agent worked (Revert then asks first).
    pub user_edits: usize,
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
    Running {
        uid: u64,
        /// The deck when the request was sent (what Revert goes back to).
        before: Arc<Presentation>,
        label: String,
        stopping: bool,
        turn_id: Option<String>,
        /// The deck as the agent's last step left it: anything else is the user's doing.
        agent_doc: Arc<Presentation>,
        /// The session journal's length after the agent's last step (later entries are the user's).
        journal_mark: usize,
        /// How many times the user changed the deck during the turn.
        user_edits: usize,
        /// Where the agent is working (its current slide and selection), apart from the person's
        /// view: it is swapped in for each of the agent's steps and out again afterwards.
        agent_view: (Selection, Option<SlideId>),
    },
}

/// What a turn changed, kept for Revert while the page lives (across deck switches and
/// reconnects, which rebuild the chat from the server's history).
#[derive(Clone, Debug)]
struct Snapshot {
    uid: u64,
    before: Arc<Presentation>,
    after: Arc<Presentation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneTab {
    #[default]
    Chat,
    Changes,
}

#[derive(Debug)]
pub struct AgentState {
    pub open: bool,
    pub tab: PaneTab,
    pub items: Vec<ChatItem>,
    pub turns: Vec<TurnCard>,
    pub turn: TurnState,
    pub providers: Vec<ProviderInfo>,
    pub provider: Option<String>,
    pub user: String,
    pub input: String,
    /// Requests typed while the agent was busy, sent one after another: (document uid, text).
    queue: VecDeque<(u64, String)>,
    /// A turn ended without finishing (stopped, error, limit): the queue waits for the person.
    queue_paused: bool,
    pub login_user: String,
    pub login_password: String,
    /// Turn card whose Revert asked for confirmation.
    pub confirm_revert: Option<usize>,
    /// How long after Stop without the server ending the turn it is ended here (ms).
    pub stop_grace_ms: f64,
    /// With a turn running and nothing heard from the server for this long, it is ended here (ms).
    pub silence_ms: f64,
    stop_at: Option<f64>,
    last_heard: f64,
    snapshots: HashMap<String, Snapshot>,
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
            tab: PaneTab::Chat,
            items: vec![],
            turns: vec![],
            turn: TurnState::Idle,
            providers: vec![],
            provider: None,
            user: String::new(),
            input: String::new(),
            queue: VecDeque::new(),
            queue_paused: false,
            login_user: String::new(),
            login_password: String::new(),
            confirm_revert: None,
            stop_grace_ms: 10_000.0,
            silence_ms: 360_000.0,
            stop_at: None,
            last_heard: 0.0,
            snapshots: HashMap::new(),
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
    Replaced,
    NeedsConfirm,
    Unavailable,
}

pub fn busy(app: &SlideApp) -> bool {
    app.agent.as_ref().is_some_and(|a| matches!(a.state.turn, TurnState::Running { .. }))
}

/// Which slides `after` added or changed relative to `before` (0-based positions in `after`),
/// how many it added, and how many of `before`'s slides are gone.
fn slide_diff(before: &Presentation, after: &Presentation) -> (Vec<usize>, usize, usize) {
    let old: HashMap<_, _> = before.slides.iter().map(|s| (s.id, s)).collect();
    let mut changed = vec![];
    let mut added = 0;
    for (i, s) in after.slides.iter().enumerate() {
        match old.get(&s.id) {
            None => {
                added += 1;
                changed.push(i);
            }
            Some(o) if !Arc::ptr_eq(o, s) => changed.push(i),
            Some(_) => {}
        }
    }
    let kept: std::collections::HashSet<_> = after.slides.iter().map(|s| s.id).collect();
    let removed = before.slides.iter().filter(|s| !kept.contains(&s.id)).count();
    (changed, added, removed)
}

/// The edits in words: each command's menu label, repeats counted ("Bold ×2"), in order.
pub fn edit_labels(actions: &[String]) -> Vec<String> {
    let mut out: Vec<(String, usize)> = vec![];
    for a in actions {
        let label = deckcraft_engine::cmd::find_command(a).map(|c| c.label.to_string()).unwrap_or_else(|| a.replace(['_', '.'], " "));
        match out.iter_mut().find(|(l, _)| *l == label) {
            Some((_, n)) => *n += 1,
            None => out.push((label, 1)),
        }
    }
    out.into_iter().map(|(l, n)| if n > 1 { format!("{l} ×{n}") } else { l }).collect()
}

fn last_user(items: &[ChatItem]) -> String {
    items.iter().rev().find_map(|i| if let ChatItem::User(s) = i { Some(s.clone()) } else { None }).unwrap_or_default()
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

/// Close the running turn and record its card. Every step stays its own undo step.
fn finish_turn(app: &mut SlideApp, client: &mut AgentClient, turn_id: String, actions: Vec<String>, status: String) {
    let TurnState::Running { uid, before, label, turn_id: started, agent_doc, journal_mark, mut user_edits, .. } =
        std::mem::replace(&mut client.state.turn, TurnState::Idle)
    else {
        return;
    };
    client.state.pending.clear();
    if !matches!(status.as_str(), "ok" | "") {
        client.state.queue_paused = true;
    }
    client.state.stop_at = None;
    if user_changes(app, uid, &agent_doc, journal_mark).is_some() {
        user_edits += 1;
    }
    let turn_id = if turn_id.is_empty() { started.unwrap_or_default() } else { turn_id };
    let label = if status == "ok" { label } else { format!("{label} ({status})") };
    let after = app.session.documents().iter().find(|d| d.uid == uid).map(|d| d.doc.clone());
    let changed = after.as_ref().is_some_and(|a| !Arc::ptr_eq(a, &before));
    let (slides, added, removed) = match &after {
        Some(a) if changed => slide_diff(&before, a),
        _ => (vec![], 0, 0),
    };
    let (before, after) = match after {
        Some(after) if changed => {
            if !turn_id.is_empty() {
                client.state.snapshots.insert(turn_id.clone(), Snapshot { uid, before: before.clone(), after: after.clone() });
            }
            (Some(before), Some(after))
        }
        _ => (None, None),
    };
    let prompt = last_user(&client.state.items);
    client.state.turns.push(TurnCard { turn_id, label, prompt, actions, status, uid, time: Some(crate::now_ms()), slides, added, removed, user_edits, before, after });
    client.state.items.push(ChatItem::Turn(client.state.turns.len() - 1));
}

/// What the user changed in document `uid` since the agent's last step left it as `agent_doc`,
/// in words for the agent ("the user changed slides 2, 4 (Bold, New Slide)"); `None` if nothing.
fn user_changes(app: &SlideApp, uid: u64, agent_doc: &Arc<Presentation>, journal_mark: usize) -> Option<String> {
    let d = app.session.documents().iter().find(|d| d.uid == uid)?;
    if Arc::ptr_eq(&d.doc, agent_doc) {
        return None;
    }
    let (slides, added, removed) = slide_diff(agent_doc, &d.doc);
    let ids: Vec<String> = app.session.journal.iter().skip(journal_mark.min(app.session.journal.len())).map(|(id, _)| id.clone()).collect();
    let what = edit_labels(&ids);
    let mut text = String::from("Note: while you were working, the user edited the deck themselves");
    if !slides.is_empty() {
        let list: Vec<String> = slides.iter().take(20).map(|i| (i + 1).to_string()).collect();
        text.push_str(&format!(" (slide{} {})", if slides.len() == 1 { "" } else { "s" }, list.join(", ")));
    }
    if added > 0 || removed > 0 {
        text.push_str(&format!(", adding {added} and removing {removed} slide(s)"));
    }
    if !what.is_empty() {
        text.push_str(&format!(": {}", what.join(", ")));
    }
    text.push_str(". Their edits stand; inspect before changing those slides and don't undo them.");
    Some(text)
}

fn open_conversation(app: &SlideApp, client: &mut AgentClient) {
    let Some((uid, key)) = deck_key(app, &mut client.state.deck_keys) else { return };
    client.state.conversation_uid = Some(uid);
    client.state.items.clear();
    client.send(ClientMsg::OpenConversation { v: VERSION, deck_key: key });
}

fn run_tool(app: &mut SlideApp, ctx: &egui::Context, client: &mut AgentClient, id: String, name: String, input: Value) {
    let TurnState::Running { uid, .. } = &client.state.turn else {
        client.send(ClientMsg::ToolResult { v: VERSION, id, ok: false, content: vec![ContentPart::Text { text: "no turn is running".into() }] });
        return;
    };
    let uid = *uid;
    let (ok, mut content, note) = if app.session.active().map(|d| d.uid) != Some(uid) {
        (false, vec![ContentPart::Text { text: "the deck this turn was editing is no longer the active document".into() }], None)
    } else {
        // Whatever the user did since the agent's last step is theirs: tell the agent.
        let note = match &client.state.turn {
            TurnState::Running { agent_doc, journal_mark, .. } => user_changes(app, uid, agent_doc, *journal_mark),
            TurnState::Idle => None,
        };
        let pre = app.session.active().map(|d| d.doc.clone());
        // The agent works from its own place in the deck; the person's view stays where it is.
        let agent_view = match &client.state.turn {
            TurnState::Running { agent_view, .. } => agent_view.clone(),
            TurnState::Idle => Default::default(),
        };
        let mine = app.session.active_mut().map(|d| {
            let mine = (d.selection.clone(), d.current_slide_id());
            d.put_selection(agent_view.0, agent_view.1);
            mine
        });
        let (ok, content) = tools::execute_on(app, ctx, Some(uid), &name, &input, client.vision());
        if let Some((sel, on)) = mine
            && let Some(d) = app.session.active_mut()
            && d.uid == uid
        {
            let agent_view = (d.selection.clone(), d.current_slide_id());
            d.put_selection(sel, on);
            if let TurnState::Running { agent_view: v, .. } = &mut client.state.turn {
                *v = agent_view;
            }
        }
        // Each of the agent's steps is its own undo step, marked as the agent's.
        if let Some(pre) = &pre {
            app.session.label_undo_since(uid, pre, "AI: ");
        }
        (ok, content, note)
    };
    if let TurnState::Running { agent_doc, journal_mark, user_edits, .. } = &mut client.state.turn
        && let Some(d) = app.session.documents().iter().find(|d| d.uid == uid)
    {
        *agent_doc = d.doc.clone();
        *journal_mark = app.session.journal.len();
        if note.is_some() {
            *user_edits += 1;
        }
    }
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
    if let Some(n) = note {
        client.state.items.push(ChatItem::Notice("You edited the deck; the agent was told.".into()));
        content.push(ContentPart::Text { text: n });
    }
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
            let uid = client.state.conversation_uid.unwrap_or(0);
            for it in items {
                if it.role == "user" {
                    client.state.items.push(ChatItem::User(it.text));
                } else {
                    if !it.text.is_empty() {
                        client.state.items.push(ChatItem::Assistant(it.text));
                    }
                    if let Some(t) = it.turn {
                        // A turn already in the change log (made since this page opened) keeps
                        // its card, snapshots and all.
                        let i = match client.state.turns.iter().position(|c| !t.turn_id.is_empty() && c.turn_id == t.turn_id) {
                            Some(i) => i,
                            None => {
                                let prompt = last_user(&client.state.items);
                                let snap = client.state.snapshots.get(&t.turn_id).cloned();
                                client.state.turns.push(TurnCard {
                                    label: format!("AI: {}", first_line(&prompt)),
                                    prompt,
                                    turn_id: t.turn_id,
                                    actions: t.actions,
                                    status: t.status,
                                    uid: snap.as_ref().map_or(uid, |s| s.uid),
                                    time: None,
                                    slides: t.slides,
                                    added: 0,
                                    removed: 0,
                                    user_edits: 0,
                                    before: snap.as_ref().map(|s| s.before.clone()),
                                    after: snap.map(|s| s.after),
                                });
                                client.state.turns.len() - 1
                            }
                        };
                        client.state.items.push(ChatItem::Turn(i));
                    }
                }
            }
        }
        ServerMsg::Conversation { .. } => {}
        ServerMsg::TurnStart { turn_id } => {
            if let TurnState::Running { turn_id: id, .. } = &mut client.state.turn {
                *id = Some(turn_id);
            }
        }
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
        client.state.last_heard = crate::now_ms();
        handle(app, &mut client, m);
    }
    // The server should end every turn; if it doesn't (hung model call, half-open socket), don't
    // keep the editor locked.
    if matches!(client.state.turn, TurnState::Running { .. }) {
        let now = crate::now_ms();
        let stop_overdue = client.state.stop_at.is_some_and(|t| now - t >= client.state.stop_grace_ms);
        let silent = now - client.state.last_heard >= client.state.silence_ms;
        if stop_overdue || silent {
            let why = if stop_overdue { "The agent didn't stop in time; the turn was ended here." } else { "No answer from the agent server; the turn was ended here." };
            client.state.items.push(ChatItem::Notice(why.into()));
            finish_turn(app, &mut client, String::new(), vec![], "interrupted".into());
        }
    }
    // The person opened or switched to another deck mid-turn: the agent stops (its steps so far
    // stay in the deck it was editing, each its own undo step) and the pane follows them.
    if let TurnState::Running { uid, stopping: stopping @ false, .. } = &mut client.state.turn
        && app.session.active().map(|d| d.uid) != Some(*uid)
    {
        *stopping = true;
        client.state.stop_at = Some(crate::now_ms());
        client.transport.send(&ClientMsg::Stop { v: VERSION });
        let title = app.session.documents().iter().find(|d| d.uid == *uid).map(|d| d.title.clone()).unwrap_or_default();
        let note = if title.is_empty() {
            "Stopped: you switched to another presentation. What the agent did so far stays in the one it was editing.".to_string()
        } else {
            format!("Stopped: you switched to another presentation. What the agent did so far stays in “{title}”.")
        };
        client.state.items.push(ChatItem::Notice(note));
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
    let next = (client.state.ready
        && status == LinkStatus::Connected
        && !client.state.queue_paused
        && matches!(client.state.turn, TurnState::Idle)
        && client.state.pending.is_empty())
    .then(|| app.session.active().map(|d| d.uid))
    .flatten()
    .filter(|uid| client.state.queue.front().is_some_and(|(u, _)| u == uid))
    .and_then(|_| client.state.queue.pop_front());
    app.agent = Some(client);
    if let Some((_, text)) = next {
        send_prompt(app, &text);
    }
}

/// Send `text` now, or after the requests ahead of it when the agent is busy.
pub fn queue_prompt(app: &mut SlideApp, text: &str) {
    let text = text.trim();
    let Some(uid) = app.session.active().map(|d| d.uid) else { return };
    let Some(client) = app.agent.as_mut() else { return };
    if text.is_empty() {
        return;
    }
    if matches!(client.state.turn, TurnState::Idle) && client.state.queue.is_empty() {
        client.state.queue_paused = false;
        send_prompt(app, text);
    } else {
        client.state.queue.push_back((uid, text.to_string()));
    }
}

/// The requests waiting to be sent, in order.
pub fn queued(app: &SlideApp) -> Vec<String> {
    app.agent.as_ref().map(|a| a.state.queue.iter().map(|(_, t)| t.clone()).collect()).unwrap_or_default()
}

pub fn remove_queued(app: &mut SlideApp, i: usize) {
    if let Some(a) = app.agent.as_mut() {
        a.state.queue.remove(i);
        if a.state.queue.is_empty() {
            a.state.queue_paused = false;
        }
    }
}

pub fn queue_paused(app: &SlideApp) -> bool {
    app.agent.as_ref().is_some_and(|a| a.state.queue_paused && !a.state.queue.is_empty())
}

pub fn resume_queue(app: &mut SlideApp) {
    if let Some(a) = app.agent.as_mut() {
        a.state.queue_paused = false;
    }
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
        client.state.turn = TurnState::Running {
            uid,
            agent_doc: before.clone(),
            before,
            label: format!("AI: {}", first_line(&text)),
            stopping: false,
            turn_id: None,
            journal_mark: app.session.journal.len(),
            user_edits: 0,
            agent_view: (d.selection.clone(), d.current_slide_id()),
        };
        client.state.last_heard = crate::now_ms();
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
        client.state.stop_at = Some(crate::now_ms());
        client.transport.send(&ClientMsg::Stop { v: VERSION });
    }
}

/// Start a fresh conversation about this deck (the server keeps the earlier one).
pub fn new_conversation(app: &mut SlideApp) {
    let Some(client) = app.agent.as_mut() else { return };
    if matches!(client.state.turn, TurnState::Running { .. }) {
        return;
    }
    // The chat starts over; the change log (and Revert) stays.
    client.state.items.clear();
    client.state.confirm_revert = None;
    client.transport.send(&ClientMsg::NewConversation { v: VERSION });
}

/// Revert turn `i`: put the deck back as it was before the request, as one undoable step. When
/// the user edited during the turn, or the deck changed since, that would discard their work too,
/// so it asks first (`confirmed`).
pub fn revert(app: &mut SlideApp, i: usize, confirmed: bool) -> RevertOutcome {
    let Some(card) = app.agent.as_ref().and_then(|a| a.state.turns.get(i)).cloned() else { return RevertOutcome::Unavailable };
    let (Some(before), Some(after)) = (card.before, card.after) else { return RevertOutcome::Unavailable };
    if busy(app) {
        return RevertOutcome::Unavailable;
    }
    let Some(d) = app.session.active().filter(|d| d.uid == card.uid) else { return RevertOutcome::Unavailable };
    let clean = Arc::ptr_eq(&d.doc, &after) && card.user_edits == 0;
    if !clean && !confirmed {
        if let Some(a) = app.agent.as_mut() {
            a.state.confirm_revert = Some(i);
        }
        return RevertOutcome::NeedsConfirm;
    }
    // The snapshot itself goes back in (one undoable step), so pictures keep their bytes.
    let replaced = app.session.replace_doc(card.uid, before, &format!("Revert {}", card.label));
    if let Some(a) = app.agent.as_mut() {
        a.state.confirm_revert = None;
    }
    if replaced { RevertOutcome::Replaced } else { RevertOutcome::Unavailable }
}

/// What else changed in the deck after turn `i` (and so goes too when it is reverted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaterChanges {
    /// Steps the person took, during the turn or after it.
    pub your_edits: usize,
    /// The agent's later requests.
    pub requests: usize,
}

impl LaterChanges {
    pub fn is_empty(&self) -> bool {
        self.your_edits == 0 && self.requests == 0
    }

    /// "2 edits you made and 1 later agent request".
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if self.your_edits > 0 {
            parts.push(format!("{} edit{} you made", self.your_edits, if self.your_edits == 1 { "" } else { "s" }));
        }
        if self.requests > 0 {
            parts.push(format!("{} later agent request{}", self.requests, if self.requests == 1 { "" } else { "s" }));
        }
        if parts.is_empty() { "other changes".into() } else { parts.join(" and ") }
    }
}

/// The changes that reverting turn `i` would also take away, read from the undo history: steps
/// marked "AI: " are the agent's, the rest the person's. None when the history no longer reaches
/// back to the turn.
pub fn later_changes(app: &SlideApp, i: usize) -> Option<LaterChanges> {
    let a = app.agent.as_ref()?;
    let card = a.state.turns.get(i)?;
    let after = card.after.as_ref()?;
    let d = app.session.active().filter(|d| d.uid == card.uid)?;
    let undo = &d.history.undo;
    // The step after the turn is the one whose "before" is the deck as the turn left it.
    let from = if Arc::ptr_eq(&d.doc, after) { undo.len() } else { undo.iter().rposition(|e| Arc::ptr_eq(&e.doc, after))? };
    let your_edits = card.user_edits + undo[from..].iter().filter(|e| !e.label.starts_with("AI: ") && !e.label.starts_with("Revert ") && !e.label.starts_with("Restore ")).count();
    let requests = a.state.turns.iter().skip(i + 1).filter(|c| c.uid == card.uid && c.after.is_some()).filter(|c| {
        c.after.as_ref().is_some_and(|x| Arc::ptr_eq(x, &d.doc) || undo[from..].iter().any(|e| Arc::ptr_eq(&e.doc, x)))
    }).count();
    Some(LaterChanges { your_edits, requests })
}

/// Turn `i`'s deck is back to how it was before the turn (it was reverted, or undone).
pub fn is_reverted(app: &SlideApp, i: usize) -> bool {
    let Some(card) = app.agent.as_ref().and_then(|a| a.state.turns.get(i)) else { return false };
    let Some(before) = &card.before else { return false };
    app.session.active().filter(|d| d.uid == card.uid).is_some_and(|d| Arc::ptr_eq(&d.doc, before))
}

/// Bring a reverted turn back: Redo when that is the next step, otherwise put the deck after the
/// turn back as one undoable step. False when there is nothing to restore.
pub fn restore(app: &mut SlideApp, i: usize) -> bool {
    if busy(app) || !is_reverted(app, i) {
        return false;
    }
    let Some(card) = app.agent.as_ref().and_then(|a| a.state.turns.get(i)).cloned() else { return false };
    let Some(after) = card.after else { return false };
    let redo_next = app.session.active().is_some_and(|d| d.history.redo.last().is_some_and(|e| Arc::ptr_eq(&e.doc, &after)));
    if redo_next {
        return app.run("edit.redo", json!({})).is_ok();
    }
    app.session.replace_doc(card.uid, after, &format!("Restore {}", card.label))
}

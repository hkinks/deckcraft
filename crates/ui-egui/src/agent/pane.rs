//! The agent pane (right side, collapsible).
//!
//! Layout: header (title, model, New chat, close) · underline tabs Chat | Changes ·
//! a status strip with Stop while the agent works · the tab's content · the prompt box with an
//! inline send button. Chat shows your messages as bubbles and the agent's replies with an accent
//! bar; the model's running commentary between steps is dimmed and its steps fold into one row,
//! followed by a one-line result with slide chips and Revert. Changes is the deck's change log.
//! While the agent works a light veil keeps editing input off the deck.

use egui::text::LayoutJob;
use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, TextFormat, Ui, vec2};

use super::client::{self, ChatItem, PaneTab, RevertOutcome, TurnCard, TurnState};
use super::transport::LinkStatus;
use crate::SlideApp;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens};

pub const WIDTH: f32 = 340.0;
const PAD: i8 = 12;
const TITLE: f32 = 15.0;
const BODY: f32 = 13.0;
const META: f32 = 11.0;

/// Suggestions in an empty chat; a click puts one in the prompt box.
const SUGGESTIONS: &[&str] = &["Add an agenda slide", "Make all titles consistent", "Turn slide 2 into a table", "Tighten the wording on this slide"];

/// Status colours that stay readable on both themes.
fn ok_color(t: &Tokens) -> Color32 {
    if t.dark { Color32::from_rgb(0x6F, 0xD0, 0x8C) } else { Color32::from_rgb(0x1E, 0x7B, 0x34) }
}
fn warn_color(t: &Tokens) -> Color32 {
    if t.dark { Color32::from_rgb(0xF2, 0xB2, 0x4C) } else { Color32::from_rgb(0xA8, 0x60, 0x00) }
}
fn err_color(t: &Tokens) -> Color32 {
    if t.dark { Color32::from_rgb(0xFF, 0x8A, 0x80) } else { Color32::from_rgb(0xC4, 0x2B, 0x1C) }
}

pub fn is_open(app: &SlideApp) -> bool {
    app.agent.as_ref().is_some_and(|a| a.state.open)
}

pub fn toggle(app: &mut SlideApp) {
    if let Some(a) = app.agent.as_mut() {
        a.state.open = !a.state.open;
    }
}

pub fn show(app: &mut SlideApp, ui: &mut Ui) {
    if !is_open(app) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    // The slide keeps most of a narrow window: the pane takes at most 45% of it.
    let max = (ui.ctx().content_rect().width() * 0.45).clamp(200.0, 640.0);
    let min = 240.0_f32.min(max);
    egui::Panel::right("agent_pane")
        .resizable(true)
        .default_size(WIDTH.min(max))
        .size_range(min..=max)
        .frame(egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin::same(PAD)))
        .show(ui, |ui| {
            // A hairline between the pane and the editor.
            let r = ui.max_rect().expand(f32::from(PAD));
            ui.painter().with_clip_rect(ui.ctx().content_rect()).vline(r.left(), r.y_range(), Stroke::new(1.0, t.border));
            ui.spacing_mut().item_spacing.y = 6.0;
            header(app, ui, &t);
            let Some(status) = app.agent.as_ref().map(|a| a.transport.status()) else { return };
            match status {
                LinkStatus::LoggedOut => sign_in(app, ui, None),
                LinkStatus::Failed(msg) if msg.starts_with("sign-in:") => sign_in(app, ui, Some(msg.trim_start_matches("sign-in:").trim().to_string())),
                LinkStatus::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to the agent server…");
                    });
                }
                LinkStatus::Failed(msg) => {
                    ui.label(RichText::new(msg).color(t.text_dim));
                    if ui.button("Retry").clicked()
                        && let Some(a) = app.agent.as_mut()
                    {
                        a.transport.retry();
                    }
                }
                LinkStatus::Connected => {
                    tabs(app, ui, &t);
                    working_strip(app, ui, &t);
                    match app.agent.as_ref().map_or(PaneTab::Chat, |a| a.state.tab) {
                        PaneTab::Chat => chat(app, ui, &t),
                        PaneTab::Changes => changes(app, ui, &t),
                    }
                }
            }
        });
}

fn icon_button(ui: &mut Ui, t: &Tokens, icon: Icon, name: &str, tip: &str, enabled: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(4), t.hover);
    }
    icons::paint(ui.painter(), r.shrink(4.0), icon, if enabled { t.text_dim } else { t.text_faint }, false);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    enabled && resp.on_hover_text(tip).clicked()
}

fn provider_name(p: &super::protocol::ProviderInfo) -> String {
    if p.label.is_empty() { p.id.clone() } else { p.label.clone() }
}

fn header(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let busy = client::busy(app);
    let connected = app.agent.as_ref().is_some_and(|a| a.transport.status() == LinkStatus::Connected);
    let mut close = false;
    let mut new_chat = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("AI Agent").font(theme::bold(TITLE)).color(t.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // While a turn runs the pane stays open: Stop lives here.
            close = icon_button(ui, t, Icon::Close, "Close the agent pane", "Close the agent pane", !busy);
            if connected {
                let r = ui.add_enabled(!busy, egui::Button::new(RichText::new("New chat").font(theme::font(12.0))).frame(false));
                new_chat = r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Start a new conversation about this deck (the change log stays)").clicked();
            }
            let Some(a) = app.agent.as_mut() else { return };
            let current = a.state.provider.clone().unwrap_or_default();
            let name = a.state.providers.iter().find(|p| p.id == current).map(provider_name).unwrap_or(current.clone());
            if a.state.providers.len() > 1 {
                egui::ComboBox::from_id_salt("agent_provider").selected_text(RichText::new(name).font(theme::font(12.0))).show_ui(ui, |ui| {
                    for p in a.state.providers.clone() {
                        let vision = if p.vision { " · sees slides" } else { "" };
                        if ui.selectable_label(current == p.id, format!("{}{vision}", provider_name(&p))).on_hover_text(&p.model).clicked() {
                            a.state.provider = Some(p.id.clone());
                        }
                    }
                });
            } else if !name.is_empty() {
                ui.label(RichText::new(name).color(t.text_dim).font(theme::font(12.0)));
            }
        });
    });
    if close {
        toggle(app);
    }
    if new_chat {
        client::new_conversation(app);
        if let Some(a) = app.agent.as_mut() {
            a.state.tab = PaneTab::Chat;
        }
    }
}

/// The deck's turns (its change log), oldest first, with their indices.
fn deck_turns(app: &SlideApp) -> Vec<(usize, TurnCard)> {
    let uid = app.session.active().map(|d| d.uid);
    app.agent
        .as_ref()
        .map(|a| a.state.turns.iter().cloned().enumerate().filter(|(_, c)| Some(c.uid) == uid || c.uid == 0).collect())
        .unwrap_or_default()
}

/// Underline tabs: Chat | Changes (n).
fn tabs(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let n = deck_turns(app).len();
    let Some(a) = app.agent.as_mut() else { return };
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (tab, label, tip) in [
            (PaneTab::Chat, "Chat".to_string(), "The conversation"),
            (PaneTab::Changes, format!("Changes ({n})"), "Everything the agent changed in this deck, with Revert"),
        ] {
            let on = a.state.tab == tab;
            let text = RichText::new(label).font(if on { theme::bold(BODY) } else { theme::font(BODY) }).color(if on { t.text } else { t.text_dim });
            let r = ui.add(egui::Label::new(text).selectable(false).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
            if on {
                ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 4.0, Stroke::new(2.0, t.accent));
            }
            if r.clicked() {
                a.state.tab = tab;
            }
        }
    });
    let y = row.response.rect.bottom() + 5.0;
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, t.border));
    ui.add_space(6.0);
}

/// While the agent works: spinner, what it's doing and Stop, at the top of the pane.
fn working_strip(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    if !client::busy(app) {
        return;
    }
    let steps = app.agent.as_ref().map_or(0, |a| a.state.items.iter().rev().take_while(|i| !matches!(i, ChatItem::User(_))).filter(|i| matches!(i, ChatItem::Tool { .. })).count());
    let stopping = app.agent.as_ref().is_some_and(|a| matches!(a.state.turn, TurnState::Running { stopping: true, .. }));
    let mut stop = false;
    egui::Frame::NONE.fill(t.selected).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Agent is editing the deck…").font(theme::font(BODY)).color(t.text));
            if steps > 0 {
                ui.label(RichText::new(format!("{steps} step{}", if steps == 1 { "" } else { "s" })).font(theme::font(META)).color(t.text_dim));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if stopping {
                    ui.add_enabled(false, egui::Button::new("Stopping…"));
                } else {
                    stop = ui.add(egui::Button::new("Stop").stroke(Stroke::new(1.0, t.border))).on_hover_text("Stop after the current step").clicked();
                }
            });
        });
    });
    if stop {
        client::stop(app);
    }
}

/// `**bold**` runs of one line: (text, bold). Other markdown stays as written.
pub fn md_segments(line: &str) -> Vec<(String, bool)> {
    if line.matches("**").count() % 2 == 1 {
        return vec![(line.to_string(), false)];
    }
    let mut out = vec![];
    for (i, part) in line.split("**").enumerate() {
        if !part.is_empty() {
            out.push((part.to_string(), i % 2 == 1));
        }
    }
    out
}

/// One line's block markup: bullets become •, headings lose their #s and are bold.
pub fn md_line(line: &str) -> (String, bool) {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
        return (format!("{indent}• {rest}"), false);
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        return (trimmed[hashes + 1..].to_string(), true);
    }
    (line.to_string(), false)
}

fn markdown(ui: &mut Ui, text: &str, color: Color32, size: f32) {
    let mut job = LayoutJob::default();
    for (n, raw) in text.lines().enumerate() {
        if n > 0 {
            job.append("\n", 0.0, TextFormat { font_id: theme::font(size), color, ..Default::default() });
        }
        let (line, heading) = md_line(raw);
        for (seg, bold) in md_segments(&line) {
            let font = if bold || heading { theme::bold(size) } else { theme::font(size) };
            job.append(&seg, 0.0, TextFormat { font_id: font, color, ..Default::default() });
        }
    }
    job.wrap.max_width = ui.available_width();
    ui.label(job);
}

fn ago(ms: Option<f64>) -> String {
    let Some(ms) = ms else { return "earlier".into() };
    let s = ((crate::now_ms() - ms) / 1000.0).max(0.0);
    if s < 60.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{} min ago", (s / 60.0) as u32)
    } else {
        format!("{} h ago", (s / 3600.0) as u32)
    }
}

/// A small coloured pill: ✓ done, ■ stopped, ↩ reverted…
fn status_pill(ui: &mut Ui, status: &str, reverted: bool, t: &Tokens) {
    let (text, color) = if reverted {
        ("↩ reverted", t.text_dim)
    } else {
        match status {
            "ok" | "" => ("✓ done", ok_color(t)),
            "stopped" => ("■ stopped", warn_color(t)),
            "interrupted" => ("⚠ interrupted", warn_color(t)),
            "limit" => ("⚠ limit reached", warn_color(t)),
            _ => ("✗ error", err_color(t)),
        }
    };
    egui::Frame::NONE.fill(color.gamma_multiply(0.15)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).color(color).font(theme::font(META)));
    });
}

fn changes_summary(c: &TurnCard) -> String {
    let n = c.actions.len();
    if n == 0 {
        return "No changes".into();
    }
    format!("{n} change{}", if n == 1 { "" } else { "s" })
}

/// What a click on a turn's buttons asks for, applied after drawing.
enum TurnAction {
    Revert(usize, bool),
    Restore(usize),
    CancelRevert,
    Details,
    GoTo(usize),
}

/// Revert (or Restore once reverted) as a real button.
fn revert_button(ui: &mut Ui, t: &Tokens, i: usize, c: &TurnCard, running: bool, reverted: bool, out: &mut Option<TurnAction>) {
    let (name, tip, can, action) = if reverted {
        ("Restore", "Bring this request's changes back (one undoable step)", !running, TurnAction::Restore(i))
    } else {
        let tip = if c.before.is_none() { "Revert is available for requests made since this page was opened" } else { "Put the deck back as it was before this request (one undoable step)" };
        ("Revert", tip, c.before.is_some() && !running, TurnAction::Revert(i, false))
    };
    let glyph = if reverted { "↻" } else { "↺" };
    let r = ui.add_enabled(can, egui::Button::new(RichText::new(format!("{glyph} {name}")).font(theme::font(12.0))).stroke(Stroke::new(1.0, t.border)));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, can, name));
    if r.on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
        *out = Some(action);
    }
}

fn slide_chips(ui: &mut Ui, c: &TurnCard, max: usize, out: &mut Option<TurnAction>) {
    for s in c.slides.iter().take(max) {
        if ui.small_button(format!("Slide {}", s + 1)).on_hover_text("Go to this slide").clicked() {
            *out = Some(TurnAction::GoTo(*s));
        }
    }
    if c.slides.len() > max {
        ui.label(RichText::new(format!("+{}", c.slides.len() - max)).font(theme::font(META)));
    }
}

fn confirm_row(ui: &mut Ui, i: usize, t: &Tokens, out: &mut Option<TurnAction>) {
    ui.label(RichText::new("Changes made after this request will be discarded too.").color(t.text_dim).font(theme::font(12.0)));
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new("Discard later changes and revert").stroke(Stroke::new(1.0, err_color(t)))).clicked() {
            *out = Some(TurnAction::Revert(i, true));
        }
        if ui.button("Cancel").clicked() {
            *out = Some(TurnAction::CancelRevert);
        }
    });
}

fn apply(app: &mut SlideApp, action: Option<TurnAction>) {
    match action {
        Some(TurnAction::CancelRevert) => {
            if let Some(a) = app.agent.as_mut() {
                a.state.confirm_revert = None;
            }
        }
        Some(TurnAction::Revert(i, confirmed)) => {
            if client::revert(app, i, confirmed) == RevertOutcome::Unavailable
                && let Some(a) = app.agent.as_mut()
            {
                a.state.items.push(ChatItem::Notice("That request can't be reverted here.".into()));
            }
        }
        Some(TurnAction::Restore(i)) => {
            if !client::restore(app, i)
                && let Some(a) = app.agent.as_mut()
            {
                a.state.items.push(ChatItem::Notice("That request can't be restored here.".into()));
            }
        }
        Some(TurnAction::Details) => {
            if let Some(a) = app.agent.as_mut() {
                a.state.tab = PaneTab::Changes;
            }
        }
        Some(TurnAction::GoTo(i)) => {
            let _ = app.run("slide.go", serde_json::json!({"index": i}));
        }
        None => {}
    }
}

/// A text field with a visible box (the app's theme draws bare `TextEdit`s without one).
fn field<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE
        .fill(t.field_bg)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, add)
        .inner
}

fn sign_in(app: &mut SlideApp, ui: &mut Ui, error: Option<String>) {
    let Some(a) = app.agent.as_mut() else { return };
    ui.label("Sign in to the DeckCraft agent server.");
    ui.add_space(6.0);
    ui.label("Username");
    field(ui, |ui| ui.add(egui::TextEdit::singleline(&mut a.state.login_user).frame(egui::Frame::NONE).desired_width(f32::INFINITY)));
    ui.label("Password");
    let pw = field(ui, |ui| {
        ui.add(egui::TextEdit::singleline(&mut a.state.login_password).password(true).frame(egui::Frame::NONE).desired_width(f32::INFINITY))
    });
    if let Some(e) = error {
        ui.colored_label(egui::Color32::from_rgb(0xC4, 0x2B, 0x1C), e);
    }
    ui.add_space(6.0);
    let enter = pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if ui.button("Sign in").clicked() || enter {
        let (u, p) = (a.state.login_user.trim().to_string(), std::mem::take(&mut a.state.login_password));
        if !u.is_empty() && !p.is_empty() {
            a.transport.login(&u, &p);
        }
    }
}


/// The agent's text: an accent bar on the left; `header` on its first reply of a turn.
fn agent_reply(ui: &mut Ui, t: &Tokens, text: &str, header: bool, dim: bool) {
    let r = egui::Frame::NONE.inner_margin(egui::Margin { left: 10, right: 0, top: 0, bottom: 0 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if header {
            ui.label(RichText::new("Agent").font(theme::bold(META)).color(t.accent));
        }
        if dim {
            markdown(ui, text, t.text_dim, 12.0);
        } else {
            markdown(ui, text, t.text, BODY);
        }
    });
    let rect = r.response.rect;
    ui.painter().vline(rect.left() + 2.0, rect.y_range(), Stroke::new(2.0, if dim { t.border } else { t.accent.gamma_multiply(0.6) }));
}

fn chat(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let running = client::busy(app);
    let input_h = 104.0;
    let scroll_h = (ui.available_height() - input_h).max(80.0);
    let mut action = None;
    let mut suggestion = None;
    egui::ScrollArea::vertical().id_salt("agent_chat").max_height(scroll_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        let Some(a) = app.agent.as_ref() else { return };
        let items = &a.state.items;
        if items.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("What should I change?").font(theme::bold(BODY)).color(t.text));
            ui.label(RichText::new("Describe it in your own words. Each request is one undo step, and the Changes tab lists them all with Revert.").font(theme::font(12.0)).color(t.text_dim));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for s in SUGGESTIONS {
                    if ui.add(egui::Button::new(RichText::new(*s).font(theme::font(12.0))).stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(10))).clicked() {
                        suggestion = Some(s.to_string());
                    }
                }
            });
        }
        let mut k = 0;
        let mut replied = false;
        while k < items.len() {
            match &items[k] {
                ChatItem::Tool { .. } => {
                    // Consecutive steps fold into one row.
                    let start = k;
                    while k < items.len() && matches!(items[k], ChatItem::Tool { .. }) {
                        k += 1;
                    }
                    let steps = &items[start..k];
                    let failed = steps.iter().filter(|s| matches!(s, ChatItem::Tool { ok: false, .. })).count();
                    let title = if failed > 0 { format!("{} steps ({failed} failed)", steps.len()) } else { format!("{} steps", steps.len()) };
                    egui::CollapsingHeader::new(RichText::new(title).font(theme::font(META)).color(t.text_dim)).id_salt(("steps", start)).default_open(false).show(ui, |ui| {
                        for s in steps {
                            if let ChatItem::Tool { name, ok, summary } = s {
                                let mark = if *ok { "✓" } else { "✗" };
                                ui.label(RichText::new(format!("{mark} {name}")).font(theme::font(META)).color(if *ok { t.text_dim } else { err_color(t) })).on_hover_text(summary);
                            }
                        }
                    });
                    continue;
                }
                ChatItem::User(s) => {
                    replied = false;
                    ui.add_space(4.0);
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        egui::Frame::NONE.fill(t.selected).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(10, 7)).show(ui, |ui| {
                            ui.set_max_width(ui.available_width() * 0.85);
                            ui.label(RichText::new(s).font(theme::font(BODY)).color(t.text));
                        });
                    });
                }
                ChatItem::Assistant(s) => {
                    // Text followed by more steps is the model thinking aloud: dimmed.
                    let narration = matches!(items.get(k + 1), Some(ChatItem::Tool { .. }));
                    agent_reply(ui, t, s, !replied, narration);
                    replied = true;
                }
                ChatItem::Notice(s) => {
                    ui.label(RichText::new(s).italics().font(theme::font(12.0)).color(t.text_dim));
                }
                ChatItem::Turn(i) => {
                    if let Some(c) = a.state.turns.get(*i) {
                        let reverted = client::is_reverted(app, *i);
                        ui.horizontal_wrapped(|ui| {
                            status_pill(ui, &c.status, reverted, t);
                            ui.label(RichText::new(changes_summary(c)).font(theme::font(12.0)).color(t.text_dim));
                            slide_chips(ui, c, 3, &mut action);
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(RichText::new("Details").font(theme::font(12.0)).color(t.text_dim)).frame(false))
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Open this request in Changes")
                                .clicked()
                            {
                                action = Some(TurnAction::Details);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| revert_button(ui, t, *i, c, running, reverted, &mut action));
                        });
                        if a.state.confirm_revert == Some(*i) {
                            confirm_row(ui, *i, t, &mut action);
                        }
                    }
                }
            }
            ui.add_space(4.0);
            k += 1;
        }
    });
    apply(app, action);
    let Some(a) = app.agent.as_mut() else { return };
    if let Some(s) = suggestion {
        a.state.input = s;
    }
    ui.add_space(4.0);
    let mut send = false;
    let enter = field(ui, |ui| {
        ui.horizontal(|ui| {
            // Exactly what is left beside the send button: any more and the pane widens itself
            // a little every frame (panels grow to fit their content).
            let w = (ui.available_width() - 26.0 - ui.spacing().item_spacing.x).max(40.0);
            let edit = ui.add_enabled(
                !running,
                egui::TextEdit::multiline(&mut a.state.input).hint_text("Ask for a change…").frame(egui::Frame::NONE).desired_rows(2).desired_width(w),
            );
            let can = !running && !a.state.input.trim().is_empty();
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if can { Sense::click() } else { Sense::hover() });
                ui.painter().rect_filled(r, CornerRadius::same(13), if can { t.accent } else { t.border });
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "➤", theme::font(12.0), if can { t.accent_text } else { t.text_faint });
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, can, "Send"));
                send = can && resp.on_hover_text("Send (Enter)").clicked();
            });
            edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
        })
        .inner
    });
    ui.label(RichText::new("Enter to send · Shift+Enter for a new line").font(theme::font(META)).color(t.text_faint));
    if enter && a.state.input.ends_with('\n') {
        a.state.input.pop();
    }
    if (send || enter) && !running && !a.state.input.trim().is_empty() {
        let text = std::mem::take(&mut a.state.input);
        client::send_prompt(app, &text);
    }
}

fn changes(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let running = client::busy(app);
    let turns = deck_turns(app);
    let confirm = app.agent.as_ref().and_then(|a| a.state.confirm_revert);
    let mut action = None;
    egui::ScrollArea::vertical().id_salt("agent_changes").auto_shrink([false, false]).show(ui, |ui| {
        if turns.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("No changes yet").font(theme::bold(BODY)).color(t.text));
            ui.label(RichText::new("Each request you send shows up here with the slides it touched, what changed, and a Revert button.").font(theme::font(12.0)).color(t.text_dim));
        }
        for (i, c) in turns.iter().rev() {
            let reverted = client::is_reverted(app, *i);
            egui::Frame::NONE.fill(t.chrome).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    status_pill(ui, &c.status, reverted, t);
                    ui.label(RichText::new(changes_summary(c)).font(theme::font(12.0)).color(t.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| revert_button(ui, t, *i, c, running, reverted, &mut action));
                });
                if !c.slides.is_empty() {
                    ui.horizontal_wrapped(|ui| slide_chips(ui, c, 12, &mut action));
                }
                ui.label(RichText::new(format!("“{}”", c.prompt.trim())).font(theme::font(12.0)).color(t.text_dim));
                let mut meta = vec![ago(c.time)];
                if c.added > 0 {
                    meta.push(format!("+{} slide{}", c.added, if c.added == 1 { "" } else { "s" }));
                }
                if c.removed > 0 {
                    meta.push(format!("−{} slide{}", c.removed, if c.removed == 1 { "" } else { "s" }));
                }
                ui.label(RichText::new(meta.join(" · ")).font(theme::font(META)).color(t.text_faint));
                let labels = client::edit_labels(&c.actions);
                if !labels.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(format!("What changed ({})", c.actions.len())).font(theme::font(12.0)))
                        .id_salt(("changes", i))
                        .default_open(false)
                        .show(ui, |ui| {
                            for l in labels {
                                ui.label(RichText::new(format!("• {l}")).font(theme::font(12.0)).color(t.text_dim));
                            }
                        });
                }
                if confirm == Some(*i) {
                    confirm_row(ui, *i, t, &mut action);
                }
            });
            ui.add_space(8.0);
        }
    });
    apply(app, action);
}

/// While a turn runs: a light veil over everything but the agent pane, so clicks can't edit the
/// deck under the agent (keyboard shortcuts are skipped by the caller; the pane says what's going
/// on and has Stop).
pub fn veil(app: &SlideApp, ctx: &egui::Context, pane_left: Option<f32>) {
    if !client::busy(app) {
        return;
    }
    let t = Tokens::get(ctx);
    let full = ctx.content_rect();
    let rect = egui::Rect::from_min_max(full.min, egui::pos2(pane_left.unwrap_or(full.max.x), full.max.y));
    egui::Area::new(egui::Id::new("agent_veil")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let resp = ui.allocate_rect(egui::Rect::from_min_size(rect.min, rect.size()), Sense::click_and_drag());
        ui.painter().rect_filled(resp.rect, CornerRadius::ZERO, t.shadow.gamma_multiply(0.2));
    });
}

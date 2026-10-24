//! The agent pane (right side, collapsible).
//!
//! Layout: header (title, model, New chat, close) · underline tabs Chat | Changes ·
//! a status strip with Stop while the agent works · the tab's content · the prompt box with an
//! inline send button. Chat shows your messages as bubbles and the agent's replies with an accent
//! bar; the model's running commentary between steps is dimmed and its steps fold into one row,
//! followed by a one-line result with slide chips and Revert. Changes is the deck's change log.
//! The deck stays editable while the agent works (co-editing): each edit is its own undo step
//! and the agent is told about the user's.

use egui::text::LayoutJob;
use egui::{Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, TextFormat, Ui, vec2};

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

/// Every symbol character the pane draws as text (checked against the UI font by a test: a
/// missing glyph shows as an empty box).
pub const SYMBOLS: &str = "✓×↺↻■!→“”•·…−";

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
                        PaneTab::Files => files(app, ui, &t),
                        PaneTab::Instructions => instructions(app, ui, &t),
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
    let has_files = app.has_workspaces();
    let Some(a) = app.agent.as_mut() else { return };
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let mut tabs = vec![
            (PaneTab::Chat, "Chat".to_string(), "The conversation"),
            (PaneTab::Changes, format!("Changes ({n})"), "Everything the agent changed in this deck, with Revert"),
        ];
        if has_files {
            tabs.push((PaneTab::Files, "Files".to_string(), "Your workspace folder: materials for you and the agent"));
        }
        tabs.push((PaneTab::Instructions, "Instructions".to_string(), "How the agent should work: your standing instructions"));
        for (tab, label, tip) in tabs {
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
            // Text and count stacked on the left, Stop on the right: they never overlap.
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("Agent is editing the deck…").font(theme::font(BODY)).color(t.text));
                let count = if steps == 0 { "starting".to_string() } else { format!("{steps} step{}", if steps == 1 { "" } else { "s" }) };
                ui.label(RichText::new(count).font(theme::font(META)).color(t.text_dim));
            });
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

/// A small coloured pill: ✓ done, ■ stopped, ↺ reverted…
fn status_pill(ui: &mut Ui, status: &str, reverted: bool, t: &Tokens) {
    let (text, color) = if reverted {
        ("↺ undone", t.text_dim)
    } else {
        match status {
            "ok" | "" => ("✓ done", ok_color(t)),
            "stopped" => ("■ stopped", warn_color(t)),
            "interrupted" => ("! interrupted", warn_color(t)),
            "limit" => ("! out of time", warn_color(t)),
            _ => ("× error", err_color(t)),
        }
    };
    let tip = if reverted {
        "A Revert took this request's changes back; Restore brings them back"
    } else {
        match status {
            "ok" | "" => "The agent finished this request",
            "stopped" => "You stopped this request; what it did so far stays",
            "interrupted" => "The connection to the agent was lost during this request; what it did so far stays",
            "limit" => "The request ran out of time (15 minutes) or steps (60); what it did so far stays. Continue finishes it",
            _ => "The request failed; what it did so far stays",
        }
    };
    egui::Frame::NONE.fill(color.gamma_multiply(0.15)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).color(color).font(theme::font(META))).on_hover_text(tip);
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
    RestoreAll,
    CancelRevert,
    Details,
    GoTo(usize),
    RemoveQueued(usize),
    ResumeQueue,
    Continue,
}

/// Revert (or Restore once reverted) as a real button.
/// Requests form a timeline: Revert goes back to before a request, so the requests after it are
/// undone too; Restore brings the undone ones back up to and including this one.
fn revert_button(ui: &mut Ui, t: &Tokens, i: usize, c: &TurnCard, running: bool, reverted: bool, out: &mut Option<TurnAction>) {
    let (name, tip, can, action) = if reverted {
        ("Restore to here", "Bring the deck back up to and including this request; requests after it stay undone", !running, TurnAction::Restore(i))
    } else {
        let tip = if c.before.is_none() {
            "Revert is available for requests made since this page was opened"
        } else {
            "Go back to how the deck was before this request. Later requests are undone too; you can restore them"
        };
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

/// While a Revert is in effect: what is undone, and one button to bring it all back.
fn rewind_banner(app: &SlideApp, ui: &mut Ui, t: &Tokens, running: bool, out: &mut Option<TurnAction>) {
    let Some(r) = client::rewind(app) else { return };
    let Some(prompt) = app.agent.as_ref().and_then(|a| a.state.turns.get(r.from)).map(|c| first_line(&c.prompt)) else { return };
    let n = r.undone.len();
    egui::Frame::NONE
        .fill(t.accent.gamma_multiply(0.08))
        .stroke(Stroke::new(1.0, t.accent.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let can = !running;
                let r = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = ui.add_enabled(can, egui::Button::new(RichText::new("↻ Restore all").font(theme::font(12.0)).color(t.accent_text)).fill(t.accent));
                    let b = b.on_hover_text("Bring back everything the Revert undid, as one step");
                    let clicked = b.clicked();
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{n} request{} undone · back to before “{prompt}”", if n == 1 { "" } else { "s" }))
                                    .font(theme::font(12.0))
                                    .color(t.text),
                            )
                            .truncate(),
                        );
                    });
                    clicked
                });
                if r.inner {
                    *out = Some(TurnAction::RestoreAll);
                }
            });
        });
}

/// Reverting a request the deck has moved on from: say exactly what else goes, and how to get it
/// back, before doing it.
fn confirm_row(ui: &mut Ui, i: usize, c: &TurnCard, later: Option<client::LaterChanges>, t: &Tokens, out: &mut Option<TurnAction>) {
    let warn = warn_color(t);
    let what = later.filter(|l| !l.is_empty()).map_or_else(|| "other changes".to_string(), |l| l.describe());
    ui.add_space(2.0);
    egui::Frame::NONE
        .fill(warn.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, warn.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add(egui::Label::new(RichText::new(format!("Go back to before “{}”?", first_line(&c.prompt))).font(theme::bold(BODY)).color(t.text)).selectable(false).wrap());
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "The deck returns to how it was before this request. Also undone: {what}. They stay listed, and Restore all brings them back in one click."
                    ))
                    .font(theme::font(12.0))
                    .color(t.text_dim),
                )
                .selectable(false)
                .wrap(),
            );
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let red = err_color(t);
                if ui.add(egui::Button::new(RichText::new("Go back").font(theme::font(12.0)).color(egui::Color32::WHITE)).fill(red)).clicked() {
                    *out = Some(TurnAction::Revert(i, true));
                }
                if ui.add(egui::Button::new(RichText::new("Cancel").font(theme::font(12.0)))).clicked() {
                    *out = Some(TurnAction::CancelRevert);
                }
            });
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
        Some(TurnAction::RestoreAll) => {
            if !client::restore_all(app)
                && let Some(a) = app.agent.as_mut()
            {
                a.state.items.push(ChatItem::Notice("Nothing to restore: the deck changed since the Revert.".into()));
            }
        }
        Some(TurnAction::RemoveQueued(i)) => client::remove_queued(app, i),
        Some(TurnAction::ResumeQueue) => client::resume_queue(app),
        Some(TurnAction::Continue) => client::queue_prompt(app, "Continue where you stopped and finish the request."),
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
    let queued = client::queued(app);
    let paused = client::queue_paused(app);
    let input_h = 104.0 + queued.len().min(4) as f32 * 26.0 + if queued.is_empty() { 0.0 } else { 24.0 } + if client::rewind(app).is_some() { 40.0 } else { 0.0 };
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
                    let noun = if steps.len() == 1 { "step" } else { "steps" };
                    let title = if failed > 0 { format!("{} {noun} ({failed} failed)", steps.len()) } else { format!("{} {noun}", steps.len()) };
                    egui::CollapsingHeader::new(RichText::new(title).font(theme::font(META)).color(t.text_dim)).id_salt(("steps", start)).default_open(false).show(ui, |ui| {
                        for s in steps {
                            if let ChatItem::Tool { name, ok, summary } = s {
                                let mark = if *ok { "✓" } else { "×" };
                                ui.label(RichText::new(format!("{mark} {name}")).font(theme::font(META)).color(if *ok { t.text_dim } else { err_color(t) })).on_hover_text(summary);
                                // Why it failed, without hovering (the agent usually recovers on its own).
                                if !*ok && !summary.is_empty() {
                                    let why: String = summary.chars().take(140).collect();
                                    ui.add(egui::Label::new(RichText::new(format!("   {why}")).font(theme::font(META)).color(t.text_faint)).wrap());
                                }
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
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                revert_button(ui, t, *i, c, running, reverted, &mut action);
                                // Cut off by a limit: pick up where it stopped (the latest request only).
                                if c.status == "limit" && !running && !reverted && *i + 1 == a.state.turns.len() {
                                    let r = ui.add(egui::Button::new(RichText::new("Continue").font(theme::font(12.0)).color(t.accent_text)).fill(t.accent));
                                    if r.on_hover_text("Ask the agent to finish this request").clicked() {
                                        action = Some(TurnAction::Continue);
                                    }
                                }
                            });
                        });
                        if a.state.confirm_revert == Some(*i) {
                            confirm_row(ui, *i, c, client::later_changes(app, *i), t, &mut action);
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
    let mut ra = None;
    rewind_banner(app, ui, t, running, &mut ra);
    apply(app, ra);
    if !queued.is_empty() {
        let mut qa = None;
        queue_list(ui, t, &queued, paused, &mut qa);
        apply(app, qa);
    }
    let Some(a) = app.agent.as_mut() else { return };
    let mut send = false;
    let enter = field(ui, |ui| {
        ui.horizontal(|ui| {
            // Exactly what is left beside the send button: any more and the pane widens itself
            // a little every frame (panels grow to fit their content).
            let w = (ui.available_width() - 26.0 - ui.spacing().item_spacing.x).max(40.0);
            let hint = if running { "Queue a follow-up…" } else { "Ask for a change…" };
            // A fixed id: the layout around the field changes as requests run and queue, and the
            // field must keep its focus through that.
            let edit = ui.add(egui::TextEdit::multiline(&mut a.state.input).id(egui::Id::new("agent_prompt")).hint_text(hint).frame(egui::Frame::NONE).desired_rows(2).desired_width(w));
            let can = !a.state.input.trim().is_empty();
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if can { Sense::click() } else { Sense::hover() });
                ui.painter().rect_filled(r, CornerRadius::same(13), if can { t.accent } else { t.border });
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "→", theme::bold(14.0), if can { t.accent_text } else { t.text_faint });
                let (name, tip) = if running { ("Queue", "Queue: sent when the agent is done (Enter)") } else { ("Send", "Send (Enter)") };
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, can, name));
                send = can && resp.on_hover_text(tip).clicked();
            });
            edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
        })
        .inner
    });
    let help = if running { "Enter to queue · Shift+Enter for a new line" } else { "Enter to send · Shift+Enter for a new line" };
    ui.label(RichText::new(help).font(theme::font(META)).color(t.text_faint));
    if enter && a.state.input.ends_with('\n') {
        a.state.input.pop();
    }
    if (send || enter) && !a.state.input.trim().is_empty() {
        let text = std::mem::take(&mut a.state.input);
        client::queue_prompt(app, &text);
        // Keep typing: the next request (or a follow-up to queue) goes in the same field.
        ui.memory_mut(|m| m.request_focus(egui::Id::new("agent_prompt")));
    }
}

/// The workspace folder: its files, with what each can be used for.
fn files(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    use crate::workspace::{Kind, UserOp, WsStatus, kind_of, size_label};
    let Some(ws) = app.workspace() else { return };
    let status = ws.host.status();
    let mut pick = false;
    let mut reconnect = false;
    let mut forget = false;
    let mut refresh = false;
    let empty_state = |ui: &mut Ui, title: &str, body: &str| {
        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 20.0, t.accent.gamma_multiply(0.12));
            icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(22.0, 22.0)), Icon::OpenFolder, t.accent, false);
            ui.add_space(4.0);
            ui.label(RichText::new(title).font(theme::bold(BODY)).color(t.text));
            ui.add(egui::Label::new(RichText::new(body).font(theme::font(12.0)).color(t.text_dim)).halign(Align::Center).wrap());
        });
        ui.add_space(6.0);
    };
    match &status {
        WsStatus::None | WsStatus::Failed(_) => {
            empty_state(
                ui,
                "Open a workspace folder",
                "Keep briefs, notes, data, pictures, draft decks and style templates in a folder. You and the agent can use them from here. Files stay on your computer; the agent reads one only when it needs it.",
            );
            if let WsStatus::Failed(e) = &status {
                ui.label(RichText::new(e).font(theme::font(12.0)).color(err_color(t)));
            }
            ui.vertical_centered(|ui| pick = primary_button(ui, t, "Open folder…").clicked());
            ui.add_space(6.0);
            ui.label(RichText::new("Tip: an AGENT.md file in the folder gives the agent standing instructions for this project.").font(theme::font(META)).color(t.text_faint));
        }
        WsStatus::NeedsPermission { name } => {
            empty_state(ui, name, "The browser needs your OK to read this folder again.");
            ui.vertical_centered(|ui| {
                reconnect = primary_button(ui, t, "Reconnect").clicked();
                forget = ui.add(egui::Button::new(RichText::new("Use another folder").font(theme::font(12.0))).frame(false)).clicked();
            });
        }
        WsStatus::Loading { name } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("Reading “{name}”…")).color(t.text_dim));
            });
        }
        WsStatus::Ready { name } => {
            let files = ws.host.files();
            let has_agent_md = crate::workspace::instructions_file(&files).is_some();
            let busy_paths: Vec<String> = ws.user_ops.iter().map(|(p, _)| p.clone()).collect();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Folder, t.accent, false);
                ui.label(RichText::new(name).font(theme::bold(BODY)).color(t.text));
                ui.label(RichText::new(format!("{} files", files.len())).font(theme::font(META)).color(t.text_faint));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    forget = ui.add(egui::Button::new(RichText::new("Close").font(theme::font(12.0)).color(t.text_dim)).frame(false)).on_hover_text("Stop using this folder").clicked();
                    pick = ui.add(egui::Button::new(RichText::new("Change…").font(theme::font(12.0)).color(t.text_dim)).frame(false)).on_hover_text("Open another folder").clicked();
                    refresh = icon_button(ui, t, Icon::Redo, "Refresh", "Read the folder again", true);
                });
            });
            if has_agent_md {
                ui.label(RichText::new("AGENT.md: project instructions go with every request").font(theme::font(META)).color(ok_color(t)));
            }
            let Some(a) = app.agent.as_mut() else { return };
            field(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Search, t.text_faint, false);
                    ui.add(egui::TextEdit::singleline(&mut a.state.file_filter).hint_text("Filter files…").frame(egui::Frame::NONE).desired_width(f32::INFINITY));
                });
            });
            let filter = a.state.file_filter.to_lowercase();
            let mut open = a.state.file_open.clone();
            let mut act: Option<(String, Option<UserOp>)> = None;
            egui::ScrollArea::vertical().id_salt("agent_files").auto_shrink([false, false]).show(ui, |ui| {
                let mut dir = String::new();
                for f in files.iter().filter(|f| filter.is_empty() || f.path.to_lowercase().contains(&filter)) {
                    let (parent, file) = f.path.rsplit_once('/').unwrap_or(("", f.path.as_str()));
                    if parent != dir {
                        dir = parent.to_string();
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("{dir}/")).font(theme::font(META)).color(t.text_faint));
                    }
                    let kind = kind_of(&f.path);
                    let icon = match kind {
                        Kind::Image => Icon::Picture,
                        Kind::Deck => Icon::Slide,
                        Kind::Theme => Icon::Themes,
                        Kind::Text if f.path.to_lowercase().ends_with(".csv") => Icon::Table,
                        _ => Icon::Notes,
                    };
                    let is_open = open.as_deref() == Some(f.path.as_str());
                    let mut head = Rect::NOTHING;
                    egui::Frame::NONE.fill(if is_open { t.selected } else { egui::Color32::TRANSPARENT }).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(4, 2)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        head = ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            icons::paint(ui.painter(), r, icon, t.text_dim, false);
                            // Painted, not a widget: the row itself is the file's one button.
                            let size = RichText::new(size_label(f.size)).font(theme::font(META)).color(t.text_faint);
                            let w = (ui.available_width() - 56.0).max(40.0);
                            let galley = egui::WidgetText::from(RichText::new(file).font(theme::font(12.5)).color(t.text)).into_galley(ui, Some(egui::TextWrapMode::Truncate), w, egui::TextStyle::Body);
                            let (r, _) = ui.allocate_exact_size(vec2(w, galley.size().y.max(16.0)), Sense::hover());
                            ui.painter().galley(egui::pos2(r.left(), r.center().y - galley.size().y / 2.0), galley, t.text);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if busy_paths.contains(&f.path) {
                                    ui.spinner();
                                } else {
                                    ui.add(egui::Label::new(size).selectable(false));
                                }
                            });
                        })
                        .response
                        .rect;
                        if is_open {
                            ui.horizontal_wrapped(|ui| {
                                let chip = |ui: &mut Ui, label: &str, tip: &str| ui.add(egui::Button::new(RichText::new(label).font(theme::font(12.0))).corner_radius(CornerRadius::same(10)).stroke(Stroke::new(1.0, t.border))).on_hover_text(tip).clicked();
                                match kind {
                                    Kind::Deck => {
                                        if chip(ui, "Open", "Open it as a presentation") {
                                            act = Some((f.path.clone(), Some(UserOp::Open)));
                                        }
                                        if chip(ui, "Apply design", "Use its theme (colours, fonts, effects) in this deck") {
                                            act = Some((f.path.clone(), Some(UserOp::ApplyDesign)));
                                        }
                                    }
                                    Kind::Theme if chip(ui, "Apply design", "Use this theme in this deck") => {
                                        act = Some((f.path.clone(), Some(UserOp::ApplyDesign)));
                                    }
                                    Kind::Image if chip(ui, "Insert", "Put the picture on this slide") => {
                                        act = Some((f.path.clone(), Some(UserOp::Insert)));
                                    }
                                    _ => {}
                                }
                                if chip(ui, "Ask agent", "Write a request about this file") {
                                    act = Some((f.path.clone(), None));
                                }
                            });
                        }
                    });
                    // The name line toggles the file's actions (the chips below stay clickable).
                    let resp = ui.interact(head, ui.id().with(("ws_file", &f.path)), Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, file));
                    if resp.clicked() {
                        open = if is_open { None } else { Some(f.path.clone()) };
                    }
                }
                if files.is_empty() {
                    ui.label(RichText::new("This folder is empty.").color(t.text_dim));
                }
            });
            a.state.file_open = open;
            if let Some((path, op)) = act {
                match op {
                    Some(op) => app.workspace_action(&path, op),
                    None => {
                        if let Some(a) = app.agent.as_mut() {
                            let mention = format!("Use `{path}` ");
                            if !a.state.input.contains(&mention) {
                                if !a.state.input.is_empty() && !a.state.input.ends_with(' ') {
                                    a.state.input.push(' ');
                                }
                                a.state.input.push_str(&mention);
                            }
                            a.state.tab = PaneTab::Chat;
                        }
                    }
                }
            }
        }
    }
    if let Some(ws) = app.workspace_mut() {
        if pick {
            ws.host.pick();
        }
        if reconnect {
            ws.host.reconnect();
        }
        if forget {
            ws.host.forget();
        }
        if refresh {
            ws.host.refresh();
        }
    }
}

fn primary_button(ui: &mut Ui, t: &Tokens, label: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(label).font(theme::font(12.5)).color(t.accent_text)).fill(t.accent).min_size(vec2(96.0, 26.0)))
}

/// The agent's working style: standing instructions the person can edit, sent with every request.
fn instructions(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let Some(a) = app.agent.as_mut() else { return };
    let saved = a.state.instructions.clone();
    let is_default = saved.trim() == a.state.default_instructions.trim();
    ui.label(RichText::new("How the agent works").font(theme::bold(BODY)).color(t.text));
    ui.label(
        RichText::new("Standing instructions sent with every request, for example tone, spelling or how much to ask before acting. Saved to your account.")
            .font(theme::font(12.0))
            .color(t.text_dim),
    );
    ui.add_space(4.0);
    let draft = a.state.instructions_draft.get_or_insert_with(|| saved.clone());
    let h = (ui.available_height() - 64.0).max(120.0);
    let shown = draft.clone();
    egui::Frame::NONE.fill(t.field_bg).stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("agent_instructions").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
            let r = ui.add(egui::TextEdit::multiline(draft).frame(egui::Frame::NONE).desired_width(f32::INFINITY).desired_rows(10).font(theme::font(12.5)));
            r.widget_info(move || {
                let mut info = egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Agent instructions");
                info.current_text_value = Some(shown.clone());
                info
            });
        });
    });
    let changed = a.state.instructions_draft.as_deref().is_some_and(|d| d.trim() != saved.trim());
    let mut save = false;
    let mut reset = false;
    let mut discard = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if !is_default
            && ui
                .add(egui::Button::new(RichText::new("Reset to default").font(theme::font(12.0)).color(t.text_dim)).frame(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        {
            reset = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if changed {
                save = ui.add(egui::Button::new(RichText::new("Save").font(theme::font(12.0)).color(t.accent_text)).fill(t.accent).min_size(vec2(64.0, 24.0))).clicked();
                discard = ui.add(egui::Button::new(RichText::new("Discard").font(theme::font(12.0))).min_size(vec2(64.0, 24.0))).clicked();
            } else {
                ui.label(RichText::new(if is_default { "Default instructions" } else { "Saved" }).font(theme::font(META)).color(t.text_faint));
            }
        });
    });
    if discard {
        a.state.instructions_draft = None;
    }
    if save {
        let text = a.state.instructions_draft.clone().unwrap_or_default();
        client::set_instructions(app, &text);
    } else if reset {
        client::set_instructions(app, "");
    }
}

/// Requests waiting their turn, above the prompt field.
fn queue_list(ui: &mut Ui, t: &Tokens, queued: &[String], paused: bool, action: &mut Option<TurnAction>) {
    egui::Frame::NONE.fill(t.chrome).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let head = if paused { "Queue paused" } else { "Up next" };
            ui.label(RichText::new(head).font(theme::bold(META)).color(if paused { warn_color(t) } else { t.text_dim }));
            if paused {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("Send next").font(theme::font(12.0)).color(t.accent_text)).fill(t.accent))
                        .on_hover_text("The last request didn't finish; send the next one anyway")
                        .clicked()
                    {
                        *action = Some(TurnAction::ResumeQueue);
                    }
                });
            }
        });
        egui::ScrollArea::vertical().id_salt("agent_queue").max_height(4.0 * 26.0).show(ui, |ui| {
            for (i, q) in queued.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).font(theme::font(META)).color(t.text_faint));
                    let w = (ui.available_width() - 30.0).max(40.0);
                    ui.add_sized(vec2(w, 20.0), egui::Label::new(RichText::new(first_line(q)).font(theme::font(12.0)).color(t.text)).truncate())
                        .on_hover_text(q.as_str());
                    if icon_button(ui, t, Icon::Close, "Remove from queue", "Remove from queue", true) {
                        *action = Some(TurnAction::RemoveQueued(i));
                    }
                });
            }
        });
    });
    ui.add_space(4.0);
}

fn first_line(s: &str) -> String {
    s.trim().lines().next().unwrap_or_default().to_string()
}

fn changes(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let running = client::busy(app);
    let turns = deck_turns(app);
    let confirm = app.agent.as_ref().and_then(|a| a.state.confirm_revert);
    let mut action = None;
    rewind_banner(app, ui, t, running, &mut action);
    ui.add_space(6.0);
    egui::ScrollArea::vertical().id_salt("agent_changes").auto_shrink([false, false]).show(ui, |ui| {
        if turns.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("No changes yet").font(theme::bold(BODY)).color(t.text));
            ui.label(RichText::new("Each request you send shows up here with the slides it touched, what changed, and a Revert button.").font(theme::font(12.0)).color(t.text_dim));
        }
        for (i, c) in turns.iter().rev() {
            let reverted = client::is_reverted(app, *i);
            // Undone requests read as set aside: dimmed, with a dashed-looking outline.
            let frame = if reverted { egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)) } else { egui::Frame::NONE.fill(t.chrome) };
            frame.corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if reverted {
                    ui.multiply_opacity(0.75);
                }
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
                    confirm_row(ui, *i, c, client::later_changes(app, *i), t, &mut action);
                }
            });
            ui.add_space(8.0);
        }
    });
    apply(app, action);
}

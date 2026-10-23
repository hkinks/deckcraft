//! The agent pane (right side, collapsible): sign-in, the Chat tab (conversation with each
//! request's steps folded and a one-line summary with Revert), the Changes tab (the deck's change
//! log: request, time, status, changed slides, edits, Revert/Restore), the prompt box with
//! Send/Stop, and the veil that keeps editing input off the deck while the agent works.

use egui::text::LayoutJob;
use egui::{Align, CornerRadius, Layout, RichText, Sense, TextFormat, Ui, vec2};

use super::client::{self, ChatItem, PaneTab, RevertOutcome, TurnCard, TurnState};
use super::transport::LinkStatus;
use crate::SlideApp;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens};

pub const WIDTH: f32 = 340.0;
const RED: egui::Color32 = egui::Color32::from_rgb(0xC4, 0x2B, 0x1C);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x1E, 0x7B, 0x34);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xB2, 0x6B, 0x00);

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
        .frame(egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin::same(10)))
        .show(ui, |ui| {
            header(app, ui, &t);
            ui.add_space(4.0);
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
                    ui.separator();
                    match app.agent.as_ref().map_or(PaneTab::Chat, |a| a.state.tab) {
                        PaneTab::Chat => chat(app, ui, &t),
                        PaneTab::Changes => changes(app, ui, &t),
                    }
                }
            }
        });
}

fn header(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("AI Agent").font(theme::bold(16.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(4), t.hover);
            }
            icons::paint(ui.painter(), r.shrink(3.0), Icon::Close, t.text_dim, false);
            // While a turn runs the pane stays open: Stop lives here.
            if resp.on_hover_text("Close the agent pane").clicked() && !client::busy(app) {
                toggle(app);
            }
            let Some(a) = app.agent.as_mut() else { return };
            if a.state.providers.len() > 1 {
                let current = a.state.provider.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt("agent_provider").selected_text(current.clone()).show_ui(ui, |ui| {
                    for p in a.state.providers.clone() {
                        let label = format!("{} ({})", p.id, p.model);
                        if ui.selectable_label(current == p.id, label).clicked() {
                            a.state.provider = Some(p.id.clone());
                        }
                    }
                });
            } else if let Some(p) = a.state.providers.first() {
                ui.label(RichText::new(&p.model).color(t.text_dim).font(theme::font(12.0)));
            }
        });
    });
}

/// The deck's turns (its change log), oldest first, with their indices.
fn deck_turns(app: &SlideApp) -> Vec<(usize, TurnCard)> {
    let uid = app.session.active().map(|d| d.uid);
    app.agent
        .as_ref()
        .map(|a| a.state.turns.iter().cloned().enumerate().filter(|(_, c)| Some(c.uid) == uid || c.uid == 0).collect())
        .unwrap_or_default()
}

fn tabs(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let n = deck_turns(app).len();
    let busy = client::busy(app);
    let Some(a) = app.agent.as_mut() else { return };
    let mut new_chat = false;
    ui.horizontal(|ui| {
        if ui.selectable_label(a.state.tab == PaneTab::Chat, "Chat").clicked() {
            a.state.tab = PaneTab::Chat;
        }
        if ui.selectable_label(a.state.tab == PaneTab::Changes, format!("Changes ({n})")).on_hover_text("Everything the agent changed in this deck, with Revert").clicked() {
            a.state.tab = PaneTab::Changes;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !busy
                && ui
                    .add(egui::Button::new(RichText::new("✚ New chat").color(t.text_dim)).frame(false))
                    .on_hover_text("Start a new conversation about this deck (the change log stays)")
                    .clicked()
            {
                new_chat = true;
            }
        });
    });
    if new_chat {
        client::new_conversation(app);
        if let Some(a) = app.agent.as_mut() {
            a.state.tab = PaneTab::Chat;
        }
    }
}

/// `**bold**` runs of one line: (text, bold). Other markdown stays as written.
pub fn md_segments(line: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    let mut bold = false;
    for (i, part) in line.split("**").enumerate() {
        if i > 0 {
            bold = !bold;
        }
        if !part.is_empty() {
            out.push((part.to_string(), bold));
        }
    }
    // An unmatched `**` is literal.
    if line.matches("**").count() % 2 == 1 {
        return vec![(line.to_string(), false)];
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

fn markdown(ui: &mut Ui, text: &str, t: &Tokens) {
    let mut job = LayoutJob::default();
    for (n, raw) in text.lines().enumerate() {
        if n > 0 {
            job.append("\n", 0.0, TextFormat { font_id: theme::font(13.0), color: t.text, ..Default::default() });
        }
        let (line, heading) = md_line(raw);
        for (seg, bold) in md_segments(&line) {
            let font = if bold || heading { theme::bold(13.0) } else { theme::font(13.0) };
            job.append(&seg, 0.0, TextFormat { font_id: font, color: t.text, ..Default::default() });
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

fn status_chip(ui: &mut Ui, status: &str, t: &Tokens) {
    let (text, color) = match status {
        "ok" | "" => ("✓ done", GREEN),
        "stopped" => ("■ stopped", AMBER),
        "interrupted" => ("⚠ interrupted", AMBER),
        "limit" => ("⚠ limit reached", AMBER),
        _ => ("✗ error", RED),
    };
    let _ = t;
    ui.label(RichText::new(text).color(color).font(theme::font(11.0)));
}

fn changes_summary(c: &TurnCard) -> String {
    let n = c.actions.len();
    if n == 0 {
        return "No changes".into();
    }
    let mut s = format!("{n} change{}", if n == 1 { "" } else { "s" });
    if !c.slides.is_empty() {
        let list: Vec<String> = c.slides.iter().take(6).map(|i| (i + 1).to_string()).collect();
        s.push_str(&format!(" · slide{} {}", if c.slides.len() == 1 { "" } else { "s" }, list.join(", ")));
        if c.slides.len() > 6 {
            s.push('…');
        }
    }
    s
}

/// What a click on a turn's buttons asks for, applied after drawing.
enum TurnAction {
    Revert(usize, bool),
    CancelRevert,
    Details,
    GoTo(usize),
}

fn revert_button(ui: &mut Ui, i: usize, c: &TurnCard, running: bool, out: &mut Option<TurnAction>) {
    let can = c.before.is_some() && !running;
    let tip = if c.before.is_none() { "Revert is available for requests made since this page was opened" } else { "Put the deck back as it was before this request (one undoable step)" };
    if ui.add_enabled(can, egui::Button::new("Revert")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
        *out = Some(TurnAction::Revert(i, false));
    }
}

fn confirm_row(ui: &mut Ui, i: usize, t: &Tokens, out: &mut Option<TurnAction>) {
    ui.label(RichText::new("Changes made after this request will be discarded too.").color(t.text_dim));
    ui.horizontal(|ui| {
        if ui.button("Discard later changes and revert").clicked() {
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
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 4))
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

fn chat(app: &mut SlideApp, ui: &mut Ui, t: &Tokens) {
    let running = client::busy(app);
    let input_h = 110.0;
    let scroll_h = (ui.available_height() - input_h).max(80.0);
    let mut action = None;
    egui::ScrollArea::vertical().id_salt("agent_chat").max_height(scroll_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        let Some(a) = app.agent.as_ref() else { return };
        if a.state.items.is_empty() {
            ui.label(RichText::new("Ask for changes to this deck: \"add a slide comparing the three plans\", \"make all titles dark blue\", \"turn slide 2 into a table\". Each request is one undo step; the Changes tab lists them all.").color(t.text_dim));
        }
        let items = &a.state.items;
        let mut k = 0;
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
                    egui::CollapsingHeader::new(RichText::new(title).font(theme::font(12.0)).color(t.text_dim)).id_salt(("steps", start)).default_open(false).show(ui, |ui| {
                        for s in steps {
                            if let ChatItem::Tool { name, ok, summary } = s {
                                let mark = if *ok { "✓" } else { "✗" };
                                ui.label(RichText::new(format!("{mark} {name}")).font(theme::font(12.0)).color(if *ok { t.text_dim } else { RED })).on_hover_text(summary);
                            }
                        }
                    });
                    ui.add_space(2.0);
                    continue;
                }
                ChatItem::User(s) => {
                    egui::Frame::NONE.fill(t.selected).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(s).color(t.text));
                    });
                }
                ChatItem::Assistant(s) => markdown(ui, s, t),
                ChatItem::Notice(s) => {
                    ui.label(RichText::new(s).italics().color(t.text_dim));
                }
                ChatItem::Turn(i) => {
                    if let Some(c) = a.state.turns.get(*i) {
                        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            // Summary on its own line (it wraps in a narrow pane), buttons below.
                            ui.horizontal_wrapped(|ui| {
                                status_chip(ui, &c.status, t);
                                ui.label(RichText::new(changes_summary(c)).font(theme::font(12.0)));
                            });
                            ui.horizontal(|ui| {
                                if ui.add(egui::Button::new(RichText::new("Details").color(t.text_dim)).frame(false)).clicked() {
                                    action = Some(TurnAction::Details);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| revert_button(ui, *i, c, running, &mut action));
                            });
                            if a.state.confirm_revert == Some(*i) {
                                confirm_row(ui, *i, t, &mut action);
                            }
                        });
                    }
                }
            }
            ui.add_space(4.0);
            k += 1;
        }
    });
    apply(app, action);
    ui.separator();
    if running {
        let stopping = app.agent.as_ref().is_some_and(|a| matches!(a.state.turn, TurnState::Running { stopping: true, .. }));
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Agent is editing the deck…").color(t.text));
        });
        if stopping {
            ui.add_enabled(false, egui::Button::new("Stopping…"));
        } else if ui.button("Stop").clicked() {
            client::stop(app);
        }
        return;
    }
    let Some(a) = app.agent.as_mut() else { return };
    let edit = field(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut a.state.input)
                .hint_text("Describe a change… (Enter to send, Shift+Enter for a new line)")
                .frame(egui::Frame::NONE)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        )
    });
    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    if enter && a.state.input.ends_with('\n') {
        a.state.input.pop();
    }
    let can_send = !a.state.input.trim().is_empty();
    let send = ui.add_enabled(can_send, egui::Button::new("Send")).clicked() || (enter && can_send);
    if send {
        let text = app.agent.as_mut().map(|a| std::mem::take(&mut a.state.input)).unwrap_or_default();
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
            ui.label(RichText::new("No agent changes to this deck yet. Each request you send appears here, with what it changed and a Revert button.").color(t.text_dim));
        }
        for (i, c) in turns.iter().rev() {
            egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    status_chip(ui, &c.status, t);
                    ui.label(RichText::new(ago(c.time)).font(theme::font(11.0)).color(t.text_faint));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| revert_button(ui, *i, c, running, &mut action));
                });
                ui.label(RichText::new(&c.prompt).color(t.text));
                if !c.slides.is_empty() || c.added > 0 || c.removed > 0 {
                    ui.horizontal_wrapped(|ui| {
                        for s in c.slides.iter().take(12) {
                            if ui.small_button(format!("Slide {}", s + 1)).on_hover_text("Go to this slide").clicked() {
                                action = Some(TurnAction::GoTo(*s));
                            }
                        }
                        let mut extra = vec![];
                        if c.added > 0 {
                            extra.push(format!("+{} added", c.added));
                        }
                        if c.removed > 0 {
                            extra.push(format!("−{} removed", c.removed));
                        }
                        if !extra.is_empty() {
                            ui.label(RichText::new(extra.join(" · ")).font(theme::font(11.0)).color(t.text_dim));
                        }
                    });
                }
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
            ui.add_space(6.0);
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

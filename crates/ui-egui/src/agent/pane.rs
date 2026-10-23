//! The agent chat pane (right side, collapsible): sign-in, conversation, turn cards with Revert,
//! the prompt box with Send/Stop, and the veil that keeps editing input off the deck while the
//! agent works.

use egui::{Align, CornerRadius, Layout, RichText, Sense, Ui, vec2};

use super::client::{self, ChatItem, RevertOutcome, TurnState};
use super::transport::LinkStatus;
use crate::SlideApp;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens};

pub const WIDTH: f32 = 340.0;

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
    egui::Panel::right("agent_pane")
        .resizable(true)
        .default_size(WIDTH)
        .size_range(280.0..=640.0)
        .frame(egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin::same(10)))
        .show(ui, |ui| {
            header(app, ui, &t);
            ui.add_space(6.0);
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
                LinkStatus::Connected => chat(app, ui, &t),
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
            if resp.on_hover_text("Close the agent pane").clicked() {
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

fn sign_in(app: &mut SlideApp, ui: &mut Ui, error: Option<String>) {
    let Some(a) = app.agent.as_mut() else { return };
    ui.label("Sign in to the DeckCraft agent server.");
    ui.add_space(6.0);
    ui.label("Username");
    ui.add(egui::TextEdit::singleline(&mut a.state.login_user).desired_width(f32::INFINITY));
    ui.label("Password");
    let pw = ui.add(egui::TextEdit::singleline(&mut a.state.login_password).password(true).desired_width(f32::INFINITY));
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
    let input_h = 96.0;
    let scroll_h = (ui.available_height() - input_h - 12.0).max(80.0);
    let mut revert: Option<(usize, bool)> = None;
    egui::ScrollArea::vertical().max_height(scroll_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        let Some(a) = app.agent.as_ref() else { return };
        if a.state.items.is_empty() {
            ui.label(RichText::new("Ask for changes to this deck: \"add a slide comparing the three plans\", \"make all titles dark blue\", \"turn slide 2 into a table\". Each request is one undo step.").color(t.text_dim));
        }
        for item in &a.state.items {
            match item {
                ChatItem::User(s) => {
                    egui::Frame::NONE.fill(t.selected).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(s).color(t.text));
                    });
                }
                ChatItem::Assistant(s) => {
                    ui.label(RichText::new(s).color(t.text));
                }
                ChatItem::Tool { name, ok, summary } => {
                    let mark = if *ok { "✓" } else { "✗" };
                    ui.label(RichText::new(format!("{mark} {name}")).font(theme::font(12.0)).color(t.text_dim)).on_hover_text(summary);
                }
                ChatItem::Notice(s) => {
                    ui.label(RichText::new(s).italics().color(t.text_dim));
                }
                ChatItem::Turn(i) => {
                    let Some(card) = a.state.turns.get(*i) else { continue };
                    egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let n = card.actions.len();
                        let what = if n == 0 { "No changes".to_string() } else { format!("{n} change{}", if n == 1 { "" } else { "s" }) };
                        let status = if card.status == "ok" || card.status.is_empty() { String::new() } else { format!(" · {}", card.status) };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{what}{status}")).font(theme::bold(12.0)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let can = card.before.is_some() && !running;
                                let tip = if card.before.is_none() { "Revert is available for turns made since this page was opened" } else { "Put the deck back as it was before this request" };
                                if ui.add_enabled(can, egui::Button::new("Revert")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                                    revert = Some((*i, false));
                                }
                            });
                        });
                        if n > 0 {
                            ui.label(RichText::new(card.actions.join(", ")).font(theme::font(11.0)).color(t.text_faint));
                        }
                        if a.state.confirm_revert == Some(*i) {
                            ui.label(RichText::new("Later changes to the deck will be discarded too.").color(t.text_dim));
                            ui.horizontal(|ui| {
                                if ui.button("Discard later changes and revert").clicked() {
                                    revert = Some((*i, true));
                                }
                                if ui.button("Cancel").clicked() {
                                    revert = Some((usize::MAX, false));
                                }
                            });
                        }
                    });
                }
            }
            ui.add_space(4.0);
        }
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Working…").color(t.text_dim));
            });
        }
    });
    match revert {
        Some((usize::MAX, _)) => {
            if let Some(a) = app.agent.as_mut() {
                a.state.confirm_revert = None;
            }
        }
        Some((i, confirmed)) => {
            if client::revert(app, i, confirmed) == RevertOutcome::Unavailable
                && let Some(a) = app.agent.as_mut()
            {
                a.state.items.push(ChatItem::Notice("That turn can't be reverted here (switch to its deck first).".into()));
            }
        }
        None => {}
    }
    ui.separator();
    let Some(a) = app.agent.as_mut() else { return };
    let edit = ui.add(
        egui::TextEdit::multiline(&mut a.state.input)
            .hint_text("Describe a change… (Enter to send, Shift+Enter for a new line)")
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    if enter && a.state.input.ends_with('\n') {
        a.state.input.pop();
    }
    let stopping = matches!(a.state.turn, TurnState::Running { stopping: true, .. });
    let can_send = !a.state.input.trim().is_empty();
    let mut send = false;
    let mut stop = false;
    ui.horizontal(|ui| {
        if stopping {
            ui.add_enabled(false, egui::Button::new("Stopping…"));
        } else if running {
            stop = ui.button("Stop").clicked();
        } else {
            send = ui.add_enabled(can_send, egui::Button::new("Send")).clicked() || (enter && can_send);
        }
    });
    if stop {
        client::stop(app);
    } else if send {
        let text = app.agent.as_mut().map(|a| std::mem::take(&mut a.state.input)).unwrap_or_default();
        client::send_prompt(app, &text);
    }
}

/// While a turn runs: a veil over everything but the agent pane, so clicks can't edit the deck
/// under the agent (keyboard shortcuts are skipped by the caller).
pub fn veil(app: &SlideApp, ctx: &egui::Context, pane_left: Option<f32>) {
    if !client::busy(app) {
        return;
    }
    let t = Tokens::get(ctx);
    let full = ctx.content_rect();
    let rect = egui::Rect::from_min_max(full.min, egui::pos2(pane_left.unwrap_or(full.max.x), full.max.y));
    egui::Area::new(egui::Id::new("agent_veil")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let resp = ui.allocate_rect(egui::Rect::from_min_size(rect.min, rect.size()), Sense::click_and_drag());
        ui.painter().rect_filled(resp.rect, CornerRadius::ZERO, t.shadow.gamma_multiply(0.25));
        let badge = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 70.0), vec2(260.0, 34.0));
        ui.painter().rect_filled(badge, CornerRadius::same(17), t.card);
        ui.put(badge, egui::Label::new(RichText::new("Agent is editing the deck…").color(t.text)));
    });
}

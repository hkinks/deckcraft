//! The agent pane in the real app UI, driven like a person would.

use deckcraft_engine::Session;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::client;
use super::protocol::{ProviderInfo, ServerMsg};
use super::test_support::{Fake, Wire};
use super::transport::LinkStatus;
use crate::{Services, SlideApp};

fn harness(status: LinkStatus) -> (Harness<'static, SlideApp>, Wire) {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "title", "title": "Deck"})).unwrap();
    let mut app = SlideApp::new(s, Services::default());
    let wire = Wire::default();
    *wire.status.borrow_mut() = Some(status);
    app.set_agent_transport(Box::new(Fake(wire.clone())));
    wire.push(ServerMsg::Ready {
        user: "ana".into(),
        providers: vec![ProviderInfo { id: "qwen-local".into(), model: "qwen3.8".into(), vision: false }],
        default_provider: "qwen-local".into(),
    });
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    (h, wire)
}

#[test]
fn logged_out_shows_sign_in() {
    let (h, _) = harness(LinkStatus::LoggedOut);
    h.get_by_label("Sign in");
}

#[test]
fn send_button_sends_the_prompt() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    h.state_mut().agent.as_mut().unwrap().state.input = "make a title slide".into();
    h.get_by_label("Send").click();
    h.run_steps(2);
    let sent = wire.sent_of("user_message");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["text"], "make a title slide");
    assert!(client::busy(h.state()));
    assert!(h.state().agent.as_ref().unwrap().state.input.is_empty());
}

#[test]
fn stop_button_while_running() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), "long job");
    h.run_steps(2);
    h.get_by_label("Stop").click();
    h.run_steps(2);
    assert_eq!(wire.sent_of("stop").len(), 1);
}

#[test]
fn editing_keys_are_locked_while_the_agent_works() {
    let (mut h, _wire) = harness(LinkStatus::Connected);
    h.state_mut().run("slide.new", json!({})).unwrap();
    let n = h.state().session.active().unwrap().doc.slides.len();
    client::send_prompt(h.state_mut(), "busy");
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.slides.len(), n, "Ctrl+Z must not undo during a turn");
    h.get_by_label("Agent is editing the deck…");
}

#[test]
fn title_bar_button_toggles_the_pane() {
    let (mut h, _) = harness(LinkStatus::Connected);
    assert!(h.state().agent.as_ref().unwrap().state.open);
    h.get_by_label("AI agent").click();
    h.run_steps(2);
    assert!(!h.state().agent.as_ref().unwrap().state.open);
}

/// One finished turn that added a slide.
fn after_turn(prompt: &str) -> (Harness<'static, SlideApp>, Wire) {
    let (mut h, wire) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), prompt);
    wire.push(ServerMsg::ToolCall { id: "t1".into(), name: "run_command".into(), input: json!({"command": "slide.new", "params": {}}) });
    wire.push(ServerMsg::ToolCall { id: "t2".into(), name: "inspect_slide".into(), input: json!({}) });
    wire.push(ServerMsg::TurnEnd { turn_id: "turn-1".into(), summary: String::new(), actions: vec!["slide.new".into()], slides: vec![], status: "ok".into() });
    h.run_steps(6);
    assert!(!client::busy(h.state()));
    (h, wire)
}

#[test]
fn steps_fold_into_one_row() {
    let (h, _) = after_turn("add a slide");
    h.get_by_label("2 steps");
}

#[test]
fn changes_tab_lists_turns_and_reverts() {
    let (mut h, _) = after_turn("add a closing slide");
    let n = h.state().session.active().unwrap().doc.slides.len();
    h.get_by_label("Changes (1)").click();
    h.run_steps(2);
    h.get_by_label("add a closing slide");
    h.get_by_label("Revert").click();
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.slides.len(), n - 1);
}

#[test]
fn slide_chip_jumps_to_the_changed_slide() {
    let (mut h, _) = after_turn("add");
    h.state_mut().run("slide.go", json!({"index": 0})).unwrap();
    h.run_steps(1);
    let n = h.state().session.active().unwrap().doc.slides.len();
    h.get_by_label("Changes (1)").click();
    h.run_steps(2);
    h.get_by_label(&format!("Slide {n}")).click();
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().selection.slide, n - 1);
}

#[test]
fn markdown_bold_and_bullets_render_without_markers() {
    let segs = super::pane::md_segments("**Slide 1** now reads *x*");
    assert_eq!(segs, vec![("Slide 1".to_string(), true), (" now reads *x*".to_string(), false)]);
    assert_eq!(super::pane::md_line("- item"), ("• item".to_string(), false));
    assert_eq!(super::pane::md_line("## Done"), ("Done".to_string(), true));
}

#[test]
fn home_ribbon_button_toggles_the_pane() {
    let (mut h, _) = harness(LinkStatus::Connected);
    // The test fonts make the ribbon wider than in the browser: give it room.
    h.set_size(egui::vec2(2200.0, 900.0));
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "AI Agent").click();
    h.run_steps(2);
    assert!(!h.state().agent.as_ref().unwrap().state.open);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "AI Agent").click();
    h.run_steps(2);
    assert!(h.state().agent.as_ref().unwrap().state.open);
}

#[test]
fn view_agent_command_toggles_and_sets() {
    let (mut h, _) = harness(LinkStatus::Connected);
    h.state_mut().run("view.agent", json!({"on": false})).unwrap();
    assert!(!h.state().agent.as_ref().unwrap().state.open);
    h.state_mut().run("view.agent", json!({})).unwrap();
    assert!(h.state().agent.as_ref().unwrap().state.open);
}

#[test]
fn pane_leaves_room_for_the_slide_in_a_narrow_window() {
    let (mut h, _) = harness(LinkStatus::Connected);
    h.set_size(egui::vec2(700.0, 800.0));
    h.run_steps(4);
    let title = h.get_by_role_and_label(egui::accesskit::Role::Label, "AI Agent").rect();
    assert!(title.left() >= 700.0 * 0.55 - 12.0, "pane starts at {}", title.left());
}

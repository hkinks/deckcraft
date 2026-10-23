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
        providers: vec![ProviderInfo { id: "qwen-local".into(), model: "qwen3.8".into(), vision: false, label: String::new() }],
        default_provider: "qwen-local".into(),
        instructions: String::new(),
        default_instructions: String::new(),
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
fn the_deck_stays_editable_while_the_agent_works() {
    let (mut h, _wire) = harness(LinkStatus::Connected);
    let n = h.state().session.active().unwrap().doc.slides.len();
    client::send_prompt(h.state_mut(), "busy");
    h.run_steps(2);
    h.state_mut().run("slide.new", json!({})).unwrap();
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.slides.len(), n, "Ctrl+Z works during a turn");
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
    h.get_by_label("“add a closing slide”");
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

#[test]
fn new_chat_button_clears_the_chat() {
    let (mut h, wire) = after_turn("x");
    h.get_by_label("New chat").click();
    h.run_steps(2);
    assert!(h.state().agent.as_ref().unwrap().state.items.is_empty());
    assert_eq!(wire.sent_of("new_conversation").len(), 1);
}

#[test]
fn changes_tab_offers_restore_after_revert() {
    let (mut h, _) = after_turn("add a closing slide");
    let n = h.state().session.active().unwrap().doc.slides.len();
    h.get_by_label("Changes (1)").click();
    h.run_steps(2);
    h.get_by_label("Revert").click();
    h.run_steps(2);
    h.get_by_label("↺ reverted");
    h.get_by_label("Restore").click();
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.slides.len(), n);
}

#[test]
fn empty_chat_offers_suggestions_that_fill_the_prompt() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    h.get_by_label("Add an agenda slide").click();
    h.run_steps(2);
    assert_eq!(h.state().agent.as_ref().unwrap().state.input, "Add an agenda slide");
    assert!(wire.sent_of("user_message").is_empty(), "a suggestion fills the box, it doesn't send");
}

#[test]
fn working_strip_with_stop_sits_on_top_while_running() {
    let (mut h, _) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), "do it");
    h.run_steps(2);
    let strip = h.get_by_label("Agent is editing the deck…").rect();
    let stop = h.get_by_label("Stop").rect();
    let prompt = h.get_by_label("Queue").rect();
    assert!(strip.top() < prompt.top() - 200.0, "status strip is at the top, not by the prompt");
    assert!((stop.center().y - strip.center().y).abs() < 20.0, "Stop sits in the strip");
}

#[test]
fn header_shows_the_friendly_model_name() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    wire.push(ServerMsg::Ready {
        user: "ana".into(),
        providers: vec![ProviderInfo { id: "qwen-local".into(), model: "qwen3.8".into(), vision: false, label: "Qwen 3.8 (local)".into() }],
        default_provider: "qwen-local".into(),
        instructions: String::new(),
        default_instructions: String::new(),
    });
    h.run_steps(3);
    h.get_by_label("Qwen 3.8 (local)");
}

#[test]
fn pane_width_is_stable_while_the_pointer_moves_over_it() {
    let (mut h, _) = after_turn("add a slide");
    let x0 = h.get_by_role_and_label(egui::accesskit::Role::Label, "AI Agent").rect().left();
    for i in 0..40 {
        h.event(egui::Event::PointerMoved(egui::pos2(1300.0 + (i % 7) as f32, 400.0 + i as f32)));
        h.run_steps(1);
    }
    let x1 = h.get_by_role_and_label(egui::accesskit::Role::Label, "AI Agent").rect().left();
    assert!((x0 - x1).abs() < 0.5, "the pane grew from x={x0} to x={x1}");
}

#[test]
fn tabs_show_a_pointing_hand() {
    let (mut h, _) = harness(LinkStatus::Connected);
    for label in ["Chat", "Changes (0)"] {
        let c = h.get_by_label(label).rect().center();
        h.event(egui::Event::PointerMoved(c));
        h.run_steps(2);
        assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::PointingHand, "{label}");
    }
}

#[test]
fn every_symbol_the_pane_draws_is_in_the_ui_font() {
    let (h, _) = harness(LinkStatus::Connected);
    let missing: Vec<char> = super::pane::SYMBOLS
        .chars()
        .filter(|c| !h.ctx.fonts_mut(|f| f.has_glyph(&crate::theme::font(13.0), *c)))
        .collect();
    assert!(missing.is_empty(), "no glyph for {missing:?}");
}

#[test]
fn step_count_does_not_overlap_stop() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    h.set_size(egui::vec2(1100.0, 800.0));
    client::send_prompt(h.state_mut(), "many steps");
    for i in 0..13 {
        wire.push(ServerMsg::ToolCall { id: format!("t{i}"), name: "inspect_slide".into(), input: json!({}) });
    }
    h.run_steps(16);
    let stop = h.get_by_label("Stop").rect();
    // The strip's count (the chat's folded steps row has the same text further down).
    let steps = h.get_all_by_label("13 steps").map(|n| n.rect()).min_by(|a, b| (a.center().y - stop.center().y).abs().total_cmp(&(b.center().y - stop.center().y).abs())).unwrap();
    assert!(!stop.intersects(steps), "steps {steps:?} overlaps Stop {stop:?}");
}

#[test]
fn typing_while_the_agent_works_queues_the_request() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), "long job");
    h.run_steps(2);
    h.state_mut().agent.as_mut().unwrap().state.input = "then this".into();
    h.get_by_label("Queue").click();
    h.run_steps(2);
    assert_eq!(wire.sent_of("user_message").len(), 1);
    assert_eq!(client::queued(h.state()), vec!["then this".to_string()]);
    h.get_by_label("then this");
    h.get_by_label("Remove from queue").click();
    h.run_steps(2);
    assert!(client::queued(h.state()).is_empty());
}

#[test]
fn revert_confirmation_says_what_goes_and_how_to_get_it_back() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), "add a slide");
    wire.push(ServerMsg::ToolCall { id: "t1".into(), name: "run_command".into(), input: json!({"command": "slide.new", "params": {}}) });
    wire.push(ServerMsg::TurnEnd { turn_id: "turn-1".into(), summary: "done".into(), actions: vec![], slides: vec![], status: "ok".into() });
    h.run_steps(6);
    h.state_mut().run("slide.new", json!({})).unwrap();
    h.run_steps(2);
    h.get_by_label("Revert").click();
    h.run_steps(2);
    h.get_by_label("Revert “add a slide”?");
    assert!(h.query_all_by_label_contains("Since then: 1 edit you made.").next().is_some(), "names the later edit");
    assert!(h.query_all_by_label_contains("Undo brings everything back").next().is_some(), "says how to get it back");
    h.get_by_label("Revert anyway").click();
    h.run_steps(2);
    assert!(client::is_reverted(h.state(), 0));
}

#[test]
fn instructions_tab_edits_and_saves_the_agents_working_style() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    wire.push(ServerMsg::Ready {
        user: "ana".into(),
        providers: vec![ProviderInfo { id: "qwen-local".into(), model: "qwen3.8".into(), vision: false, label: String::new() }],
        default_provider: "qwen-local".into(),
        instructions: "Pick and apply.".into(),
        default_instructions: "Pick and apply.".into(),
    });
    h.run_steps(3);
    h.get_by_label("Instructions").click();
    h.run_steps(2);
    let field = h.get_by_label("Agent instructions");
    assert_eq!(field.value().as_deref(), Some("Pick and apply."));
    assert!(h.query_by_label("Save").is_none(), "nothing to save yet");
    h.state_mut().agent.as_mut().unwrap().state.instructions_draft = Some("Use British spelling.".into());
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    let sent = wire.sent_of("set_instructions");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["text"], "Use British spelling.");
    wire.push(ServerMsg::Instructions { text: "Use British spelling.".into() });
    h.run_steps(3);
    assert_eq!(h.get_by_label("Agent instructions").value().as_deref(), Some("Use British spelling."));
    h.get_by_label("Reset to default").click();
    h.run_steps(2);
    assert_eq!(wire.sent_of("set_instructions")[1]["text"], "");
}

fn with_files(h: &mut Harness<'static, SlideApp>, files: &[(&str, &[u8])]) -> crate::workspace_fake::FakeHost {
    let host = crate::workspace_fake::FakeHost::with(files);
    h.state_mut().set_workspace_host(Box::new(host.clone()));
    host
}

#[test]
fn files_tab_offers_to_open_a_folder() {
    let (mut h, _) = harness(LinkStatus::Connected);
    let host = with_files(&mut h, &[]);
    host.0.borrow_mut().name.clear();
    h.run_steps(2);
    h.get_by_label("Files").click();
    h.run_steps(2);
    h.get_by_label("Open folder…").click();
    h.run_steps(2);
    assert!(host.0.borrow().picked, "the folder picker opened");
}

#[test]
fn files_tab_lists_the_folder_and_acts_on_a_file() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    let host = with_files(&mut h, &[("AGENT.md", b"Audience: board"), ("brief.md", b"Ship in March"), ("img/logo.png", b"png"), ("brand/Corporate.potx", b"x")]);
    h.run_steps(2);
    h.get_by_label("Files").click();
    h.run_steps(2);
    h.get_by_label("Launch");
    h.get_by_label("brief.md");
    h.get_by_label("logo.png");
    // Ask the agent about a file: the prompt names it and the chat opens.
    h.get_by_label("brief.md").click();
    h.run_steps(2);
    h.get_by_label("Ask agent").click();
    h.run_steps(2);
    let input = h.state().agent.as_ref().unwrap().state.input.clone();
    assert!(input.contains("brief.md"), "{input}");
    assert_eq!(h.state().agent.as_ref().unwrap().state.tab, client::PaneTab::Chat);
    // Project instructions are read and go with the next request.
    host.0.borrow_mut().instant = true;
    h.run_steps(4);
    client::send_prompt(h.state_mut(), "draft the deck");
    let sent = wire.sent_of("user_message");
    assert_eq!(sent[0]["deck"]["workspace"]["instructions"], "Audience: board");
}

#[test]
fn files_tab_filter_narrows_the_list() {
    let (mut h, _) = harness(LinkStatus::Connected);
    with_files(&mut h, &[("brief.md", b"x"), ("data/q3.csv", b"y")]);
    h.run_steps(2);
    h.get_by_label("Files").click();
    h.run_steps(2);
    h.state_mut().agent.as_mut().unwrap().state.file_filter = "csv".into();
    h.run_steps(2);
    assert!(h.query_by_label("brief.md").is_none());
    h.get_by_label("q3.csv");
}

#[test]
fn a_failed_step_says_why_without_hovering() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    client::send_prompt(h.state_mut(), "do it");
    wire.push(ServerMsg::ToolCall { id: "t1".into(), name: "run_command".into(), input: json!({"command": "nope.nothing", "params": {}}) });
    wire.push(ServerMsg::TurnEnd { turn_id: "turn-1".into(), summary: "done".into(), actions: vec![], slides: vec![], status: "ok".into() });
    h.run_steps(6);
    h.get_by_label("1 step (1 failed)").click();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("unknown command").next().is_some());
}

#[test]
fn the_prompt_keeps_focus_after_enter() {
    let (mut h, wire) = harness(LinkStatus::Connected);
    let id = egui::Id::new("agent_prompt");
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.run_steps(1);
    h.state_mut().agent.as_mut().unwrap().state.input = "make it pop".into();
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert_eq!(wire.sent_of("user_message").len(), 1);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(id), "the field still has the focus");
    // Typing goes straight on: a follow-up is queued behind the running request.
    h.state_mut().agent.as_mut().unwrap().state.input = "then add a summary".into();
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(id));
    assert_eq!(client::queued(h.state()), vec!["then add a summary".to_string()]);
}

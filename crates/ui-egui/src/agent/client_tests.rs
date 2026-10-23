//! Agent turns end to end against a scripted server: tool calls run on the live deck, a turn is
//! one undo step, revert, disconnects and refusals.

use std::sync::Arc;

use deckcraft_engine::Session;
use serde_json::{Value, json};

use super::client::{self, RevertOutcome};
use super::protocol::{ProviderInfo, ServerMsg};
use super::test_support::{Fake, Wire};
use super::transport::LinkStatus;
use crate::{Services, SlideApp};

fn setup() -> (SlideApp, egui::Context, Wire) {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "title", "title": "Start"})).unwrap();
    let mut app = SlideApp::new(s, Services::default());
    let wire = Wire::default();
    app.set_agent_transport(Box::new(Fake(wire.clone())));
    let ctx = egui::Context::default();
    client::tick(&mut app, &ctx);
    assert_eq!(wire.sent_of("hello").len(), 1);
    wire.push(ServerMsg::Ready {
        user: "ana".into(),
        providers: vec![ProviderInfo { id: "q".into(), model: "qwen".into(), vision: false, label: String::new() }],
        default_provider: "q".into(),
    });
    client::tick(&mut app, &ctx);
    assert_eq!(wire.sent_of("open_conversation").len(), 1);
    wire.push(ServerMsg::History { conversation_id: None, items: vec![] });
    client::tick(&mut app, &ctx);
    (app, ctx, wire)
}

fn call(id: &str, command: &str, params: Value) -> ServerMsg {
    ServerMsg::ToolCall { id: id.into(), name: "run_command".into(), input: json!({"command": command, "params": params}) }
}

fn end(turn: &str) -> ServerMsg {
    ServerMsg::TurnEnd { turn_id: turn.into(), summary: "done".into(), actions: vec!["slide.new".into()], slides: vec![], status: "ok".into() }
}

fn run_until_idle(app: &mut SlideApp, ctx: &egui::Context) {
    for _ in 0..50 {
        client::tick(app, ctx);
        if !client::busy(app) {
            return;
        }
    }
    panic!("turn did not finish");
}

fn doc(app: &SlideApp) -> Arc<deckcraft_model::Presentation> {
    app.session.active().unwrap().doc.clone()
}

#[test]
fn each_agent_step_is_its_own_labelled_undo_step_and_revert_is_one() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    let slides = before.slides.len();
    client::send_prompt(&mut app, "add a slide\nand a box");
    assert!(client::busy(&app));
    let um = wire.sent_of("user_message");
    assert_eq!(um[0]["deck"]["slides"], slides);
    assert_eq!(um[0]["provider"], "q");
    let undo0 = app.session.active().unwrap().history.undo.len();
    wire.push(call("t1", "slide.new", json!({"layout": "title"})));
    wire.push(call("t2", "shape.insert", json!({"preset": "rect", "rect": [10, 10, 100, 60]})));
    client::tick(&mut app, &ctx);
    assert_eq!(wire.sent_of("tool_result").len(), 1, "one tool call per frame");
    client::tick(&mut app, &ctx);
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    let results = wire.sent_of("tool_result");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r["ok"] == true), "{results:?}");
    let undo = &app.session.active().unwrap().history.undo;
    assert_eq!(undo.len(), undo0 + 2, "each agent step is atomic");
    assert!(undo[undo0..].iter().all(|e| e.label.starts_with("AI: ")), "{:?}", undo.iter().map(|e| &e.label).collect::<Vec<_>>());
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
    assert!(Arc::ptr_eq(&doc(&app), &before), "Revert puts the whole request back in one step");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(doc(&app).slides.len(), slides + 1);
}

#[test]
fn user_edit_during_a_turn_is_its_own_step_and_the_agent_is_told() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "work on it");
    wire.push(call("t1", "slide.new", json!({})));
    client::tick(&mut app, &ctx);
    // The person adds a slide of their own while the agent works.
    app.run("slide.new", json!({"layout": "title"})).unwrap();
    let user_label = app.session.active().unwrap().history.undo.last().unwrap().label.clone();
    assert!(!user_label.starts_with("AI: "), "{user_label}");
    wire.push(call("t2", "shape.insert", json!({"preset": "rect", "rect": [1, 1, 50, 50]})));
    client::tick(&mut app, &ctx);
    let r = wire.sent_of("tool_result");
    let note = r[1]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
    assert!(note.contains("the user"), "{note}");
    assert!(note.contains("New Slide"), "{note}");
    let undo = &app.session.active().unwrap().history.undo;
    let labels: Vec<_> = undo.iter().rev().take(3).map(|e| e.label.clone()).collect();
    assert!(labels[0].starts_with("AI: ") && !labels[1].starts_with("AI: ") && labels[2].starts_with("AI: "), "{labels:?}");
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    assert_eq!(app.agent.as_ref().unwrap().state.turns[0].user_edits, 1);
}

#[test]
fn revert_of_a_turn_with_user_edits_asks_first() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    client::send_prompt(&mut app, "x");
    wire.push(call("t1", "slide.new", json!({})));
    client::tick(&mut app, &ctx);
    app.run("slide.new", json!({})).unwrap();
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::NeedsConfirm);
    assert_eq!(client::revert(&mut app, 0, true), RevertOutcome::Replaced);
    assert!(Arc::ptr_eq(&doc(&app), &before));
}

#[test]
fn revert_latest_turn_undoes() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    client::send_prompt(&mut app, "x");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
    assert!(Arc::ptr_eq(&doc(&app), &before));
}

#[test]
fn revert_older_turn_needs_confirmation_then_replaces_document() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    client::send_prompt(&mut app, "first");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    app.run("slide.new", json!({})).unwrap(); // a manual edit after the turn
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::NeedsConfirm);
    assert_eq!(client::revert(&mut app, 0, true), RevertOutcome::Replaced);
    assert_eq!(doc(&app).slides.len(), before.slides.len());
    app.run("edit.undo", json!({})).unwrap(); // the revert itself is undoable
    assert_eq!(doc(&app).slides.len(), before.slides.len() + 2);
}

#[test]
fn disconnect_mid_turn_unlocks_and_records_interrupted() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "do things");
    wire.push(call("t1", "slide.new", json!({})));
    client::tick(&mut app, &ctx);
    *wire.status.borrow_mut() = Some(LinkStatus::Failed("disconnected".into()));
    client::tick(&mut app, &ctx);
    assert!(!client::busy(&app));
    assert_eq!(app.session.active().unwrap().history.undo.last().unwrap().label, "AI: New Slide");
    assert_eq!(app.agent.as_ref().unwrap().state.turns.last().unwrap().status, "interrupted");
}

#[test]
fn refused_tool_reports_error_and_changes_nothing() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    client::send_prompt(&mut app, "save it");
    wire.push(ServerMsg::ToolCall { id: "t1".into(), name: "save_presentation".into(), input: json!({"path": "/tmp/x.pptx"}) });
    client::tick(&mut app, &ctx);
    let r = wire.sent_of("tool_result");
    assert_eq!(r[0]["ok"], false);
    assert!(Arc::ptr_eq(&doc(&app), &before));
}

#[test]
fn tool_call_after_switching_document_fails_cleanly() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "edit");
    app.run("file.new", json!({})).unwrap(); // another deck becomes active mid-turn
    let other = doc(&app);
    wire.push(call("t1", "slide.new", json!({})));
    client::tick(&mut app, &ctx);
    let r = wire.sent_of("tool_result");
    assert_eq!(r[0]["ok"], false);
    assert!(r[0]["content"][0]["text"].as_str().unwrap().contains("no longer"));
    assert!(Arc::ptr_eq(&doc(&app), &other));
}

#[test]
fn history_restores_chat_without_revert() {
    let (mut app, ctx, wire) = setup();
    wire.push(ServerMsg::History {
        conversation_id: Some("c1".into()),
        items: vec![
            super::protocol::HistoryItem { role: "user".into(), text: "hi".into(), turn: None },
            super::protocol::HistoryItem {
                role: "assistant".into(),
                text: "hello".into(),
                turn: Some(super::protocol::TurnInfo { turn_id: "tx".into(), actions: vec!["slide.new".into()], slides: vec![], status: "ok".into() }),
            },
        ],
    });
    client::tick(&mut app, &ctx);
    let st = &app.agent.as_ref().unwrap().state;
    assert_eq!(st.turns.len(), 1);
    assert!(st.turns[0].before.is_none());
    assert_eq!(client::revert(&mut app, 0, true), RevertOutcome::Unavailable);
}

#[test]
fn stop_sends_stop() {
    let (mut app, _ctx, wire) = setup();
    client::send_prompt(&mut app, "long job");
    client::stop(&mut app);
    assert_eq!(wire.sent_of("stop").len(), 1);
}

const PNG_1PX: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[test]
fn revert_of_an_older_turn_keeps_picture_bytes() {
    let (mut app, ctx, wire) = setup();
    app.run("insert.picture", json!({"data": PNG_1PX, "name": "dot.png"})).unwrap();
    let bytes = doc(&app).media[0].data.len();
    client::send_prompt(&mut app, "drop the media");
    wire.push(ServerMsg::ToolCall { id: "t1".into(), name: "patch_deck_json".into(), input: json!({"patch": [{"op": "replace", "path": "/media", "value": []}]}) });
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    app.run("slide.new", json!({})).unwrap();
    assert_eq!(client::revert(&mut app, 0, true), RevertOutcome::Replaced);
    assert_eq!(doc(&app).media.first().map(|m| m.data.len()), Some(bytes));
}

fn turn_start(id: &str) -> ServerMsg {
    ServerMsg::TurnStart { turn_id: id.into() }
}

#[test]
fn revert_survives_switching_decks_and_back() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "x");
    wire.push(turn_start("turn-1"));
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    let first = app.session.active_index().unwrap();
    app.run("file.new", json!({})).unwrap();
    client::tick(&mut app, &ctx);
    app.session.set_active(first);
    client::tick(&mut app, &ctx);
    wire.push(ServerMsg::History {
        conversation_id: Some("c".into()),
        items: vec![
            super::protocol::HistoryItem { role: "user".into(), text: "x".into(), turn: None },
            super::protocol::HistoryItem { role: "assistant".into(), text: "ok".into(), turn: Some(super::protocol::TurnInfo { turn_id: "turn-1".into(), actions: vec!["slide.new".into()], slides: vec![], status: "ok".into() }) },
        ],
    });
    client::tick(&mut app, &ctx);
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
}

#[test]
fn revert_survives_a_reconnect() {
    let (mut app, ctx, wire) = setup();
    let before = doc(&app);
    client::send_prompt(&mut app, "x");
    wire.push(turn_start("turn-9"));
    wire.push(call("t1", "slide.new", json!({})));
    client::tick(&mut app, &ctx);
    client::tick(&mut app, &ctx);
    *wire.status.borrow_mut() = Some(LinkStatus::Failed("disconnected".into()));
    client::tick(&mut app, &ctx);
    *wire.status.borrow_mut() = Some(LinkStatus::Connected);
    wire.push(ServerMsg::Ready { user: "ana".into(), providers: vec![ProviderInfo { id: "q".into(), model: "m".into(), vision: false, label: String::new() }], default_provider: "q".into() });
    client::tick(&mut app, &ctx);
    wire.push(ServerMsg::History {
        conversation_id: Some("c".into()),
        items: vec![
            super::protocol::HistoryItem { role: "user".into(), text: "x".into(), turn: None },
            super::protocol::HistoryItem { role: "assistant".into(), text: "(interrupted)".into(), turn: Some(super::protocol::TurnInfo { turn_id: "turn-9".into(), actions: vec![], slides: vec![], status: "interrupted".into() }) },
        ],
    });
    client::tick(&mut app, &ctx);
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
    assert!(Arc::ptr_eq(&doc(&app), &before));
}

#[test]
fn stop_without_an_answer_unlocks_after_the_grace_period() {
    let (mut app, ctx, _wire) = setup();
    app.agent.as_mut().unwrap().state.stop_grace_ms = 0.0;
    client::send_prompt(&mut app, "hang");
    client::stop(&mut app);
    client::tick(&mut app, &ctx);
    assert!(!client::busy(&app));
}

#[test]
fn new_conversation_clears_the_chat_and_tells_the_server() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "x");
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    client::new_conversation(&mut app);
    assert!(app.agent.as_ref().unwrap().state.items.is_empty());
    assert_eq!(app.agent.as_ref().unwrap().state.turns.len(), 1, "the change log stays");
    assert_eq!(wire.sent_of("new_conversation").len(), 1);
}

#[test]
fn turn_card_records_changed_slides_and_plain_language_edits() {
    let (mut app, ctx, wire) = setup();
    let n = doc(&app).slides.len();
    client::send_prompt(&mut app, "add one");
    wire.push(call("t1", "slide.new", json!({"layout": "title"})));
    wire.push(ServerMsg::TurnEnd { turn_id: "t".into(), summary: String::new(), actions: vec!["slide.new".into(), "format.bold".into(), "format.bold".into()], slides: vec![], status: "ok".into() });
    run_until_idle(&mut app, &ctx);
    let card = &app.agent.as_ref().unwrap().state.turns[0];
    assert_eq!(card.slides, vec![n], "the new slide is the changed one (0-based)");
    assert_eq!(card.added, 1);
    assert_eq!(card.removed, 0);
    assert_eq!(card.prompt, "add one");
    let bold = deckcraft_engine::cmd::find_command("format.bold").unwrap().label;
    let new = deckcraft_engine::cmd::find_command("slide.new").unwrap().label;
    assert_eq!(client::edit_labels(&card.actions), vec![new.to_string(), format!("{bold} ×2")]);
}

#[test]
fn new_chat_keeps_the_change_log() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "x");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    client::new_conversation(&mut app);
    let st = &app.agent.as_ref().unwrap().state;
    assert!(st.items.is_empty());
    assert_eq!(st.turns.len(), 1);
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
}

#[test]
fn reverted_turn_can_be_restored() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "x");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    let after = doc(&app);
    assert!(!client::is_reverted(&app, 0));
    assert_eq!(client::revert(&mut app, 0, false), RevertOutcome::Replaced);
    assert!(client::is_reverted(&app, 0));
    assert!(client::restore(&mut app, 0));
    assert!(Arc::ptr_eq(&doc(&app), &after));
    assert!(!client::is_reverted(&app, 0));
}

#[test]
fn restore_is_not_offered_once_the_deck_moved_on() {
    // Restoring then would silently throw away the later edit.
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "x");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    client::revert(&mut app, 0, false);
    app.run("slide.new", json!({})).unwrap();
    let edited = doc(&app);
    assert!(!client::is_reverted(&app, 0));
    assert!(!client::restore(&mut app, 0));
    assert!(Arc::ptr_eq(&doc(&app), &edited));
}

#[test]
fn agent_works_on_its_own_slide_without_moving_the_users_view() {
    let (mut app, ctx, wire) = setup();
    app.run("slide.go", json!({"index": 0})).unwrap();
    let mine = doc(&app).slides[0].id;
    let mine_shapes = doc(&app).slides[0].shapes.len();
    let other_shapes = doc(&app).slides[1].shapes.len();
    client::send_prompt(&mut app, "put a box on slide 2");
    wire.push(call("t1", "slide.go", json!({"index": 1})));
    client::tick(&mut app, &ctx);
    assert_eq!(app.session.active().unwrap().selection.slide, 0, "the person's view stays put");
    // The agent's next step works where the agent is, not where the person is.
    wire.push(call("t2", "shape.insert", json!({"preset": "rect", "rect": [10, 10, 100, 60]})));
    client::tick(&mut app, &ctx);
    assert_eq!(doc(&app).slides[0].shapes.len(), mine_shapes, "nothing landed on the person's slide");
    assert_eq!(doc(&app).slides[1].shapes.len(), other_shapes + 1);
    assert_eq!(app.session.active().unwrap().selection.slide, 0);
    // A slide inserted in front of the person's: their view follows their slide.
    wire.push(call("t3", "slide.new", json!({"at": 0})));
    client::tick(&mut app, &ctx);
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    let a = app.session.active().unwrap();
    assert_eq!(a.doc.slides.len(), 3);
    assert_eq!(a.doc.slides[a.selection.slide].id, mine, "still on the slide they were on");
}

#[test]
fn requests_queued_while_the_agent_works_are_sent_in_order() {
    let (mut app, ctx, wire) = setup();
    client::queue_prompt(&mut app, "first");
    client::queue_prompt(&mut app, "second");
    client::queue_prompt(&mut app, "third");
    assert_eq!(wire.sent_of("user_message").len(), 1, "only the first goes out right away");
    assert_eq!(client::queued(&app), vec!["second".to_string(), "third".to_string()]);
    wire.push(end("turn-1"));
    client::tick(&mut app, &ctx);
    client::tick(&mut app, &ctx);
    let sent = wire.sent_of("user_message");
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["text"], "second");
    assert!(client::busy(&app), "the next request is running");
    client::remove_queued(&mut app, 0);
    wire.push(end("turn-2"));
    run_until_idle(&mut app, &ctx);
    client::tick(&mut app, &ctx);
    assert_eq!(wire.sent_of("user_message").len(), 2, "a removed request isn't sent");
    assert!(client::queued(&app).is_empty());
}

#[test]
fn a_stopped_turn_pauses_the_queue_until_resumed() {
    let (mut app, ctx, wire) = setup();
    client::queue_prompt(&mut app, "first");
    client::queue_prompt(&mut app, "second");
    wire.push(ServerMsg::TurnEnd { turn_id: "turn-1".into(), summary: String::new(), actions: vec![], slides: vec![], status: "stopped".into() });
    run_until_idle(&mut app, &ctx);
    client::tick(&mut app, &ctx);
    assert_eq!(wire.sent_of("user_message").len(), 1, "after a stop nothing more is sent by itself");
    assert!(client::queue_paused(&app));
    client::resume_queue(&mut app);
    client::tick(&mut app, &ctx);
    let sent = wire.sent_of("user_message");
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["text"], "second");
}

#[test]
fn revert_confirmation_names_what_else_would_be_lost() {
    let (mut app, ctx, wire) = setup();
    client::send_prompt(&mut app, "first");
    wire.push(call("t1", "slide.new", json!({})));
    wire.push(end("turn-1"));
    run_until_idle(&mut app, &ctx);
    // Two edits of the person's own, then another request.
    app.run("slide.new", json!({})).unwrap();
    app.run("slide.new", json!({})).unwrap();
    client::send_prompt(&mut app, "second");
    wire.push(call("t2", "slide.new", json!({})));
    wire.push(end("turn-2"));
    run_until_idle(&mut app, &ctx);
    assert_eq!(client::later_changes(&app, 0), Some(client::LaterChanges { your_edits: 2, requests: 1 }));
    assert_eq!(client::later_changes(&app, 0).unwrap().describe(), "2 edits you made and 1 later agent request");
    assert_eq!(client::later_changes(&app, 1), Some(client::LaterChanges { your_edits: 0, requests: 0 }));
}

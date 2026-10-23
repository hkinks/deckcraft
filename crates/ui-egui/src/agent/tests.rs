//! The agent core without a network: protocol decoding, the tool allow-list and running tools on
//! a live `SlideApp`.

use deckcraft_engine::Session;
use serde_json::json;

use super::protocol::{self, ContentPart, ServerMsg};
use super::tools;
use crate::{Services, SlideApp};

fn app_with_deck() -> (SlideApp, egui::Context) {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "title", "title": "Hello"})).unwrap();
    (SlideApp::new(s, Services::default()), egui::Context::default())
}

#[test]
fn check_refuses_file_tools_and_paths() {
    assert!(tools::check("save_presentation", &json!({})).is_err());
    assert!(tools::check("open_presentation", &json!({})).is_err());
    assert!(tools::check("screenshot", &json!({})).is_err());
    assert!(tools::check("run_command", &json!({"command": "file.save", "params": {}})).is_err());
    assert!(tools::check("run_command", &json!({"command": "app.quit"})).is_err());
    assert!(tools::check("run_command", &json!({"command": "insert.picture", "params": {"path": "/etc/passwd"}})).is_err());
    assert!(tools::check("batch", &json!({"commands": [{"command": "slide.new"}, {"command": "file.open", "params": {}}]})).is_err());
    assert!(tools::check("render_slide", &json!({"path": "/tmp/x.png"})).is_err());
    assert!(tools::check("run_command", &json!({})).is_err());
    assert!(tools::check("run_command", &json!({"command": "slide.new", "params": {}})).is_ok());
    assert!(tools::check("patch_deck_json", &json!({"patch": []})).is_ok());
}

#[test]
fn advertised_tools_are_exactly_the_allowed_ones() {
    let names: Vec<String> = tools::advertised().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"run_command".to_string()));
    assert!(names.contains(&"get_deck_json".to_string()));
    assert!(!names.iter().any(|n| n == "save_presentation" || n == "call_app" || n == "screenshot"));
    assert!(tools::advertised().iter().all(|t| t.get("inputSchema").is_some()));
}

#[test]
fn execute_runs_on_the_live_session() {
    let (mut app, ctx) = app_with_deck();
    let n = app.session.active().unwrap().doc.slides.len();
    let (ok, parts) = tools::execute(&mut app, &ctx, "run_command", &json!({"command": "slide.new", "params": {"layout": "title"}}), false);
    assert!(ok, "{parts:?}");
    assert_eq!(app.session.active().unwrap().doc.slides.len(), n + 1);
}

#[test]
fn refused_tool_changes_nothing() {
    let (mut app, ctx) = app_with_deck();
    let before = app.session.active().unwrap().doc.clone();
    let (ok, parts) = tools::execute(&mut app, &ctx, "run_command", &json!({"command": "file.new"}), false);
    assert!(!ok);
    assert!(matches!(&parts[0], ContentPart::Text { text } if text.contains("not allowed")));
    assert!(std::sync::Arc::ptr_eq(&app.session.active().unwrap().doc, &before));
}

#[test]
fn execute_without_document_is_an_error_not_a_panic() {
    let mut app = SlideApp::new(Session::new(), Services::default());
    let ctx = egui::Context::default();
    for (name, input) in [
        ("inspect_document", json!({})),
        ("inspect_slide", json!({})),
        ("render_slide", json!({})),
        ("run_command", json!({"command": "slide.new"})),
        ("get_deck_json", json!({})),
    ] {
        let (ok, _) = tools::execute(&mut app, &ctx, name, &input, true);
        assert!(!ok, "{name} should fail without a document");
    }
}

#[test]
fn render_slide_image_only_with_vision() {
    let (mut app, ctx) = app_with_deck();
    let (ok, parts) = tools::execute(&mut app, &ctx, "render_slide", &json!({"slide": 0}), true);
    assert!(ok);
    assert!(parts.iter().any(|p| matches!(p, ContentPart::Image { media_type, .. } if media_type == "image/png")));
    let (ok, parts) = tools::execute(&mut app, &ctx, "render_slide", &json!({"slide": 0}), false);
    assert!(ok);
    assert!(!parts.iter().any(|p| matches!(p, ContentPart::Image { .. })));
}

#[test]
fn decode_refuses_oversized_and_unknown() {
    assert!(protocol::decode(&"x".repeat(protocol::MAX_FRAME + 1)).is_err());
    assert!(protocol::decode(r#"{"type":"nope"}"#).is_err());
    assert!(protocol::decode("not json").is_err());
    assert!(matches!(protocol::decode(r#"{"type":"assistant_delta","text":"hi"}"#), Ok(ServerMsg::AssistantDelta { text }) if text == "hi"));
    let call = protocol::decode(r#"{"type":"tool_call","id":"t1","name":"run_command","input":{"command":"slide.new"}}"#).unwrap();
    assert!(matches!(call, ServerMsg::ToolCall { ref name, .. } if name == "run_command"));
    let ready = protocol::decode(r#"{"type":"ready","user":"ana","providers":[{"id":"q","model":"m","vision":false}],"default_provider":"q"}"#).unwrap();
    assert!(matches!(ready, ServerMsg::Ready { ref providers, .. } if providers.len() == 1));
}

#[test]
fn client_messages_encode_with_type_tags() {
    let m = protocol::ClientMsg::ToolResult { v: 1, id: "t1".into(), ok: true, content: vec![ContentPart::Text { text: "ok".into() }] };
    let v: serde_json::Value = serde_json::from_str(&protocol::encode(&m)).unwrap();
    assert_eq!(v["type"], "tool_result");
    assert_eq!(v["content"][0]["type"], "text");
}

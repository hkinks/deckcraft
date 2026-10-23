//! The agent's tools: which MCP tools it may use, and running a call on the live editor.
//!
//! The browser is the authority on what an agent may do: anything that reads or writes files, or
//! drives the UI rather than the document, is refused here whatever the server sends.

use deckcraft_mcp::{Backend, call_tool};
use serde_json::{Value, json};

use super::protocol::ContentPart;
use crate::SlideApp;
use crate::control::{self, ControlRequest, Outcome};

/// MCP tools the agent gets (see the design spec §3.3).
pub const ALLOWED_TOOLS: &[&str] = &[
    "list_commands",
    "run_command",
    "batch",
    "inspect_document",
    "inspect_slide",
    "render_slide",
    "add_slide",
    "go_to_slide",
    "add_shape",
    "add_text_box",
    "set_text",
    "insert_table",
    "insert_chart",
    "select",
    "get_deck_json",
    "patch_deck_json",
];

/// Command families that reach outside the open deck (files, the app, the UI, other windows).
const REFUSED_COMMAND_PREFIXES: &[&str] = &["file.", "app.", "ui.", "show.", "window."];
/// The agent's turn is itself the undo unit; undoing from inside it would reach the user's steps.
const REFUSED_COMMANDS: &[&str] = &["edit.undo", "edit.redo"];

/// Largest text part handed back to the model.
const MAX_TEXT: usize = 200 * 1024;

pub fn advertised() -> Vec<Value> {
    deckcraft_mcp::tool_definitions()
        .into_iter()
        .filter(|t| t.get("name").and_then(Value::as_str).is_some_and(|n| ALLOWED_TOOLS.contains(&n)))
        .map(|t| json!({"name": t["name"], "description": t["description"], "inputSchema": t["inputSchema"]}))
        .chain(super::workspace_tools::definitions())
        .collect()
}

fn check_command(c: &Value) -> Result<(), String> {
    let id = c.get("command").and_then(Value::as_str).ok_or("missing `command`")?;
    if REFUSED_COMMAND_PREFIXES.iter().any(|p| id.starts_with(p)) || REFUSED_COMMANDS.contains(&id) {
        return Err(format!("command `{id}` is not allowed for the agent: it can only change the open deck"));
    }
    if c.get("params").and_then(|p| p.get("path")).is_some() {
        return Err(format!("`{id}`: file paths are not allowed for the agent; pass data instead"));
    }
    Ok(())
}

/// Whether the agent may make this call.
pub fn check(name: &str, input: &Value) -> Result<(), String> {
    // Workspace tools read the person's workspace by path (relative, listed; never the disk).
    if super::workspace_tools::is_workspace_tool(name) {
        return Ok(());
    }
    if !ALLOWED_TOOLS.contains(&name) {
        return Err(format!("tool `{name}` is not allowed for the agent"));
    }
    // `path` names a file everywhere except in the deck JSON tools, where it is a JSON Pointer.
    if input.get("path").is_some() && !matches!(name, "get_deck_json" | "patch_deck_json") {
        return Err(format!("`{name}`: file paths are not allowed for the agent"));
    }
    match name {
        "run_command" => check_command(input),
        "batch" => {
            let cmds = input.get("commands").and_then(Value::as_array).ok_or("batch: missing `commands`")?;
            cmds.iter().try_for_each(check_command)
        }
        _ => Ok(()),
    }
}

/// [`deckcraft_mcp::Backend`] over the running app: the same dispatcher the desktop control
/// channel uses, so agent edits go through `Session::execute` like the ribbon's.
struct LiveBackend<'a> {
    app: &'a mut SlideApp,
    ctx: &'a egui::Context,
    /// The document the turn edits: every command checks it is still the active one.
    uid: Option<u64>,
}

impl Backend for LiveBackend<'_> {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let Some(active) = self.app.session.active().map(|d| d.uid) else {
            return Err("no presentation is open in the editor".into());
        };
        if self.uid.is_some_and(|u| u != active) {
            return Err("the deck this turn was editing is no longer the active document".into());
        }
        let (req, _rx) = ControlRequest::new(method, params);
        // The agent hears about its own failures in the tool result; the person's status bar is
        // theirs.
        let status = self.app.status.clone();
        let outcome = control::handle(self.app, self.ctx, &req);
        self.app.status = status;
        match outcome {
            Outcome::Done(v) => {
                if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(v.get("error").and_then(Value::as_str).unwrap_or("failed").to_string())
                }
            }
            Outcome::Screenshot { .. } => Err("screenshots are not available to the agent".into()),
        }
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "the editor in this browser tab".into()
    }
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n…(truncated; narrow the request)");
    }
    s
}

/// Run one tool call on the live session. `vision`: whether rendered images go back to the model.
pub fn execute(app: &mut SlideApp, ctx: &egui::Context, name: &str, input: &Value, vision: bool) -> (bool, Vec<ContentPart>) {
    execute_on(app, ctx, None, name, input, vision)
}

/// [`execute`], refusing every command once document `uid` is no longer the active one.
pub fn execute_on(app: &mut SlideApp, ctx: &egui::Context, uid: Option<u64>, name: &str, input: &Value, vision: bool) -> (bool, Vec<ContentPart>) {
    if let Err(e) = check(name, input) {
        return (false, vec![ContentPart::Text { text: e }]);
    }
    let r = call_tool(&mut LiveBackend { app, ctx, uid }, name, input);
    let mut parts = vec![];
    for c in r.content {
        match c.get("type").and_then(Value::as_str) {
            Some("image") if vision => parts.push(ContentPart::Image {
                media_type: c.get("mimeType").and_then(Value::as_str).unwrap_or("image/png").to_string(),
                data: c.get("data").and_then(Value::as_str).unwrap_or_default().to_string(),
            }),
            Some("image") => parts.push(ContentPart::Text { text: "(rendered image omitted: this model has no vision; use inspect_slide)".into() }),
            _ => parts.push(ContentPart::Text { text: clip(c.get("text").and_then(Value::as_str).unwrap_or_default().to_string()) }),
        }
    }
    (!r.is_error, parts)
}

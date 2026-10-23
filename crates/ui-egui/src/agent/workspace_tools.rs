//! The agent's workspace tools: list and read the person's workspace files, insert a picture from
//! it, apply a template's design. They run in the editor like the other tools; a file that isn't
//! read yet makes the tool wait (the client asks the host, then runs the tool again).

use serde_json::{Value, json};

use super::protocol::ContentPart;
use super::tools;
use crate::SlideApp;
use crate::workspace::{Kind, MAX_FILE_BYTES, WsStatus, kind_of, mime_of, size_label, text_of};

pub const NAMES: &[&str] = &["list_workspace_files", "read_workspace_file", "insert_workspace_image", "apply_workspace_template"];

pub fn is_workspace_tool(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_workspace_files",
            "description": "List the files in the user's workspace folder (their materials: briefs, notes, data, pictures, draft decks, style templates), with sizes. Optionally filter by a part of the path.",
            "inputSchema": {"type": "object", "properties": {"filter": {"type": "string", "description": "Only paths containing this text (case-insensitive)"}}}
        }),
        json!({
            "name": "read_workspace_file",
            "description": "Read a workspace file: text, Markdown, CSV and JSON as text; a deck (.pptx/.potx/.deckcraft) as an outline of its slides; a Word document's paragraphs; a PDF's text; a picture as an image (when you can see images).",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string", "description": "Path as listed by list_workspace_files"}, "maxChars": {"type": "integer", "description": "Longest text returned (default 30000)"}}, "required": ["path"]}
        }),
        json!({
            "name": "insert_workspace_image",
            "description": "Insert a picture from the workspace on a slide. Position and size in points are optional (default: centred, natural size).",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "slide": {"type": "integer", "description": "Slide index (default: the slide you're on)"}, "x": {"type": "number"}, "y": {"type": "number"}, "w": {"type": "number"}, "h": {"type": "number"}}, "required": ["path"]}
        }),
        json!({
            "name": "apply_workspace_template",
            "description": "Apply the design (theme colours, fonts, effects, background styles) of a template or deck in the workspace (.potx, .pptx, .thmx, .deckcraft) to this deck. The slides' content stays.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}
        }),
    ]
}

/// What a tool needs before it can run.
pub enum Need {
    /// Nothing: run it.
    Nothing,
    /// The file at this path, read by the host.
    File(String),
}

fn path_arg(input: &Value) -> Option<String> {
    input.get("path").and_then(Value::as_str).map(|p| p.trim().trim_start_matches("./").trim_start_matches('/').to_string()).filter(|p| !p.is_empty())
}

/// The file `name` must have read before it runs (None when it's missing, unlisted or too big:
/// the tool then reports that itself).
pub fn needs(app: &SlideApp, name: &str, input: &Value) -> Need {
    if name == "list_workspace_files" {
        return Need::Nothing;
    }
    let (Some(ws), Some(path)) = (app.workspace(), path_arg(input)) else { return Need::Nothing };
    if ws.cache.contains_key(&path) {
        return Need::Nothing;
    }
    match ws.host.files().iter().find(|f| f.path == path) {
        Some(f) if f.size <= MAX_FILE_BYTES => Need::File(path),
        _ => Need::Nothing,
    }
}

fn text(s: impl Into<String>) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: s.into() }]
}

/// Run a workspace tool (its file, if it needs one, is in the cache).
pub fn run(app: &mut SlideApp, ctx: &egui::Context, uid: Option<u64>, name: &str, input: &Value, vision: bool) -> (bool, Vec<ContentPart>) {
    let Some(ws) = app.workspace() else {
        return (false, text("No workspace folder is open. The user can open one in the agent pane's Files tab."));
    };
    let folder = match ws.host.status() {
        WsStatus::Ready { name } => name,
        WsStatus::NeedsPermission { name } => {
            return (false, text(format!("The workspace folder “{name}” needs the user's OK again: ask them to click Reconnect in the Files tab.")));
        }
        WsStatus::Loading { .. } => return (false, text("The workspace folder is still being listed; try again in a moment.")),
        WsStatus::None | WsStatus::Failed(_) => {
            return (false, text("No workspace folder is open. The user can open one in the agent pane's Files tab."));
        }
    };
    let files = ws.host.files();
    if name == "list_workspace_files" {
        let filter = input.get("filter").and_then(Value::as_str).unwrap_or("").to_lowercase();
        let shown: Vec<String> = files
            .iter()
            .filter(|f| filter.is_empty() || f.path.to_lowercase().contains(&filter))
            .take(500)
            .map(|f| format!("{} ({})", f.path, size_label(f.size)))
            .collect();
        let head = format!("Workspace “{folder}”: {} file(s){}", files.len(), if filter.is_empty() { String::new() } else { format!(", {} matching “{filter}”", shown.len()) });
        return (true, text(format!("{head}\n{}", shown.join("\n"))));
    }
    let Some(path) = path_arg(input) else { return (false, text(format!("{name} needs a `path`"))) };
    let Some(entry) = files.iter().find(|f| f.path == path) else {
        return (false, text(format!("no file {path:?} in the workspace; list_workspace_files shows the paths")));
    };
    if entry.size > MAX_FILE_BYTES {
        return (false, text(format!("{path} is {}; the workspace reads files up to {}", size_label(entry.size), size_label(MAX_FILE_BYTES))));
    }
    let bytes = match ws.cache.get(&path) {
        Some(Ok(b)) => b.clone(),
        Some(Err(e)) => return (false, text(format!("couldn't read {path}: {e}"))),
        None => return (false, text(format!("{path} hasn't been read"))),
    };
    let kind = kind_of(&path);
    let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
    match name {
        "read_workspace_file" => {
            let max = input.get("maxChars").and_then(Value::as_u64).map_or(30_000, |n| n.clamp(500, 200_000)) as usize;
            if let Some(t) = text_of(&path, &bytes) {
                return match t {
                    Ok(t) => (true, text(clip_chars(&t, max))),
                    Err(e) => (false, text(format!("couldn't read {path}: {e}"))),
                };
            }
            // Sent over the agent connection (16 MiB frames, base64): pictures for the model and
            // PDFs for the server stay well under that.
            let limit = match kind {
                Kind::Image => 4 << 20,
                Kind::Pdf => 10 << 20,
                _ => u64::MAX,
            };
            if entry.size > limit && (kind == Kind::Pdf || vision) {
                return (false, text(format!("{path} is {}: too big to read here (limit {})", size_label(entry.size), size_label(limit))));
            }
            match kind {
                Kind::Image if vision && mime_of(&path) != "image/svg+xml" => {
                    (true, vec![ContentPart::Image { media_type: mime_of(&path).into(), data: deckcraft_engine::cmd::base64_encode(&bytes) }])
                }
                Kind::Image => (true, text(format!("{path}: a picture ({}). This model can't see pictures; insert_workspace_image places it.", size_label(entry.size)))),
                // The server turns the PDF into text for the model.
                Kind::Pdf => (true, vec![ContentPart::File { name: file_name, media_type: "application/pdf".into(), data: deckcraft_engine::cmd::base64_encode(&bytes) }]),
                Kind::Theme => (true, text(format!("{path}: a theme file; apply_workspace_template applies it."))),
                _ => (false, text(format!("{path}: can't read this kind of file"))),
            }
        }
        "insert_workspace_image" => {
            if kind != Kind::Image {
                return (false, text(format!("{path} isn't a picture")));
            }
            if let Some(i) = input.get("slide").and_then(Value::as_u64) {
                let (ok, c) = tools::execute_on(app, ctx, uid, "run_command", &json!({"command": "slide.go", "params": {"index": i}}), false);
                if !ok {
                    return (false, c);
                }
            }
            let mut params = json!({"data": deckcraft_engine::cmd::base64_encode(&bytes), "name": file_name});
            let num = |k: &str| input.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
            if let Some(rect) = fit(pixel_size(&bytes), num("x"), num("y"), num("w"), num("h")) {
                params["rect"] = json!(rect);
            }
            tools::execute_on(app, ctx, uid, "run_command", &json!({"command": "insert.picture", "params": params}), false)
        }
        "apply_workspace_template" => {
            if !matches!(kind, Kind::Deck | Kind::Theme) {
                return (false, text(format!("{path} isn't a template, deck or theme")));
            }
            tools::execute_on(app, ctx, uid, "run_command", &json!({"command": "design.browseTheme", "params": {"data": deckcraft_engine::cmd::base64_encode(&bytes)}}), false)
        }
        _ => (false, text(format!("unknown workspace tool {name}"))),
    }
}

/// A picture's size in pixels (only its proportions matter here).
fn pixel_size(bytes: &[u8]) -> Option<(f64, f64)> {
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok().map(|(w, h)| (w as f64, h as f64))
}

/// Where a picture goes: inside the asked box at the picture's own proportions (centred), or
/// with the missing side following from them. None leaves it to the editor (natural size).
fn fit(natural: Option<(f64, f64)>, x: Option<f64>, y: Option<f64>, w: Option<f64>, h: Option<f64>) -> Option<[f64; 4]> {
    let (x, y) = (x?, y?);
    let aspect = natural.filter(|(nw, nh)| *nw > 0.0 && *nh > 0.0).map(|(nw, nh)| nw / nh);
    let (w, h) = match (w.filter(|v| *v > 0.0), h.filter(|v| *v > 0.0), aspect) {
        (Some(w), Some(h), Some(a)) => {
            let (fw, fh) = if w / h > a { (h * a, h) } else { (w, w / a) };
            return Some([x + (w - fw) / 2.0, y + (h - fh) / 2.0, fw, fh]);
        }
        (Some(w), Some(h), None) => (w, h),
        (Some(w), None, Some(a)) => (w, w / a),
        (None, Some(h), Some(a)) => (h * a, h),
        _ => {
            let (nw, nh) = natural.unwrap_or((320.0, 180.0));
            (w.unwrap_or(nw), h.unwrap_or(nh))
        }
    };
    Some([x, y, w, h])
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n[cut at {max} characters; ask for more with maxChars]")
}

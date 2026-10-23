//! The document model as JSON, for agents: read it (`doc.json`) and change it with an RFC 6902
//! JSON Patch (`doc.patch`) or wholesale (`doc.replace`). The escape hatch for edits no command
//! expresses; every change is validated the way an opened `.deckcraft` file is.

use std::collections::HashMap;
use std::sync::Arc;

use deckcraft_model::{MediaId, Presentation};
use serde_json::{Value, json};

use super::*;
use crate::{EngineError, Result, Session};

/// Default and largest `maxBytes` for `doc.json`.
const DEFAULT_MAX_BYTES: u64 = 256 * 1024;
const MAX_MAX_BYTES: u64 = 4 << 20;
/// Largest document JSON a patch or replace may produce.
const MAX_DOC_BYTES: usize = 64 << 20;
const MAX_PATCH_OPS: usize = 10_000;
const MAX_SLIDES: usize = 10_000;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "doc.json",
            "Document JSON",
            [],
            None,
            "{pointer?: JSON Pointer (e.g. /slides/0), maxBytes?: int} → {json, bytes, truncated}: the presentation model as JSON (media bytes omitted)",
            has_doc,
            doc_json
        ),
        cmd!(
            "doc.patch",
            "Edit Document",
            [],
            None,
            "{patch: [RFC 6902 operations: add|remove|replace|move|copy|test]} applied to the doc.json model; validated like an opened file",
            has_doc,
            doc_patch
        ),
        cmd!("doc.replace", "Replace Document", [], None, "{json: presentation model} — replaces the whole document (validated)", has_doc, doc_replace),
    ]
}

fn doc_value(s: &Session) -> Result<Value> {
    let doc = s.doc()?.doc.clone();
    serde_json::to_value(&*doc).map_err(|e| EngineError::Other(format!("document to JSON: {e}")))
}

fn doc_json(s: &mut Session, p: &Value) -> Result<Value> {
    let full = doc_value(s)?;
    let v = match str_param(p, "pointer") {
        Some(ptr) if !ptr.is_empty() => full.pointer(ptr).cloned().ok_or_else(|| bad("doc.json", format!("nothing at pointer {ptr}")))?,
        _ => full,
    };
    let max = p.get("maxBytes").and_then(Value::as_u64).unwrap_or(DEFAULT_MAX_BYTES).clamp(64, MAX_MAX_BYTES) as usize;
    let text = serde_json::to_string(&v).map_err(|e| EngineError::Other(e.to_string()))?;
    let bytes = text.len();
    if bytes <= max {
        return Ok(json!({"json": v, "bytes": bytes, "truncated": false}));
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let preview = text.get(..cut).unwrap_or_default().to_string();
    Ok(json!({"json": preview, "bytes": bytes, "truncated": true, "hint": "narrow with `pointer` (e.g. /slides/0/shapes) or raise maxBytes"}))
}

fn doc_patch(s: &mut Session, p: &Value) -> Result<Value> {
    let ops = p.get("patch").and_then(Value::as_array).ok_or_else(|| bad("doc.patch", "`patch` must be an array of operations"))?;
    if ops.len() > MAX_PATCH_OPS {
        return Err(bad("doc.patch", format!("at most {MAX_PATCH_OPS} operations")));
    }
    let mut v = doc_value(s)?;
    for (i, op) in ops.iter().enumerate() {
        apply_op(&mut v, op).map_err(|e| bad("doc.patch", format!("operation {i}: {e}")))?;
    }
    let slides = replace_with(s, v, "doc.patch")?;
    Ok(json!({"ok": true, "slides": slides}))
}

fn doc_replace(s: &mut Session, p: &Value) -> Result<Value> {
    let v = p.get("json").cloned().ok_or_else(|| bad("doc.replace", "missing `json`"))?;
    let slides = replace_with(s, v, "doc.replace")?;
    Ok(json!({"ok": true, "slides": slides}))
}

/// Validate `v` as a presentation (as the `.deckcraft` reader does), keep the media bytes the
/// JSON can't carry, and make it the document.
fn replace_with(s: &mut Session, v: Value, cmd: &str) -> Result<usize> {
    let size = serde_json::to_vec(&v).map(|b| b.len()).unwrap_or(usize::MAX);
    if size > MAX_DOC_BYTES {
        return Err(bad(cmd, "the resulting document is too large"));
    }
    let mut new: Presentation = serde_json::from_value(v).map_err(|e| bad(cmd, format!("not a valid presentation: {e}")))?;
    if new.slides.len() > MAX_SLIDES {
        return Err(bad(cmd, format!("at most {MAX_SLIDES} slides")));
    }
    let old = s.doc()?.doc.clone();
    let bytes: HashMap<MediaId, Arc<Vec<u8>>> = old.media.iter().map(|m| (m.id, m.data.clone())).collect();
    for m in &mut new.media {
        if m.data.is_empty()
            && let Some(d) = bytes.get(&m.id)
        {
            m.data = d.clone();
        }
    }
    deckcraft_format::repair(&mut new);
    deckcraft_format::sane_numbers(&mut new);
    let slides = new.slides.len();
    s.edit(|doc, sel| {
        *doc = new;
        sel.shapes.clear();
        sel.text = None;
        sel.cells = None;
        sel.slide = sel.slide.min(slides.saturating_sub(1));
        Ok(())
    })?;
    Ok(slides)
}

// RFC 6902 over serde_json values.

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn split(path: &str) -> std::result::Result<(String, String), String> {
    if path.is_empty() {
        return Err("the whole document can't be the target; use doc.replace".into());
    }
    if !path.starts_with('/') {
        return Err(format!("path {path:?} must start with /"));
    }
    let i = path.rfind('/').unwrap_or(0);
    Ok((path.get(..i).unwrap_or_default().to_string(), unescape(path.get(i + 1..).unwrap_or_default())))
}

fn parent_mut<'a>(root: &'a mut Value, parent: &str) -> std::result::Result<&'a mut Value, String> {
    root.pointer_mut(parent).ok_or_else(|| format!("no parent at {parent:?}"))
}

fn index(key: &str, len: usize, allow_end: bool) -> std::result::Result<usize, String> {
    if allow_end && key == "-" {
        return Ok(len);
    }
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{key:?} is not an array index"));
    }
    let i: usize = key.parse().map_err(|_| format!("{key:?} is not an array index"))?;
    let max = if allow_end { len } else { len.saturating_sub(1) };
    if i > max || (!allow_end && len == 0) {
        return Err(format!("index {i} out of range (length {len})"));
    }
    Ok(i)
}

fn add(root: &mut Value, path: &str, value: Value) -> std::result::Result<(), String> {
    let (parent, key) = split(path)?;
    match parent_mut(root, &parent)? {
        Value::Object(m) => {
            m.insert(key, value);
            Ok(())
        }
        Value::Array(a) => {
            let i = index(&key, a.len(), true)?;
            a.insert(i, value);
            Ok(())
        }
        _ => Err(format!("{parent:?} is not an object or array")),
    }
}

fn remove(root: &mut Value, path: &str) -> std::result::Result<Value, String> {
    let (parent, key) = split(path)?;
    match parent_mut(root, &parent)? {
        Value::Object(m) => m.remove(&key).ok_or_else(|| format!("nothing at {path:?}")),
        Value::Array(a) => {
            let i = index(&key, a.len(), false)?;
            Ok(a.remove(i))
        }
        _ => Err(format!("{parent:?} is not an object or array")),
    }
}

fn apply_op(root: &mut Value, op: &Value) -> std::result::Result<(), String> {
    let name = op.get("op").and_then(Value::as_str).ok_or("missing `op`")?;
    let path = op.get("path").and_then(Value::as_str).ok_or("missing `path`")?;
    let value = || op.get("value").cloned().ok_or_else(|| "missing `value`".to_string());
    let from = || op.get("from").and_then(Value::as_str).ok_or_else(|| "missing `from`".to_string());
    match name {
        "add" => add(root, path, value()?),
        "remove" => remove(root, path).map(|_| ()),
        "replace" => {
            let v = value()?;
            if path.is_empty() {
                return Err("replacing the whole document: use doc.replace".into());
            }
            let slot = root.pointer_mut(path).ok_or_else(|| format!("nothing at {path:?}"))?;
            *slot = v;
            Ok(())
        }
        "move" => {
            let from = from()?;
            if path.starts_with(&format!("{from}/")) {
                return Err("can't move a value into itself".into());
            }
            let v = remove(root, from)?;
            add(root, path, v)
        }
        "copy" => {
            let v = root.pointer(from()?).cloned().ok_or("nothing at `from`")?;
            add(root, path, v)
        }
        "test" => {
            let expected = value()?;
            match root.pointer(path) {
                Some(actual) if *actual == expected => Ok(()),
                Some(actual) => Err(format!("test failed at {path:?}: found {actual}")),
                None => Err(format!("test failed: nothing at {path:?}")),
            }
        }
        other => Err(format!("unknown op {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::Session;

    const PNG_1PX: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn doc_json_reads_a_slide() {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "title", "title": "Hello"})).unwrap();
        let r = s.execute("doc.json", &json!({"pointer": "/slides/0"})).unwrap();
        assert!(r["json"].is_object());
        assert_eq!(r["truncated"], false);
        assert!(s.execute("doc.json", &json!({"pointer": "/nope"})).is_err());
    }

    #[test]
    fn doc_json_truncates_large_output() {
        let mut s = Session::with_new();
        let r = s.execute("doc.json", &json!({"maxBytes": 100})).unwrap();
        assert_eq!(r["truncated"], true);
        assert!(r["json"].as_str().unwrap().len() <= 100);
    }

    #[test]
    fn doc_json_is_not_an_undo_step() {
        let mut s = Session::with_new();
        let n = s.active().unwrap().history.undo.len();
        s.execute("doc.json", &json!({})).unwrap();
        assert_eq!(s.active().unwrap().history.undo.len(), n);
    }

    #[test]
    fn doc_patch_removes_a_slide_as_one_undo_step() {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "title"})).unwrap();
        let before = s.active().unwrap().doc.slides.len();
        let r = s.execute("doc.patch", &json!({"patch": [{"op": "remove", "path": "/slides/0"}]})).unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(s.active().unwrap().doc.slides.len(), before - 1);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.slides.len(), before);
    }

    #[test]
    fn doc_patch_ops_add_replace_move_copy_test() {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "title"})).unwrap();
        let w = s.active().unwrap().doc.slide_size.width;
        let patch = json!([
            {"op": "test", "path": "/slideSize/width", "value": w},
            {"op": "replace", "path": "/props/title", "value": "Patched"},
            {"op": "copy", "from": "/slides/0", "path": "/slides/-"},
            {"op": "move", "from": "/slides/2", "path": "/slides/0"},
            {"op": "add", "path": "/tags", "value": [["k", "v"]]}
        ]);
        s.execute("doc.patch", &json!({"patch": patch})).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!(d.slides.len(), 3);
        assert_eq!(d.props.title, "Patched");
        assert_eq!(d.tags.last().unwrap(), &("k".to_string(), "v".to_string()));
        // A copied slide gets fresh ids (repair), so ids stay unique.
        let mut ids: Vec<_> = d.slides.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn doc_patch_failed_test_op_changes_nothing() {
        let mut s = Session::with_new();
        let before = s.active().unwrap().doc.clone();
        let r = s.execute("doc.patch", &json!({"patch": [{"op": "replace", "path": "/props/title", "value": "x"}, {"op": "test", "path": "/props/title", "value": "y"}]}));
        assert!(r.is_err());
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before));
    }

    #[test]
    fn doc_patch_rejects_invalid_and_keeps_the_deck() {
        let mut s = Session::with_new();
        let before = s.active().unwrap().doc.clone();
        assert!(s.execute("doc.patch", &json!({"patch": [{"op": "replace", "path": "/slides", "value": "nope"}]})).is_err());
        assert!(s.execute("doc.patch", &json!({"patch": [{"op": "remove", "path": "/nope/1"}]})).is_err());
        assert!(s.execute("doc.patch", &json!({"patch": [{"op": "frobnicate", "path": "/props"}]})).is_err());
        assert!(s.execute("doc.patch", &json!({"patch": "x"})).is_err());
        assert!(s.execute("doc.patch", &json!({"patch": [{"op": "remove", "path": ""}]})).is_err());
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before));
    }

    #[test]
    fn doc_patch_keeps_media_bytes() {
        let mut s = Session::with_new();
        s.execute("insert.picture", &json!({"data": PNG_1PX, "name": "dot.png"})).unwrap();
        let len = s.active().unwrap().doc.media.first().unwrap().data.len();
        assert!(len > 0);
        s.execute("doc.patch", &json!({"patch": [{"op": "replace", "path": "/props/title", "value": "t"}]})).unwrap();
        assert_eq!(s.active().unwrap().doc.media.first().unwrap().data.len(), len);
    }

    #[test]
    fn doc_replace_round_trips_and_undoes() {
        let mut s = Session::with_new();
        let snapshot = serde_json::to_value(&*s.active().unwrap().doc).unwrap();
        s.execute("slide.new", &json!({})).unwrap();
        s.execute("slide.new", &json!({})).unwrap();
        s.execute("doc.replace", &json!({"json": snapshot})).unwrap();
        assert_eq!(s.active().unwrap().doc.slides.len(), 1);
        assert_eq!(s.active().unwrap().history.undo.last().unwrap().label, "Replace Document");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.slides.len(), 3);
    }

    #[test]
    fn doc_commands_without_a_document_fail() {
        let mut s = Session::new();
        assert!(s.execute("doc.json", &json!({})).is_err());
        assert!(s.execute("doc.patch", &json!({"patch": []})).is_err());
    }
}

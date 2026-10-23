//! The workspace: a folder of the person's materials (briefs, notes, data, pictures, drafts, style
//! templates) that they open next to the deck, for themselves and for the agent. The host reads
//! it (the browser's File System Access API on the web); files arrive asynchronously.
//!
//! An `AGENT.md` (or `agent.md`, `DECKCRAFT.md`) at the folder's root holds project instructions
//! that go with every request to the agent.
//!
//! A workspace belongs to a presentation: each open deck has its own folder (or none), a new
//! presentation starts without one, and a deck opened from a workspace joins it. The host
//! remembers a deck's folder under its key (its file name), so it comes back next time.

use std::collections::HashMap;

/// One file in the workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct WsEntry {
    /// Path relative to the folder, `/`-separated.
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WsStatus {
    /// No folder chosen.
    None,
    /// A folder was chosen on an earlier visit; reading it again needs the person's OK (a click).
    NeedsPermission { name: String },
    /// Listing the folder.
    Loading { name: String },
    Ready { name: String },
    Failed(String),
}

/// What reads the folder (the browser, or a test's fake).
pub trait WorkspaceHost {
    fn status(&self) -> WsStatus;
    /// Ask the person for a folder (a picker; call from a click).
    fn pick(&mut self);
    /// Read the remembered folder again (call from a click).
    fn reconnect(&mut self);
    /// Stop using the folder.
    fn forget(&mut self);
    /// List the folder again.
    fn refresh(&mut self);
    /// The files, once listed.
    fn files(&self) -> Vec<WsEntry>;
    /// Start reading `path`; its bytes arrive through [`WorkspaceHost::poll_reads`].
    fn read(&mut self, path: &str);
    /// Reads that finished since the last call: (path, bytes or error).
    fn poll_reads(&mut self) -> Vec<(String, Result<Vec<u8>, String>)>;
    /// The same folder for another deck (one opened from this workspace), remembered under its key.
    fn share(&self, key: &str) -> Box<dyn WorkspaceHost>;
}

/// Makes the host for a deck, given its key (which folder it had before, if any).
pub type WorkspaceFactory = Box<dyn Fn(&str) -> Box<dyn WorkspaceHost>>;

/// The key a deck's workspace is remembered under: its file, or its name once it came from a
/// file; a new, untitled presentation's workspace lasts for the session.
pub fn workspace_key(d: &deckcraft_engine::DocState) -> String {
    if let Some(p) = &d.path {
        return format!("file:{p}");
    }
    let untitled = d.title.strip_prefix("Presentation").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    if untitled { format!("doc:{:x}", d.uid) } else { format!("title:{}", d.title) }
}

/// The largest file the workspace reads.
pub const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

/// Names of the project-instructions file, tried in order at the folder's root.
pub const INSTRUCTION_FILES: &[&str] = &["AGENT.md", "agent.md", "Agent.md", "DECKCRAFT.md", "deckcraft.md"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    /// A deck: .pptx, .potx, .ppsx, .deckcraft.
    Deck,
    /// A theme (.thmx).
    Theme,
    Word,
    Pdf,
    Other,
}

pub fn kind_of(path: &str) -> Kind {
    let ext = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "yaml" | "yml" | "toml" | "html" | "htm" | "xml" | "rtf" => Kind::Text,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" => Kind::Image,
        "pptx" | "potx" | "ppsx" | "pptm" | "potm" | "deckcraft" => Kind::Deck,
        "thmx" => Kind::Theme,
        "docx" => Kind::Word,
        "pdf" => Kind::Pdf,
        _ => Kind::Other,
    }
}

pub fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The text of a file the agent (or a person) can read: plain text as is, a deck as an outline
/// (one slide per block: title, then body lines), a Word document's paragraphs. None for other
/// kinds.
pub fn text_of(path: &str, bytes: &[u8]) -> Option<Result<String, String>> {
    match kind_of(path) {
        Kind::Text => Some(Ok(String::from_utf8_lossy(bytes).into_owned())),
        Kind::Deck => Some(deck_outline(path, bytes)),
        Kind::Word => Some(docx_text(bytes)),
        _ => None,
    }
}

fn deck_outline(path: &str, bytes: &[u8]) -> Result<String, String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (p, _) = deckcraft_engine::cmd::file::open_checked(name, bytes).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for (i, s) in p.slides.iter().enumerate() {
        out.push_str(&format!("Slide {}: {}\n", i + 1, s.title().replace('\n', " ")));
        for sh in &s.shapes {
            if sh.ph_type().is_some_and(|k| k.is_title()) {
                continue;
            }
            if let Some(t) = &sh.text {
                for para in &t.paragraphs {
                    let line: String = para.runs.iter().map(|r| r.content()).collect();
                    if !line.trim().is_empty() {
                        out.push_str(&format!("  {}{}\n", "  ".repeat(para.level as usize), line.trim()));
                    }
                }
            }
        }
        let notes = s.notes_text();
        if !notes.trim().is_empty() {
            out.push_str(&format!("  [notes] {}\n", notes.trim().replace('\n', " ")));
        }
    }
    let theme = p.masters.first().map(|m| format!("Theme fonts: {} / {}\n", m.theme.fonts.major.latin, m.theme.fonts.minor.latin)).unwrap_or_default();
    Ok(format!("{} slides, {:.0} x {:.0} pt\n{theme}\n{out}", p.slides.len(), p.slide_size.width, p.slide_size.height))
}

/// The paragraphs of a .docx (word/document.xml), one per line.
fn docx_text(bytes: &[u8]) -> Result<String, String> {
    use std::io::Read as _;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a Word document: {e}"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml").map_err(|e| format!("not a Word document: {e}"))?.take(32 << 20).read_to_string(&mut xml).map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut rest = xml.as_str();
    // <w:t …>text</w:t> runs; </w:p> ends a paragraph; <w:tab/> and <w:br/> are spacing.
    while let Some(i) = rest.find('<') {
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        if tag.starts_with("w:t") && !tag.ends_with('/') && (tag.len() == 3 || tag.as_bytes()[3] == b' ') {
            let end = rest.find("</w:t>").unwrap_or(rest.len());
            out.push_str(&unescape(&rest[..end]));
            rest = &rest[end..];
        } else if tag == "/w:p" {
            out.push('\n');
        } else if tag.starts_with("w:tab") {
            out.push('\t');
        } else if tag.starts_with("w:br") {
            out.push('\n');
        }
    }
    Ok(out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string())
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Files read during a request, by path (the agent's tools wait for a read, then run).
#[derive(Default)]
pub struct ReadCache {
    pub files: HashMap<String, Result<Vec<u8>, String>>,
}

/// `AGENT.md` at the root, if the workspace has one.
pub fn instructions_file(files: &[WsEntry]) -> Option<&WsEntry> {
    INSTRUCTION_FILES.iter().find_map(|n| files.iter().find(|f| f.path == *n))
}

/// Human file size: "12 KB".
pub fn size_label(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// Something the person asked to do with a workspace file, done when its bytes arrive.
#[derive(Clone, Debug, PartialEq)]
pub enum UserOp {
    /// Open a deck as a new presentation.
    Open,
    /// Insert a picture on the current slide.
    Insert,
    /// Apply a template's design to this deck.
    ApplyDesign,
}

/// The open workspace: its host, files read so far, and the person's pending actions.
pub struct Workspace {
    pub host: Box<dyn WorkspaceHost>,
    pub cache: HashMap<String, Result<Vec<u8>, String>>,
    /// Reads asked for and not yet back.
    pub reading: std::collections::HashSet<String>,
    pub user_ops: Vec<(String, UserOp)>,
    /// `AGENT.md`'s text, once read.
    pub instructions: Option<String>,
    instructions_for: Option<String>,
}

impl Workspace {
    pub fn new(host: Box<dyn WorkspaceHost>) -> Self {
        Self { host, cache: HashMap::new(), reading: Default::default(), user_ops: vec![], instructions: None, instructions_for: None }
    }

    /// Ask the host for `path` unless it's read or being read.
    pub fn want(&mut self, path: &str) {
        if !self.cache.contains_key(path) && self.reading.insert(path.to_string()) {
            self.host.read(path);
        }
    }

    pub fn folder_name(&self) -> Option<String> {
        match self.host.status() {
            WsStatus::Ready { name } => Some(name),
            _ => None,
        }
    }
}

impl crate::SlideApp {
    /// How decks get their workspace host (the browser's folder access).
    pub fn set_workspace_factory(&mut self, f: WorkspaceFactory) {
        self.workspace_factory = Some(f);
    }

    /// Give the active deck this workspace host (tests; the app uses the factory).
    pub fn set_workspace_host(&mut self, host: Box<dyn WorkspaceHost>) {
        if let Some(uid) = self.session.active().map(|d| d.uid) {
            self.workspaces.insert(uid, Workspace::new(host));
        }
    }

    /// Whether this app can open workspace folders at all.
    pub fn has_workspaces(&self) -> bool {
        self.workspace_factory.is_some() || !self.workspaces.is_empty()
    }

    /// The active deck's workspace.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.workspaces.get(&self.session.active()?.uid)
    }

    pub fn workspace_mut(&mut self) -> Option<&mut Workspace> {
        let uid = self.session.active()?.uid;
        self.workspaces.get_mut(&uid)
    }

    /// Rename the presentation; its workspace folder is remembered under the new name too.
    pub fn rename_presentation(&mut self, title: &str) {
        match self.run("file.rename", serde_json::json!({"title": title})) {
            Ok(_) => {
                if let Some(d) = self.session.active() {
                    let key = workspace_key(d);
                    let uid = d.uid;
                    if let Some(ws) = self.workspaces.get(&uid)
                        && ws.folder_name().is_some()
                    {
                        let host = ws.host.share(&key);
                        self.workspaces.insert(uid, Workspace::new(host));
                    }
                }
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }

    /// The person's action on a workspace file (done once it's read).
    pub fn workspace_action(&mut self, path: &str, op: UserOp) {
        if let Some(ws) = self.workspace_mut() {
            ws.user_ops.push((path.to_string(), op));
            ws.want(path);
        }
    }

    /// Give the active deck its workspace host, collect finished reads, do the person's pending
    /// actions, and keep each workspace's `AGENT.md` read.
    pub fn workspace_tick(&mut self) {
        let open: std::collections::HashSet<u64> = self.session.documents().iter().map(|d| d.uid).collect();
        self.workspaces.retain(|uid, _| open.contains(uid));
        if let (Some(f), Some(d)) = (self.workspace_factory.as_ref(), self.session.active())
            && !self.workspaces.contains_key(&d.uid)
        {
            let host = f(&workspace_key(d));
            self.workspaces.insert(d.uid, Workspace::new(host));
        }
        let mut done: Vec<(u64, String, UserOp, Result<Vec<u8>, String>)> = vec![];
        for (uid, ws) in self.workspaces.iter_mut() {
            ws.tick();
            let (ready, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut ws.user_ops).into_iter().partition(|(p, _)| ws.cache.contains_key(p));
            ws.user_ops = wait;
            for (path, op) in ready {
                if let Some(r) = ws.cache.remove(&path) {
                    done.push((*uid, path, op, r));
                }
            }
        }
        for (uid, path, op, r) in done {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            let result = r.and_then(|bytes| {
                let data = deckcraft_engine::cmd::base64_encode(&bytes);
                let (cmd, params) = match op {
                    UserOp::Open => ("file.openBytes", serde_json::json!({"name": name, "data": data})),
                    UserOp::Insert => ("insert.picture", serde_json::json!({"name": name, "data": data})),
                    UserOp::ApplyDesign => ("design.browseTheme", serde_json::json!({"data": data})),
                };
                self.run(cmd, params).map(|_| ()).map_err(|e| e.to_string())
            });
            match result {
                // A deck opened from a workspace belongs to it.
                Ok(()) if op == UserOp::Open => {
                    if let (Some(from), Some(d)) = (self.workspaces.get(&uid), self.session.active())
                        && d.uid != uid
                    {
                        let host = from.host.share(&workspace_key(d));
                        self.workspaces.insert(d.uid, Workspace::new(host));
                    }
                }
                Ok(()) => {}
                Err(e) => self.set_status(format!("{name}: {e}")),
            }
        }
    }
}

impl Workspace {
    /// Collect finished reads and keep `AGENT.md` read.
    fn tick(&mut self) {
        for (path, r) in self.host.poll_reads() {
            self.reading.remove(&path);
            self.cache.insert(path, r);
        }
        let files = self.host.files();
        let key = self.folder_name().and_then(|n| instructions_file(&files).map(|f| format!("{n}/{}:{}", f.path, f.size)));
        if key != self.instructions_for {
            self.instructions = None;
            self.instructions_for = key;
            if let Some(f) = instructions_file(&files) {
                let p = f.path.clone();
                self.cache.remove(&p);
                self.want(&p);
            }
        }
        if self.instructions.is_none()
            && let Some(f) = instructions_file(&files)
            && let Some(Ok(b)) = self.cache.get(&f.path)
        {
            self.instructions = Some(String::from_utf8_lossy(b).chars().take(20_000).collect());
        }
    }
}

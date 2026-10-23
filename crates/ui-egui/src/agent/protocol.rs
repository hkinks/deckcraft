//! Messages between the editor and the agent server (`/ws/agent`), as JSON text frames.
//! Every decode is bounded: frames over [`MAX_FRAME`] and unknown message types are refused.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
/// Largest frame either side accepts (the server enforces the same cap).
pub const MAX_FRAME: usize = 16 << 20;

/// One part of a tool result: text, or a PNG the model can look at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { media_type: String, data: String },
}

/// What the agent is told about the editor with each request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckContext {
    pub deck_key: String,
    pub slides: usize,
    pub current: usize,
    pub selection: Vec<u32>,
    pub slide_size: [f64; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello { v: u32, tools: Vec<Value>, client_version: String },
    OpenConversation { v: u32, deck_key: String },
    UserMessage { v: u32, text: String, deck: DeckContext, provider: Option<String> },
    ToolResult { v: u32, id: String, ok: bool, content: Vec<ContentPart> },
    Stop { v: u32 },
    /// The editor tab was hidden or shown: a hidden tab draws no frames and runs no tool calls,
    /// so the server pauses its tool timeout.
    Visibility { v: u32, hidden: bool },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub vision: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct TurnInfo {
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub slides: Vec<usize>,
    #[serde(default)]
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HistoryItem {
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub turn: Option<TurnInfo>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Ready {
        #[serde(default)]
        user: String,
        #[serde(default)]
        providers: Vec<ProviderInfo>,
        #[serde(default)]
        default_provider: String,
    },
    History {
        #[serde(default)]
        conversation_id: Option<String>,
        #[serde(default)]
        items: Vec<HistoryItem>,
    },
    Conversation {
        id: String,
        #[serde(default)]
        title: String,
    },
    AssistantDelta {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    TurnEnd {
        #[serde(default)]
        turn_id: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        actions: Vec<String>,
        #[serde(default)]
        slides: Vec<usize>,
        #[serde(default)]
        status: String,
    },
    Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    },
}

pub fn decode(frame: &str) -> Result<ServerMsg, String> {
    if frame.len() > MAX_FRAME {
        return Err(format!("message too large ({} bytes)", frame.len()));
    }
    serde_json::from_str(frame).map_err(|e| format!("bad message from the agent server: {e}"))
}

pub fn encode(m: &ClientMsg) -> String {
    serde_json::to_string(m).unwrap_or_else(|e| format!("{{\"type\":\"error\",\"message\":\"{e}\"}}"))
}

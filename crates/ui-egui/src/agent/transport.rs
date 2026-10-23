//! How the pane reaches the agent server. The web app implements this over a WebSocket and
//! `fetch`; tests use a scripted fake. Keeps `ui-egui` free of browser APIs.

use super::protocol::{ClientMsg, ServerMsg};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    /// Not signed in: the pane shows the sign-in form.
    LoggedOut,
    /// Signing in or opening the socket.
    Connecting,
    Connected,
    /// Not usable (no agent server, wrong password, socket closed); the text says why.
    Failed(String),
}

pub trait AgentTransport {
    fn send(&mut self, m: &ClientMsg);
    /// Messages that arrived since the last poll.
    fn poll(&mut self) -> Vec<ServerMsg>;
    fn status(&self) -> LinkStatus;
    fn login(&mut self, user: &str, password: &str);
    fn logout(&mut self);
    /// Try to connect again after a failure.
    fn retry(&mut self);
}

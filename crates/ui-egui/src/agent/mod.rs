//! The agent chat pane's core: messages exchanged with the DeckCraft agent server, the tool
//! allow-list, and running the agent's tool calls on the live session.

pub mod client;
pub mod pane;
pub mod protocol;
pub mod tools;
pub mod transport;
pub mod workspace_tools;

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod pane_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

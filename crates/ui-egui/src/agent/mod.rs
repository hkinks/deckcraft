//! The agent chat pane's core: messages exchanged with the DeckCraft agent server, the tool
//! allow-list, and running the agent's tool calls on the live session.

pub mod protocol;
pub mod tools;

#[cfg(test)]
mod tests;

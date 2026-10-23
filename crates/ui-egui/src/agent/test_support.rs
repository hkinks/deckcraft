//! A scripted agent server for tests.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use serde_json::Value;

use super::protocol::{ClientMsg, ServerMsg};
use super::transport::{AgentTransport, LinkStatus};

#[derive(Clone, Default)]
pub struct Wire {
    pub inbox: Rc<RefCell<VecDeque<ServerMsg>>>,
    pub sent: Rc<RefCell<Vec<ClientMsg>>>,
    pub status: Rc<RefCell<Option<LinkStatus>>>,
}

pub struct Fake(pub Wire);

impl AgentTransport for Fake {
    fn send(&mut self, m: &ClientMsg) {
        self.0.sent.borrow_mut().push(m.clone());
    }
    fn poll(&mut self) -> Vec<ServerMsg> {
        self.0.inbox.borrow_mut().drain(..).collect()
    }
    fn status(&self) -> LinkStatus {
        self.0.status.borrow().clone().unwrap_or(LinkStatus::Connected)
    }
    fn login(&mut self, _user: &str, _password: &str) {}
    fn logout(&mut self) {}
    fn retry(&mut self) {}
}

impl Wire {
    pub fn push(&self, m: ServerMsg) {
        self.inbox.borrow_mut().push_back(m);
    }
    pub fn sent_of(&self, kind: &str) -> Vec<Value> {
        self.sent
            .borrow()
            .iter()
            .map(|m| serde_json::from_str::<Value>(&super::protocol::encode(m)).unwrap())
            .filter(|v| v["type"] == kind)
            .collect()
    }
}


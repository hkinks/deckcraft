//! A workspace host for tests: files in memory, reads delivered when the test says.

use std::cell::RefCell;
use std::rc::Rc;

use crate::workspace::{WorkspaceHost, WsEntry, WsStatus};

#[derive(Default)]
pub struct FakeFs {
    pub name: String,
    pub files: Vec<(String, Vec<u8>)>,
    pub asked: Vec<String>,
    /// Deliver reads right away (otherwise `deliver` does).
    pub instant: bool,
    pending: Vec<String>,
    pub picked: bool,
}

#[derive(Clone, Default)]
pub struct FakeHost(pub Rc<RefCell<FakeFs>>);

impl FakeHost {
    pub fn with(files: &[(&str, &[u8])]) -> Self {
        let h = FakeHost::default();
        {
            let mut fs = h.0.borrow_mut();
            fs.name = "Launch".into();
            fs.files = files.iter().map(|(p, b)| (p.to_string(), b.to_vec())).collect();
        }
        h
    }
}

impl WorkspaceHost for FakeHost {
    fn status(&self) -> WsStatus {
        let fs = self.0.borrow();
        if fs.name.is_empty() { WsStatus::None } else { WsStatus::Ready { name: fs.name.clone() } }
    }
    fn pick(&mut self) {
        self.0.borrow_mut().picked = true;
    }
    fn reconnect(&mut self) {}
    fn forget(&mut self) {
        self.0.borrow_mut().name.clear();
    }
    fn refresh(&mut self) {}
    fn files(&self) -> Vec<WsEntry> {
        self.0.borrow().files.iter().map(|(p, b)| WsEntry { path: p.clone(), size: b.len() as u64 }).collect()
    }
    fn read(&mut self, path: &str) {
        let mut fs = self.0.borrow_mut();
        fs.asked.push(path.to_string());
        fs.pending.push(path.to_string());
    }
    fn poll_reads(&mut self) -> Vec<(String, Result<Vec<u8>, String>)> {
        let mut fs = self.0.borrow_mut();
        if !fs.instant {
            return vec![];
        }
        let pending = std::mem::take(&mut fs.pending);
        pending
            .into_iter()
            .map(|p| {
                let r = fs.files.iter().find(|(q, _)| *q == p).map(|(_, b)| b.clone()).ok_or_else(|| "gone".to_string());
                (p, r)
            })
            .collect()
    }
}

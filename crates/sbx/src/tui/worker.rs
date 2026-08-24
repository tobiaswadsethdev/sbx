//! Background worker.
//!
//! Every gateway call is a subprocess round trip costing hundreds of
//! milliseconds, so none of them may happen on the render thread. The worker
//! owns all I/O; the UI only ever sends requests and drains updates.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use openshell_client::{CliClient, PolicyRevision, PolicyUpdate};

use crate::events::Event;
use crate::ops;
use crate::session::Session;

pub enum Request {
    Refresh,
    Preview(Box<Session>),
    Diff(Box<Session>),
    Poll(Box<Session>),
    Policy(Box<Session>),
    Events(Box<Session>),
    /// Widen or tighten a live sandbox's network rules. Carries the label to
    /// report, since only the caller knows whether this was a widen or a
    /// tighten by the time the answer comes back.
    Repolicy {
        session: Box<Session>,
        update: Box<PolicyUpdate>,
        label: String,
    },
    Shutdown,
}

pub enum Update {
    Sessions(Box<ops::Refreshed>),
    Preview {
        session: String,
        body: String,
    },
    Diff {
        session: String,
        body: String,
    },
    Polled {
        session: String,
        poll: Box<ops::Poll>,
    },
    Policy {
        session: String,
        result: Box<Result<PolicyRevision, String>>,
    },
    Events {
        session: String,
        result: Box<Result<Vec<Event>, String>>,
    },
    /// A completed policy change. The revision comes back with it so the pane
    /// can show the result rather than refetching and briefly showing the old
    /// rules as though nothing happened.
    Repolicied {
        session: String,
        label: String,
        result: Box<Result<PolicyRevision, String>>,
    },
    Failed(String),
}

pub struct Worker {
    pub tx: Sender<Request>,
    pub rx: Receiver<Update>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(client: CliClient) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (up_tx, up_rx) = channel::<Update>();

        let handle = thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let update = match req {
                    Request::Shutdown => break,
                    Request::Refresh => match ops::refresh(&client) {
                        Ok(r) => Update::Sessions(Box::new(r)),
                        Err(e) => Update::Failed(e.to_string()),
                    },
                    Request::Preview(session) => Update::Preview {
                        body: ops::repo_preview(&client, &session),
                        session: session.name,
                    },
                    Request::Diff(session) => Update::Diff {
                        body: ops::repo_diff(&client, &session),
                        session: session.name,
                    },
                    Request::Poll(session) => Update::Polled {
                        poll: Box::new(ops::poll(&client, &session)),
                        session: session.name,
                    },
                    Request::Policy(session) => Update::Policy {
                        result: Box::new(ops::policy(&client, &session)),
                        session: session.name,
                    },
                    Request::Events(session) => Update::Events {
                        result: Box::new(ops::events(&client, &session)),
                        session: session.name,
                    },
                    Request::Repolicy {
                        session,
                        update,
                        label,
                    } => Update::Repolicied {
                        result: Box::new(ops::repolicy(&client, &session, &update)),
                        session: session.name,
                        label,
                    },
                };
                // A closed channel means the UI is gone; stop quietly.
                if up_tx.send(update).is_err() {
                    break;
                }
            }
        });

        Worker {
            tx: req_tx,
            rx: up_rx,
            handle: Some(handle),
        }
    }

    pub fn send(&self, req: Request) {
        // The worker only disappears during shutdown, where dropping the
        // request is the correct behaviour.
        let _ = self.tx.send(req);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Shutdown);
        if let Some(h) = self.handle.take() {
            // The worker may be mid-request; joining keeps the terminal from
            // being restored underneath a still-running subprocess.
            let _ = h.join();
        }
    }
}

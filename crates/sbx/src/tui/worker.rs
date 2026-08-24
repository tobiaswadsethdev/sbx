//! Background worker.
//!
//! Every gateway call is a subprocess round trip costing hundreds of
//! milliseconds, so none of them may happen on the render thread. The worker
//! owns all I/O; the UI only ever sends requests and drains updates.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use openshell_client::CliClient;

use crate::ops;
use crate::session::Session;

pub enum Request {
    Refresh,
    Preview(Box<Session>),
    Diff(Box<Session>),
    Poll(Box<Session>),
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

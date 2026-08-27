//! Background worker.
//!
//! Every gateway call is a subprocess round trip costing hundreds of
//! milliseconds, so none of them may happen on the render thread. The worker
//! owns all I/O; the UI only ever sends requests and drains updates.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use openshell_client::{CliClient, OpenShell, PolicyRevision, PolicyUpdate, Provider};

use crate::events::Event;
use crate::ops;
use crate::repos::{self, Facts, LocalRepo};
use crate::session::Session;

pub enum Request {
    /// Reconcile the list against the gateway. `repair` re-reads the metadata of
    /// anything stuck mid-create, which is worth doing once at startup and not
    /// once a second; see [`ops::refresh_with`].
    Refresh {
        repair: bool,
    },
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
    Publish(Box<Session>),
    /// Delete a sandbox and forget the session. Carries the name rather than the
    /// session, since the record may be all that is left of it.
    Destroy(String),
    /// Scan the host for git repositories, for the create flow's picker.
    ScanRepos,
    /// Ask git how far a checkout has drifted from its remote.
    Inspect {
        path: PathBuf,
        branch: Option<String>,
    },
    /// The providers defined at the gateway, for the create form.
    Providers,
    /// Start a session. Runs on a thread of its own; see [`Worker::spawn`].
    Create(Box<ops::Draft>),
    Shutdown,
}

pub enum Update {
    Sessions(Box<ops::Refreshed>),
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
    Published {
        session: String,
        result: Box<Result<crate::publish::Outcome, String>>,
    },
    Destroyed {
        session: String,
        result: Box<Result<ops::Destroyed, String>>,
    },
    /// The result of a host scan. Never an error: an unreadable directory is
    /// skipped rather than failing the scan, so the worst case is an empty list.
    Repos(Vec<LocalRepo>),
    /// Git's answer about one checkout. Carries the path it was asked about, so
    /// an answer arriving after the repository was changed can be discarded
    /// rather than shown against the wrong one.
    Inspected {
        path: PathBuf,
        facts: Box<Facts>,
    },
    Providers(Box<Result<Vec<Provider>, String>>),
    /// A stage of a create beginning, so the list and the footer can say where
    /// it has got to over the half-minute it takes.
    Creating {
        session: String,
        step: ops::Step,
    },
    Created {
        session: String,
        result: Box<Result<ops::Created, String>>,
    },
    Failed(String),
}

/// Run one create on its own thread, reporting each stage as it starts.
///
/// Detached deliberately: joining it on shutdown would hold the terminal
/// hostage for the rest of a clone, and the session is recoverable either way --
/// the store carries a record of how far it got, and the sandbox carries its own
/// metadata. Quitting mid-create asks for confirmation for the same reason.
fn spawn_create(client: CliClient, up_tx: Sender<Update>, draft: ops::Draft) {
    thread::spawn(move || {
        let name = draft.name.clone();
        // Checked here rather than built: `image::build` streams docker's output
        // to the terminal, which would tear the TUI apart mid-frame.
        if !crate::image::exists() {
            let _ = up_tx.send(Update::Created {
                session: name,
                result: Box::new(Err(format!(
                    "the sandbox image {} is missing; run `sbx image build`",
                    crate::session::IMAGE
                ))),
            });
            return;
        }

        let progress = &mut |step: ops::Step| {
            let _ = up_tx.send(Update::Creating {
                session: name.clone(),
                step,
            });
        };
        let result = ops::create(&client, &draft, progress);
        let _ = up_tx.send(Update::Created {
            session: draft.name,
            result: Box::new(result),
        });
    });
}

pub struct Worker {
    pub tx: Sender<Request>,
    pub rx: Receiver<Update>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Worker {
    /// `roots` is where a repository scan looks. Resolved by the caller rather
    /// than here: it depends on the config file and the working directory, and
    /// neither is the worker's business to go and read between requests.
    pub fn spawn(client: CliClient, roots: Vec<repos::Root>) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (up_tx, up_rx) = channel::<Update>();

        let handle = thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let update = match req {
                    Request::Shutdown => break,
                    // On its own thread, unlike every other request. A create
                    // takes tens of seconds -- sandbox, clone, agent -- and
                    // requests here are served one at a time, so running it
                    // inline would freeze the state column and every pane for
                    // as long as it lasts.
                    Request::Create(draft) => {
                        spawn_create(client.clone(), up_tx.clone(), *draft);
                        continue;
                    }
                    Request::Refresh { repair } => match ops::refresh_with(&client, repair) {
                        Ok(r) => Update::Sessions(Box::new(r)),
                        Err(e) => Update::Failed(e.to_string()),
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
                    Request::Publish(session) => Update::Published {
                        result: Box::new(ops::publish(
                            &client,
                            &session,
                            &crate::publish::Options::default(),
                        )),
                        session: session.name,
                    },
                    Request::Destroy(name) => Update::Destroyed {
                        result: Box::new(ops::destroy(&client, &name)),
                        session: name,
                    },
                    Request::ScanRepos => Update::Repos(repos::discover_in(&roots)),
                    Request::Inspect { path, branch } => Update::Inspected {
                        facts: Box::new(repos::inspect(&path, branch.as_deref())),
                        path,
                    },
                    Request::Providers => Update::Providers(Box::new(
                        client
                            .providers()
                            .map_err(|e| format!("could not read the provider list: {e}")),
                    )),
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

//! Background worker.
//!
//! Every gateway call is a subprocess round trip costing hundreds of
//! milliseconds, so none of them may happen on the render thread. The worker
//! owns all I/O; the UI only ever sends requests and drains updates.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use openshell_client::{CliClient, OpenShell, PolicyRevision, PolicyUpdate, Provider};

use sbx_core::backend::Backends;
use sbx_core::config::Config;
use sbx_core::events::Event;
use sbx_core::ops;
use sbx_core::repos::{self, Facts, LocalRepo};
use sbx_core::session::Session;

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
        result: Box<Result<sbx_core::publish::Outcome, String>>,
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
fn spawn_create(client: CliClient, cfg: Config, up_tx: Sender<Update>, draft: ops::Draft) {
    thread::spawn(move || {
        let name = draft.name.clone();
        // Checked here rather than built: `image::build` streams docker's output
        // to the terminal, which would tear the TUI apart mid-frame.
        //
        // The image the *draft* needs, not the base one: a session asking for a
        // toolchain runs a variant, and a variant nobody has built yet fails at
        // the gateway with docker's words about a manifest. The message names the
        // command that builds it, which is a command line rather than a
        // keystroke for the reason above.
        let tag = sbx_core::toolchain::tag(&draft.toolchains);
        if !sbx_core::image::exists_tag(&tag) {
            let fix = if draft.toolchains.is_empty() {
                "sbx image build".to_string()
            } else {
                format!(
                    "sbx image build --toolchain {}",
                    sbx_core::toolchain::labels(&draft.toolchains).join(",")
                )
            };
            let _ = up_tx.send(Update::Created {
                session: name,
                result: Box::new(Err(format!(
                    "the sandbox image {tag} is missing; run `{fix}`"
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
        let backends = Backends::from_config(Box::new(client), &cfg);
        let result = ops::create(&backends, &draft, progress);
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
    pub fn spawn(client: CliClient, cfg: Config, roots: Vec<repos::Root>) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (up_tx, up_rx) = channel::<Update>();

        let handle = thread::spawn(move || {
            // Both backends, built per request rather than held: it is two paths
            // and a client handle, no I/O, and building it each time is what
            // lets an edited `worktree_root` apply without restarting the TUI.
            let backends = || Backends::from_config(Box::new(client.clone()), &cfg);
            while let Ok(req) = req_rx.recv() {
                let update = match req {
                    Request::Shutdown => break,
                    // On its own thread, unlike every other request. A create
                    // takes tens of seconds -- sandbox, clone, agent -- and
                    // requests here are served one at a time, so running it
                    // inline would freeze the state column and every pane for
                    // as long as it lasts.
                    Request::Create(draft) => {
                        spawn_create(client.clone(), cfg.clone(), up_tx.clone(), *draft);
                        continue;
                    }
                    Request::Refresh { repair } => match ops::refresh_with(&backends(), repair) {
                        Ok(r) => Update::Sessions(Box::new(r)),
                        Err(e) => Update::Failed(e.to_string()),
                    },
                    Request::Diff(session) => Update::Diff {
                        body: ops::repo_diff(backends().for_session(&session), &session),
                        session: session.name,
                    },
                    Request::Poll(session) => Update::Polled {
                        poll: Box::new(ops::poll(backends().for_session(&session), &session)),
                        session: session.name,
                    },
                    Request::Policy(session) => Update::Policy {
                        result: Box::new(ops::policy(backends().for_session(&session), &session)),
                        session: session.name,
                    },
                    Request::Events(session) => Update::Events {
                        result: Box::new(ops::events(backends().for_session(&session), &session)),
                        session: session.name,
                    },
                    Request::Publish(session) => Update::Published {
                        result: Box::new(ops::publish(
                            backends().for_session(&session),
                            &session,
                            &sbx_core::publish::Options::default(),
                        )),
                        session: session.name,
                    },
                    Request::Destroy(name) => Update::Destroyed {
                        result: Box::new(ops::destroy(&backends(), &name)),
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
                        result: Box::new(ops::repolicy(
                            backends().for_session(&session),
                            &session,
                            &update,
                        )),
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

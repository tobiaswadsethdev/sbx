//! The desktop application's Rust half.
//!
//! Thin on purpose. Every command here is a paired server's [`Remote::call`]
//! and a `?`; the reasoning about what a session *is* stays in `sbx-core`, and
//! the wire stays in `sbx-proto`. A behaviour that existed only here would be
//! one the CLI cannot do, which is the drift the whole split exists to avoid.
//!
//! **The connection is made on this side, and it has to be.** The certificate
//! is pinned by fingerprint, and a webview cannot do that -- `fetch` has no say
//! in which certificate it will accept, and asking a user to click through a
//! warning is how a self-signed server becomes an unauthenticated one. So the
//! webview never speaks to `sbxd` at all: it calls these, and `sbx-client` --
//! the same client the CLI uses -- makes the connection.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use sbx_client::{Incoming, Remote, Remotes, Sink};
use sbx_core::comments::{Comment, NewComment};
use sbx_core::events::Event;
use sbx_core::files::{Dir, FileText};
use sbx_core::git::{Against, FileDiff, Status as GitStatus};
use sbx_core::ops::{NewOptions, NewSession, Picked, Poll};
use sbx_core::policy::View as PolicyView;
use sbx_core::projects::{NewProject, Project};
use sbx_core::repos::Listing;
use sbx_core::session::Session;
use sbx_proto::stream::{Channel, ChannelId, ClientFrame, ServerFrame};
use sbx_core::integrations::View as IntegrationsView;
use sbx_core::tracker::Inbox;
use sbx_proto::{FailureKind, GitOp, McpOp, Reply, Request};
use serde::Serialize;
use tauri::{Emitter as _, Manager as _};

/// What a failed command looks like in the webview.
///
/// It was a bare string, on the grounds that every one of these is shown to a
/// person rather than branched on, and that when something did need to branch
/// this would grow a field rather than have the UI parse English. That is what
/// has happened: a worktree session answers `no-isolation` to a request for its
/// policy, and the difference between "there is no policy, here is why" and
/// "the policy could not be read" is the difference between a pane that states
/// a fact and one that looks broken.
///
/// The `message` is still the server's own words. What the kind decides is how
/// they are drawn.
#[derive(Debug, Clone, Serialize)]
struct Failed {
    kind: FailureKind,
    message: String,
}

fn to_message(e: sbx_client::Error) -> Failed {
    let message = e.to_string();
    Failed {
        // A transport error and a reply that was not a reply are both failures
        // of this request; only the server's own `Failure` carries a kind.
        kind: match e {
            sbx_client::Error::Failed(f) => f.kind,
            _ => FailureKind::Failed,
        },
        message,
    }
}

/// The same shape for a failure this side produced: no pairing, no such server.
fn failed(message: impl std::fmt::Display) -> Failed {
    Failed {
        kind: FailureKind::Failed,
        message: message.to_string(),
    }
}

/// A paired server, as the picker shows it. No token: it is a credential, and
/// the webview has no use for one it can never present.
#[derive(Debug, Clone, Serialize)]
struct ServerSummary {
    name: String,
    address: String,
}

#[tauri::command]
fn servers() -> Result<Vec<ServerSummary>, Failed> {
    let remotes = Remotes::load().map_err(failed)?;
    Ok(remotes
        .list()
        .iter()
        .map(|r| ServerSummary {
            name: r.name.clone(),
            address: r.address(),
        })
        .collect())
}

/// What pairing produced: the server just added, the list it is now in, and
/// the version of the `sbxd` that answered.
///
/// The version is in here because it is the proof: a pairing string is a claim
/// about an address, and a version that came back over the pinned connection is
/// that claim answered. Hand-written like [`ServerSummary`], being the bridge's
/// own shape rather than a message on the wire.
#[derive(Debug, Clone, Serialize)]
struct Paired {
    server: ServerSummary,
    servers: Vec<ServerSummary>,
    version: String,
}

/// Pair with a server from the window, rather than from a terminal.
///
/// The whole of the checking is `sbx_client::pair`, which is what `sbx connect`
/// calls -- so a string this window accepts is one the CLI would accept, and a
/// server it refuses is refused for the same stated reason. This command exists
/// because the alternative on Windows is installing a CLI whose other half
/// cannot run there at all.
#[tauri::command]
fn connect(pairing: String, name: Option<String>) -> Result<Paired, Failed> {
    let (remote, hello) = sbx_client::pair(&pairing, name.as_deref()).map_err(failed)?;
    Ok(Paired {
        server: ServerSummary {
            name: remote.name.clone(),
            address: remote.address(),
        },
        servers: servers()?,
        version: hello.version,
    })
}

/// Forget one, which is `sbx remotes --forget`.
///
/// It drops a token this machine holds and nothing on the server: the server
/// stops accepting one when `sbxd revoke` says so, which is the half that
/// matters if the token has been somewhere it should not.
#[tauri::command]
fn forget(name: String) -> Result<Vec<ServerSummary>, Failed> {
    let mut remotes = Remotes::load().map_err(failed)?;
    if !remotes.remove(&name) {
        return Err(failed(format!("no server named `{name}`")));
    }
    remotes.save().map_err(failed)?;
    servers()
}

fn remote(name: &str) -> Result<Remote, Failed> {
    let remotes = Remotes::load().map_err(failed)?;
    remotes.select(Some(name)).cloned().map_err(failed)
}

/// The reply a request was supposed to produce, or a message saying it was not.
///
/// A server answering a `Diff` with a `Policy` is a bug rather than a state to
/// handle, but it has to fail as something the window can show rather than as a
/// panic that takes the process with it.
macro_rules! expect_reply {
    ($reply:expr, $pattern:pat => $value:expr, $what:literal) => {
        match $reply {
            $pattern => Ok($value),
            _ => Err(crate::failed(format!(
                "the server answered something other than {}",
                $what
            ))),
        }
    };
}

#[tauri::command]
fn sessions(server: String) -> Result<Vec<Session>, Failed> {
    let reply = remote(&server)?.call(Request::Ls).map_err(to_message)?;
    expect_reply!(reply, Reply::Ls { sessions, .. } => sessions, "a session list")
}

#[tauri::command]
fn poll(server: String, name: String) -> Result<Poll, Failed> {
    let reply = remote(&server)?
        .call(Request::Poll { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Poll(poll) => poll, "a poll")
}

#[tauri::command]
fn policy(server: String, name: String) -> Result<PolicyView, Failed> {
    let reply = remote(&server)?
        .call(Request::Policy { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Policy(view) => view, "a policy")
}

#[tauri::command]
fn events(server: String, name: String) -> Result<Vec<Event>, Failed> {
    let reply = remote(&server)?
        .call(Request::Events { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Events { events } => events, "an event feed")
}

#[tauri::command]
fn diff(server: String, name: String) -> Result<String, Failed> {
    let reply = remote(&server)?
        .call(Request::Diff { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Diff { body } => body, "a diff")
}

/// The working copy as git describes it, and the result of doing something to
/// it. Both answer the same way, so the window has one shape to handle.
#[derive(Serialize)]
struct GitAnswer {
    said: String,
    status: GitStatus,
}

#[tauri::command]
fn git_status(server: String, name: String) -> Result<GitAnswer, Failed> {
    let reply = remote(&server)?
        .call(Request::GitStatus { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Git { said, status } => GitAnswer { said, status }, "a git status")
}

#[tauri::command]
fn git(server: String, name: String, action: GitOp) -> Result<GitAnswer, Failed> {
    let reply = remote(&server)?
        .call(Request::Git { name, action })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Git { said, status } => GitAnswer { said, status }, "a git status")
}

#[tauri::command]
fn git_diff(
    server: String,
    name: String,
    path: String,
    against: Against,
) -> Result<FileDiff, Failed> {
    let reply = remote(&server)?
        .call(Request::GitDiff {
            name,
            path,
            against,
        })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::GitDiff(diff) => diff, "a file diff")
}

/// One directory of a worktree's working copy.
#[tauri::command]
fn files(server: String, name: String, path: String) -> Result<Dir, Failed> {
    let reply = remote(&server)?
        .call(Request::Files { name, path })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Files(dir) => dir, "a directory")
}

#[tauri::command]
fn file(server: String, name: String, path: String) -> Result<FileText, Failed> {
    let reply = remote(&server)?
        .call(Request::File { name, path })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::File(text) => text, "a file")
}

/// The shells open beside a worktree's agent.
#[tauri::command]
fn shells(server: String, name: String) -> Result<Vec<String>, Failed> {
    let reply = remote(&server)?
        .call(Request::Shells { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Shells { shells } => shells, "a shell list")
}

#[tauri::command]
fn new_shell(server: String, name: String) -> Result<Vec<String>, Failed> {
    let reply = remote(&server)?
        .call(Request::NewShell { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Shells { shells } => shells, "a shell list")
}

#[tauri::command]
fn kill_shell(server: String, name: String, tmux: String) -> Result<Vec<String>, Failed> {
    let reply = remote(&server)?
        .call(Request::KillShell { name, tmux })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Shells { shells } => shells, "a shell list")
}

#[tauri::command]
fn comments(server: String, name: String) -> Result<Vec<Comment>, Failed> {
    let reply = remote(&server)?
        .call(Request::Comments { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Comments { comments } => comments, "a review")
}

#[tauri::command]
fn comment(server: String, name: String, comment: NewComment) -> Result<Vec<Comment>, Failed> {
    let reply = remote(&server)?
        .call(Request::Comment { name, comment })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Comments { comments } => comments, "a review")
}

#[tauri::command]
fn uncomment(server: String, name: String, id: u64) -> Result<Vec<Comment>, Failed> {
    let reply = remote(&server)?
        .call(Request::Uncomment { name, id })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Comments { comments } => comments, "a review")
}

/// Send the review to the agent. Answers with the message it sent.
#[tauri::command]
fn send_comments(server: String, name: String) -> Result<String, Failed> {
    let reply = remote(&server)?
        .call(Request::SendComments { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Told { message } => message, "a delivered review")
}

/// The projects on the server: what the tree is grouped under.
#[tauri::command]
fn projects(server: String) -> Result<Vec<Project>, Failed> {
    let reply = remote(&server)?.call(Request::Projects).map_err(to_message)?;
    expect_reply!(reply, Reply::Projects { projects } => projects, "a project list")
}

#[tauri::command]
fn new_project(server: String, project: NewProject) -> Result<Vec<Project>, Failed> {
    let reply = remote(&server)?
        .call(Request::NewProject(project))
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Projects { projects } => projects, "a project list")
}

#[tauri::command]
fn forget_project(server: String, name: String) -> Result<Vec<Project>, Failed> {
    let reply = remote(&server)?
        .call(Request::ForgetProject { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Projects { projects } => projects, "a project list")
}

/// The repositories the *server* can see, for the picker.
///
/// The server's and not this machine's: a checkout is only a way of naming a
/// remote, but which checkouts exist is a fact about the machine that will do
/// the cloning, and `repo_roots` is configured there.
#[tauri::command]
fn repos(server: String) -> Result<Listing, Failed> {
    let reply = remote(&server)?.call(Request::Repos).map_err(to_message)?;
    expect_reply!(reply, Reply::Repos(listing) => listing, "a repository list")
}

#[tauri::command]
fn inspect(server: String, path: String, branch: Option<String>) -> Result<Picked, Failed> {
    let reply = remote(&server)?
        .call(Request::Inspect { path, branch })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Inspect(picked) => picked, "repository facts")
}

#[tauri::command]
fn new_options(server: String) -> Result<NewOptions, Failed> {
    let reply = remote(&server)?
        .call(Request::NewOptions)
        .map_err(to_message)?;
    expect_reply!(reply, Reply::NewOptions(options) => options, "the create options")
}

/// Ask for a session. Answers as soon as the server has accepted the request,
/// which is before the session exists: it appears in the list a moment later,
/// in `creating`.
#[tauri::command]
fn create(server: String, session: NewSession) -> Result<String, Failed> {
    let reply = remote(&server)?
        .call(Request::Create(Box::new(session)))
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Created { name } => name, "a created session")
}

/// What the server holds on a session's behalf: the MCP catalog and what each
/// managed container is doing, the secret names, and the uploaded skills.
#[tauri::command]
fn integrations(server: String) -> Result<IntegrationsView, Failed> {
    let reply = remote(&server)?
        .call(Request::Integrations)
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Integrations(view) => view, "the integrations view")
}

#[tauri::command]
fn mcp(server: String, name: String, action: McpOp) -> Result<IntegrationsView, Failed> {
    let reply = remote(&server)?
        .call(Request::Mcp { name, action })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Integrations(view) => view, "the integrations view")
}

/// Store a secret on the server, or forget it.
///
/// The value goes one way. There is no command here that reads one back and no
/// reply that carries one, so a token typed into this window is in this
/// process's memory for the length of one request and in the server's store
/// afterwards -- and never in the webview at all.
#[tauri::command]
fn secret(server: String, name: String, value: Option<String>) -> Result<IntegrationsView, Failed> {
    let reply = remote(&server)?
        .call(Request::Secret { name, value })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Integrations(view) => view, "the integrations view")
}

/// Push this machine's own skills to the server.
///
/// **The reading and the packing happen on this side of the bridge**, which is
/// the whole reason this is a Tauri command and not something the webview does:
/// `~/.claude/skills` is on *this* machine, a webview cannot read it, and the
/// packing is `sbx_core::skills::payload` -- the same tar the seeder carries, so
/// there is one definition of what a packed skill is.
///
/// Every skill the agent would load, not a selection: a list to maintain here
/// would be a list that goes stale the first time you add a skill and forget.
#[tauri::command]
fn upload_skills(server: String) -> Result<IntegrationsView, Failed> {
    let mine = sbx_core::skills::local();
    if mine.is_empty() {
        return Err(failed(format!(
            "no skills in {} to upload",
            sbx_core::skills::host_skills_dir().display()
        )));
    }
    let mut uploads = Vec::new();
    let mut problems = Vec::new();
    for skill in &mine {
        match sbx_core::skills::payload(&skill.source) {
            Ok(tar) => uploads.push(sbx_core::skills::Upload {
                name: skill.name.clone(),
                origin: skill.source.display().to_string(),
                tar,
            }),
            // One skill that has grown a virtualenv should not stop the rest.
            Err(e) => problems.push(format!("{}: {e}", skill.name)),
        }
    }
    if uploads.is_empty() {
        return Err(failed(format!(
            "none of the skills could be packed: {}",
            problems.join("; ")
        )));
    }
    let reply = remote(&server)?
        .call(Request::UploadSkills { skills: uploads })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Integrations(view) => view, "the integrations view")
}

#[tauri::command]
fn forget_skill(server: String, name: String) -> Result<IntegrationsView, Failed> {
    let reply = remote(&server)?
        .call(Request::ForgetSkill { name })
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Integrations(view) => view, "the integrations view")
}

/// What this machine has to upload, for a screen that wants to say so before
/// anything is sent.
#[tauri::command]
fn my_skills() -> Vec<String> {
    sbx_core::skills::local()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// The task inbox: what the server's trackers say is assigned to you.
///
/// Read on the server, with the credentials in its store, so this window shows
/// a list and never holds a token. Whatever could not be read comes back beside
/// what could -- see `sbx_core::tracker`.
#[tauri::command]
fn tasks(server: String) -> Result<Inbox, Failed> {
    let reply = remote(&server)?.call(Request::Tasks).map_err(to_message)?;
    expect_reply!(reply, Reply::Tasks(inbox) => inbox, "a task inbox")
}

/// The one streaming connection, and which server it is to.
///
/// One per window rather than one per pane: the protocol multiplexes, so four
/// terminals and four feeds share a socket, a token check and a reconnect. See
/// [`sbx_proto::stream`].
#[derive(Default)]
struct Streaming {
    open: Mutex<Option<Open>>,
}

struct Open {
    server: String,
    sink: Sink,
}

/// Every frame the server sends, as a window event.
///
/// One event name for all of them rather than one per channel: the frame
/// already carries its channel id, and a listener per channel would mean the
/// frontend unsubscribing correctly every time a pane closes -- which it would
/// eventually not.
const FRAME: &str = "sbx://frame";

/// Connect if the window is not already connected to this server.
///
/// Switching servers replaces the connection, which also ends every channel on
/// it -- correct, since the channels named sessions on the old one.
fn connected(app: &tauri::AppHandle, server: &str) -> Result<(), Failed> {
    let state = app.state::<Streaming>();
    let mut open = state.open.lock().map_err(failed)?;

    if open.as_ref().is_some_and(|o| o.server == server) {
        return Ok(());
    }

    let (sink, frames) = remote(server)?.stream().map_err(to_message)?.split();

    let handle = app.clone();
    std::thread::spawn(move || {
        for message in frames {
            match message {
                Incoming::Frame(frame) => {
                    let _ = handle.emit(FRAME, *frame);
                }
                // The connection has gone. Every open channel is closed by it,
                // so each is told rather than left waiting for output that will
                // not come.
                Incoming::Ended(reason) => {
                    let _ = handle.emit(
                        FRAME,
                        ServerFrame::Closed {
                            id: ALL_CHANNELS,
                            reason: reason.or_else(|| Some("the connection ended".into())),
                        },
                    );
                    break;
                }
            }
        }
    });

    *open = Some(Open {
        server: server.to_string(),
        sink,
    });
    Ok(())
}

/// The id a `Closed` carries when it is about the whole connection rather than
/// one channel. Not a real channel id: no client allocates it.
const ALL_CHANNELS: ChannelId = ChannelId::MAX;

fn send(app: &tauri::AppHandle, frame: ClientFrame) -> Result<(), Failed> {
    let state = app.state::<Streaming>();
    let open = state.open.lock().map_err(failed)?;
    let Some(open) = open.as_ref() else {
        return Err(failed("not connected"));
    };
    open.sink
        .send(frame)
        .then_some(())
        .ok_or_else(|| failed("the connection has ended"))
}

#[tauri::command]
fn watch(
    app: tauri::AppHandle,
    server: String,
    id: ChannelId,
    channel: Channel,
) -> Result<(), Failed> {
    connected(&app, &server)?;
    send(&app, ClientFrame::Open { id, channel })
}

#[tauri::command]
fn unwatch(app: tauri::AppHandle, id: ChannelId) -> Result<(), Failed> {
    send(&app, ClientFrame::Close { id })
}

#[tauri::command]
fn terminal_input(app: tauri::AppHandle, id: ChannelId, data: String) -> Result<(), Failed> {
    send(&app, ClientFrame::Input { id, data })
}

#[tauri::command]
fn terminal_resize(
    app: tauri::AppHandle,
    id: ChannelId,
    cols: u16,
    rows: u16,
) -> Result<(), Failed> {
    send(&app, ClientFrame::Resize { id, cols, rows })
}

fn main() {
    // Wayland is left alone: WSLg is a Wayland compositor and the window is a
    // Wayland client there. `GDK_BACKEND=x11` is a way to make X11 screenshot
    // tooling see the surface, not a way to run.
    tauri::Builder::default()
        .manage(Streaming::default())
        .setup(|app| {
            // Debug builds open the inspector. There is no other way to see a
            // console message from inside this window.
            #[cfg(debug_assertions)]
            {
                use tauri::Manager as _;
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            servers,
            connect,
            forget,
            sessions,
            poll,
            policy,
            events,
            diff,
            git_status,
            git,
            git_diff,
            files,
            file,
            shells,
            new_shell,
            kill_shell,
            comments,
            comment,
            uncomment,
            send_comments,
            projects,
            new_project,
            forget_project,
            repos,
            inspect,
            new_options,
            create,
            integrations,
            mcp,
            secret,
            upload_skills,
            forget_skill,
            my_skills,
            tasks,
            watch,
            unwatch,
            terminal_input,
            terminal_resize
        ])
        .run(tauri::generate_context!())
        .expect("the window could not be created");
}

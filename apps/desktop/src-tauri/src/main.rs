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
use sbx_proto::{GitOp, Reply, Request};
use serde::Serialize;
use tauri::{Emitter as _, Manager as _};

/// What a failed command looks like in the webview.
///
/// A string, because every one of these is shown to a person rather than
/// branched on. The typed `FailureKind` the protocol carries is what a *client*
/// branches on, and this application does not yet need to -- when it does, this
/// grows a field rather than the UI parsing English.
type Failed = String;

fn to_message(e: sbx_client::Error) -> Failed {
    e.to_string()
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
    let remotes = Remotes::load().map_err(|e| e.to_string())?;
    Ok(remotes
        .list()
        .iter()
        .map(|r| ServerSummary {
            name: r.name.clone(),
            address: r.address(),
        })
        .collect())
}

fn remote(name: &str) -> Result<Remote, Failed> {
    let remotes = Remotes::load().map_err(|e| e.to_string())?;
    remotes.select(Some(name)).cloned()
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
            _ => Err(format!("the server answered something other than {}", $what)),
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
        .call(Request::Create(session))
        .map_err(to_message)?;
    expect_reply!(reply, Reply::Created { name } => name, "a created session")
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
    let mut open = state.open.lock().map_err(|e| e.to_string())?;

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
    let open = state.open.lock().map_err(|e| e.to_string())?;
    let Some(open) = open.as_ref() else {
        return Err("not connected".into());
    };
    open.sink
        .send(frame)
        .then_some(())
        .ok_or_else(|| "the connection has ended".to_string())
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
            watch,
            unwatch,
            terminal_input,
            terminal_resize
        ])
        .run(tauri::generate_context!())
        .expect("the window could not be created");
}

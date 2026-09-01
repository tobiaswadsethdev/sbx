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

use sbx_client::{Remote, Remotes};
use sbx_core::events::Event;
use sbx_core::ops::Poll;
use sbx_core::policy::View as PolicyView;
use sbx_core::session::Session;
use sbx_proto::{Reply, Request};
use serde::Serialize;

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

fn main() {
    // Wayland is left alone: WSLg is a Wayland compositor and the window is a
    // Wayland client there. `GDK_BACKEND=x11` is a way to make X11 screenshot
    // tooling see the surface, not a way to run.
    tauri::Builder::default()
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
            servers, sessions, poll, policy, events, diff
        ])
        .run(tauri::generate_context!())
        .expect("the window could not be created");
}

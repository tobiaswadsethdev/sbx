//! The websocket: one connection, several channels.
//!
//! Each channel is a task producing [`ServerFrame`]s into one queue, and a
//! single writer drains that queue onto the socket. That shape is what keeps
//! the terminal responsive while an events poll is waiting on the gateway: the
//! slow channel blocks itself and nothing else.
//!
//! **Polling lives here, not in the client.** A client that asked `/rpc` for
//! events every second would spend a TLS handshake per session per second to be
//! told nothing had changed. The server is next to the gateway, so it does the
//! asking and sends only what is new -- which is also the only way a second
//! client watching the same session does not double the load on it.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use openshell_client::CliClient;
use sbx_core::events::Event;
use sbx_core::ops;
use sbx_core::session::Session;
use sbx_core::store::Store;
use sbx_proto::stream::{Channel, ChannelId, ClientFrame, ServerFrame, bytes};
use tokio::sync::mpsc;

/// How often a feed or a status channel asks the gateway.
///
/// Slower than the terminal interface's own second, deliberately. Each of these
/// is an exec against the sandbox, execs are serialised gateway-side, and a
/// server may be answering several clients about several sessions at once --
/// where the TUI was one process watching one machine.
const POLL: Duration = Duration::from_secs(2);

/// How much output may queue for a client that has stopped reading.
///
/// A terminal that produces faster than the socket drains -- `yes`, a build --
/// must not grow a queue until the server runs out of memory. When this fills,
/// the channel closes and says so, which is recoverable; the alternative is not.
const BACKLOG: usize = 256;

// Checked where they are written rather than in a test, because both are
// judgements about a number and a test that folds to `assert!(true)` proves
// nothing at run time.
const _: () = assert!(
    BACKLOG > 0 && BACKLOG <= 1024,
    "a backlog this large is not a bound"
);
const _: () = assert!(
    POLL.as_secs() >= 1 && POLL.as_secs() <= 5,
    "a server polls for every client at once; a TUI's own second is too fast here"
);

/// What a terminal channel accepts while it is running.
enum ToTerminal {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

pub async fn run(socket: WebSocket) {
    let (mut sink, mut source) = {
        use futures_util::StreamExt as _;
        socket.split()
    };

    let (out, mut queued) = mpsc::channel::<ServerFrame>(BACKLOG);

    // One writer. Every channel produces into `out`, so nothing else ever
    // touches the socket and frames cannot interleave halfway.
    let writer = tokio::spawn(async move {
        use futures_util::SinkExt as _;
        while let Some(frame) = queued.recv().await {
            let Ok(text) = serde_json::to_string(&frame) else {
                continue;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let mut channels: HashMap<ChannelId, ChannelHandle> = HashMap::new();

    use futures_util::StreamExt as _;
    while let Some(Ok(message)) = source.next().await {
        let Message::Text(text) = message else {
            // Binary and ping/pong are not part of this protocol; axum answers
            // pings itself.
            continue;
        };
        let Ok(frame) = serde_json::from_str::<ClientFrame>(&text) else {
            continue;
        };

        match frame {
            ClientFrame::Open { id, channel } => {
                // Re-opening an id closes what was there. A client that has
                // lost track is better served by the newer intent than by a
                // silent refusal.
                if let Some(old) = channels.remove(&id) {
                    old.shutdown().await;
                }
                match open(id, channel, out.clone()).await {
                    Ok(handle) => {
                        channels.insert(id, handle);
                    }
                    Err(reason) => {
                        let _ = out
                            .send(ServerFrame::Closed {
                                id,
                                reason: Some(reason),
                            })
                            .await;
                    }
                }
            }
            ClientFrame::Close { id } => {
                if let Some(handle) = channels.remove(&id) {
                    handle.shutdown().await;
                    let _ = out.send(ServerFrame::Closed { id, reason: None }).await;
                }
            }
            ClientFrame::Input { id, data } => {
                if let (Some(handle), Some(raw)) = (channels.get(&id), bytes::decode(&data)) {
                    handle.send(ToTerminal::Input(raw)).await;
                }
            }
            ClientFrame::Resize { id, cols, rows } => {
                if let Some(handle) = channels.get(&id) {
                    handle.send(ToTerminal::Resize { cols, rows }).await;
                }
            }
        }
    }

    // The socket has gone. Every channel goes with it, and a terminal detaches
    // rather than being killed -- see `terminal`.
    for (_, handle) in channels.drain() {
        handle.shutdown().await;
    }
    drop(out);
    let _ = writer.await;
}

struct ChannelHandle {
    task: tokio::task::JoinHandle<()>,
    to_terminal: Option<mpsc::Sender<ToTerminal>>,
}

impl ChannelHandle {
    async fn send(&self, message: ToTerminal) {
        if let Some(tx) = &self.to_terminal {
            let _ = tx.send(message).await;
        }
    }

    /// End the channel.
    ///
    /// A terminal is asked to detach and given a moment to do it; everything
    /// else is simply dropped. See `terminal` for why the difference matters.
    async fn shutdown(self) {
        if let Some(tx) = &self.to_terminal {
            let _ = tx.send(ToTerminal::Input(DETACH.to_vec())).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.task.abort();
    }
}

/// `Ctrl-b d`: what a person types to leave a tmux session.
///
/// The alternative would be killing the exec, and that is the one thing this
/// must not do. Killing an `openshell exec --tty` wedges the exec path for that
/// sandbox until it is recreated -- the session survives but nothing can reach
/// it again, including the poll that would have told you so.
const DETACH: &[u8] = b"\x02d";

async fn open(
    id: ChannelId,
    channel: Channel,
    out: mpsc::Sender<ServerFrame>,
) -> Result<ChannelHandle, String> {
    let name = channel.session().to_string();
    let session = tokio::task::spawn_blocking(move || {
        Store::load()
            .map_err(|e| format!("could not read the session cache: {e}"))?
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("no session named `{name}`"))
    })
    .await
    .map_err(|e| e.to_string())??;

    let _ = out.send(ServerFrame::Opened { id }).await;

    Ok(match channel {
        Channel::Events { .. } => ChannelHandle {
            task: tokio::spawn(events(id, session, out)),
            to_terminal: None,
        },
        Channel::Status { .. } => ChannelHandle {
            task: tokio::spawn(status(id, session, out)),
            to_terminal: None,
        },
        Channel::Terminal { tmux, .. } => {
            let (tx, rx) = mpsc::channel(64);
            // Defaulted here rather than in the worker, so everything below
            // this point has one name for its target and no opinion about
            // which tmux session is special.
            let target = tmux.clone().unwrap_or_else(|| session.tmux.clone());
            ChannelHandle {
                task: tokio::spawn(terminal(id, session, target, out, rx)),
                to_terminal: Some(tx),
            }
        }
    })
}

/// The allow/deny feed, as decisions are made.
///
/// The first frame carries the recent log and every frame after it carries the
/// difference, keyed on what the core already considers the same event -- so a
/// decision the gateway's window reports twice is sent once.
async fn events(id: ChannelId, session: Session, out: mpsc::Sender<ServerFrame>) {
    let mut seen: HashSet<(u64, String, String)> = HashSet::new();

    loop {
        let s = session.clone();
        let fetched = tokio::task::spawn_blocking(move || ops::events(&CliClient::default(), &s))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));

        match fetched {
            Ok(all) => {
                let fresh: Vec<Event> = all.into_iter().filter(|e| seen.insert(e.key())).collect();
                if !fresh.is_empty()
                    && out
                        .send(ServerFrame::Events { id, events: fresh })
                        .await
                        .is_err()
                {
                    return;
                }
            }
            // A log that cannot be read is usually a sandbox that has just gone.
            // The channel says so and stops rather than retrying into nothing.
            Err(reason) => {
                let _ = out
                    .send(ServerFrame::Closed {
                        id,
                        reason: Some(reason),
                    })
                    .await;
                return;
            }
        }

        tokio::time::sleep(POLL).await;
    }
}

/// What the agent is doing, sent when it changes.
///
/// Only on change, which is what makes this cheaper than the client asking:
/// most polls of a session that is thinking return the same answer.
async fn status(id: ChannelId, session: Session, out: mpsc::Sender<ServerFrame>) {
    let mut last: Option<ops::Poll> = None;

    loop {
        let s = session.clone();
        let Ok(poll) =
            tokio::task::spawn_blocking(move || ops::poll(&CliClient::default(), &s)).await
        else {
            return;
        };

        if last.as_ref() != Some(&poll) {
            last = Some(poll.clone());
            if out.send(ServerFrame::Status { id, poll }).await.is_err() {
                return;
            }
        }

        tokio::time::sleep(POLL).await;
    }
}

/// The agent's terminal.
///
/// The same `exec --tty` and the same shell that `sbx attach` runs -- one
/// definition, in `ops::attach_script`, so an embedded terminal and a terminal
/// on the machine itself cannot end up attaching differently.
///
/// **It runs under a pty on *this* side, and it has to.** `openshell sandbox
/// exec --tty` allocates a pty at the sandbox end, which is enough for the
/// process in there to have a terminal -- `tty` reports one, `test -t 0`
/// succeeds -- but the CLI will not proxy interactively through plain pipes.
/// Measured against 0.0.110: with stdin closed it writes the 600-odd bytes of
/// tmux's redraw and carries on; with stdin an open pipe it writes nothing at
/// all, whether or not anything has been sent to it. The channel opens, the
/// child runs, and no byte ever arrives.
///
/// So the pty is local as well, and the child is spawned into it exactly as a
/// terminal emulator would. That is what `interactive_exec_argv` is shaped for:
/// spawning under a pty needs the program and its arguments apart.
///
/// Resizing is then the pty's own, rather than an exec running
/// `tmux resize-window`. Better in two ways: the client's size reaches tmux the
/// way any terminal's does, and there is no second exec to queue behind this
/// one -- execs are serialised per sandbox, so an exec issued while the attach
/// is holding the path is an exec that waits for it to finish.
async fn terminal(
    id: ChannelId,
    session: Session,
    tmux: String,
    out: mpsc::Sender<ServerFrame>,
    mut input: mpsc::Receiver<ToTerminal>,
) {
    let (from_pty, mut output) = mpsc::channel::<Vec<u8>>(BACKLOG);
    let (to_pty, pty_input) = std::sync::mpsc::channel::<ToTerminal>();

    let worker = {
        let session = session.clone();
        std::thread::spawn(move || pty_worker(session, tmux, from_pty, pty_input))
    };

    let mut reason = None;
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                // The worker has finished: the child exited, or the pty failed.
                None => break,
                Some(raw) => {
                    let frame = ServerFrame::Output { id, data: bytes::encode(&raw) };
                    // A full queue is a client that has stopped draining.
                    // Ending the channel is recoverable; growing is not.
                    if out.try_send(frame).is_err() {
                        reason = Some("the client stopped reading its terminal".to_string());
                        break;
                    }
                }
            },
            message = input.recv() => match message {
                None => break,
                Some(message) => {
                    if to_pty.send(message).is_err() {
                        break;
                    }
                }
            },
        }
    }

    // Dropped rather than killed. Closing this end hangs up the pty, the child
    // sees it and exits; nothing sends it a signal. See `DETACH`.
    drop(to_pty);
    let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    let _ = out.send(ServerFrame::Closed { id, reason }).await;
}

/// The blocking half: a pty, a child in it, and the two directions of traffic.
///
/// Its own thread rather than `spawn_blocking`, because it outlives a single
/// call and holds a reader that blocks until there is something to read.
fn pty_worker(
    session: Session,
    tmux: String,
    out: mpsc::Sender<Vec<u8>>,
    input: std::sync::mpsc::Receiver<ToTerminal>,
) {
    use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

    let client = CliClient::default();
    let script = ops::attach_script(&tmux);
    let argv = client.interactive_exec_argv(&session.sandbox, &["sh", "-c", &script]);

    // A size to start with. The client sends its own as soon as it has one, and
    // until then this is what tmux draws for -- so it is the session's own
    // scrape size rather than an 80x24 the pane would have to reflow out of.
    let (cols, rows) = sbx_core::session::SCRAPE_SIZE;
    let pty = NativePtySystem::default();
    let Ok(pair) = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        return;
    };

    let mut command = CommandBuilder::new(&argv[0]);
    command.args(&argv[1..]);
    // The gateway passes nothing of the image's environment through, and tmux
    // needs to know it is talking to something. `ops::attach_script` sets the
    // locale inside; this is the outside half.
    command.env("TERM", "xterm-256color");

    let Ok(mut child) = pair.slave.spawn_command(command) else {
        return;
    };
    // The slave is the child's now. Holding a copy would keep the pty from ever
    // reporting end of file, so the reader below would block for ever.
    drop(pair.slave);

    let Ok(mut reader) = pair.master.try_clone_reader() else {
        return;
    };
    let Ok(mut writer) = pair.master.take_writer() else {
        return;
    };

    let reading = std::thread::spawn(move || {
        let mut buf = vec![0u8; 8192];
        while let Ok(n) = std::io::Read::read(&mut reader, &mut buf) {
            if n == 0 || out.blocking_send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    while let Ok(message) = input.recv() {
        match message {
            ToTerminal::Input(raw) => {
                if std::io::Write::write_all(&mut writer, &raw).is_err()
                    || std::io::Write::flush(&mut writer).is_err()
                {
                    break;
                }
            }
            // The client's window, as a real terminal's size change. tmux
            // resizes from it the way it would for any client.
            ToTerminal::Resize { cols, rows } => {
                let _ = pair.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }

    // The caller has gone. Hanging up the pty is what tells the child, and the
    // detach it was sent first is what lets it leave tmux cleanly.
    drop(writer);
    drop(pair.master);
    let _ = child.wait();
    let _ = reading.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing a terminal channel must never do is kill its exec: that
    /// wedges the exec path for the whole sandbox. The detach sequence is what
    /// replaces it, and it is `Ctrl-b d`.
    #[test]
    fn the_detach_sequence_is_the_one_tmux_answers_to() {
        assert_eq!(DETACH, b"\x02d");
        assert_eq!(DETACH[0], 0x02, "Ctrl-b");
        assert_eq!(DETACH[1], b'd');
    }
}

//! The streaming half, from the client's end.
//!
//! Blocking, and on a thread of its own, which matches everything else on this
//! side: the CLI and the TUI are blocking, and the desktop application's Rust
//! half can spare a thread per connection. What it must not do is invent a
//! second way to trust a certificate -- so the `rustls::ClientConfig` here is
//! the one [`super::pin`] builds, handed to tungstenite as its connector.
//!
//! Frames come back on a channel rather than through a callback, so the caller
//! decides what thread they are handled on. The desktop application turns them
//! into Tauri events; a terminal client could draw them directly.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use sbx_proto::stream::{ClientFrame, ServerFrame};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client::IntoClientRequest};

use super::pin::client_config;
use super::{Error, Remote};

/// A live connection: frames arrive on [`Stream::frames`], and go out through
/// [`Stream::send`].
pub struct Stream {
    sink: Sink,
    incoming: Receiver<Incoming>,
}

/// What the reader thread reports.
#[derive(Debug)]
pub enum Incoming {
    Frame(Box<ServerFrame>),
    /// The connection has ended, with a reason when there was one. Always the
    /// last thing on the channel.
    Ended(Option<String>),
}

enum Outgoing {
    Frame(ClientFrame),
    Stop,
}

impl Remote {
    /// Open the streaming connection.
    ///
    /// One socket per client, not one per channel: see
    /// [`sbx_proto::stream`] for why.
    pub fn stream(&self) -> Result<Stream, Error> {
        let url = format!("wss://{}:{}/ws", self.host, self.port);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| Error::Server(format!("could not build the request: {e}")))?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token)
                .parse()
                .map_err(|_| Error::Server("the token is not a valid header".into()))?,
        );

        let config = client_config(&self.fingerprint)
            .map_err(|e| Error::Server(format!("could not set up TLS: {e}")))?;

        let (socket, _response) = tungstenite::client_tls_with_config(
            request,
            // The same dial the request half makes: a per-address timeout and
            // the memory of which address answered. `TcpStream::connect` on a
            // hostname walks the addresses with the kernel's own retry behind
            // each, which on Windows is twenty seconds of nothing happening
            // before `localhost` falls back from `::1` to `127.0.0.1`.
            super::http::connect(&self.host, self.port)
                .map_err(|e| Error::Server(format!("could not reach the server: {e}")))?,
            None,
            Some(tungstenite::Connector::Rustls(Arc::new(config))),
        )
        .map_err(|e| Error::Server(describe_handshake(e)))?;

        Ok(Stream::spawn(socket))
    }
}

type Socket = WebSocket<MaybeTlsStream<std::net::TcpStream>>;

impl Stream {
    fn spawn(socket: Socket) -> Self {
        let (to_caller, incoming) = channel::<Incoming>();
        let (outgoing, from_caller) = channel::<Outgoing>();

        // Two threads and no shared lock on the socket would be a data race, so
        // one thread owns it and alternates: drain whatever the caller has
        // queued, then read with a timeout. The timeout is what keeps a silent
        // connection from blocking sends behind a read that never returns.
        std::thread::spawn(move || pump(socket, to_caller, from_caller));

        Stream {
            sink: Sink { outgoing },
            incoming,
        }
    }

    /// Queue a frame. Returns `false` once the connection has ended.
    pub fn send(&self, frame: ClientFrame) -> bool {
        self.sink.send(frame)
    }

    /// Everything the server has said. Ends with [`Incoming::Ended`].
    pub fn frames(&self) -> &Receiver<Incoming> {
        &self.incoming
    }

    /// Split the two directions apart.
    ///
    /// A `Receiver` is `!Sync`, so a `Stream` cannot be shared state -- and the
    /// desktop application needs exactly that: the receiver moved onto a thread
    /// that turns frames into window events, and the sending half held where
    /// every command can reach it.
    pub fn split(self) -> (Sink, Receiver<Incoming>) {
        (self.sink, self.incoming)
    }
}

/// The sending half of a connection.
///
/// Dropping it ends the connection, the same as dropping a whole [`Stream`]:
/// the reader thread notices the caller has gone.
pub struct Sink {
    outgoing: Sender<Outgoing>,
}

impl Sink {
    /// Queue a frame. Returns `false` once the connection has ended.
    pub fn send(&self, frame: ClientFrame) -> bool {
        self.outgoing.send(Outgoing::Frame(frame)).is_ok()
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = self.outgoing.send(Outgoing::Stop);
    }
}

fn pump(mut socket: Socket, to_caller: Sender<Incoming>, from_caller: Receiver<Outgoing>) {
    // Short, because it is the granularity at which a queued send is noticed,
    // not a network timeout. Long enough not to spin.
    const SLICE: std::time::Duration = std::time::Duration::from_millis(50);

    if let MaybeTlsStream::Rustls(stream) = socket.get_mut() {
        let _ = stream.get_mut().set_read_timeout(Some(SLICE));
    }

    let ended = loop {
        // Everything the caller has queued, first: a keystroke waiting behind a
        // read is a terminal that feels broken.
        loop {
            match from_caller.try_recv() {
                Ok(Outgoing::Frame(frame)) => {
                    let Ok(text) = serde_json::to_string(&frame) else {
                        continue;
                    };
                    if socket.send(Message::Text(text.into())).is_err() {
                        break;
                    }
                }
                Ok(Outgoing::Stop) => {
                    let _ = socket.close(None);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The caller dropped the `Stream`.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    return;
                }
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str::<ServerFrame>(&text) {
                Ok(frame) => {
                    if to_caller.send(Incoming::Frame(Box::new(frame))).is_err() {
                        return;
                    }
                }
                // A frame this build cannot read is a newer server, not a
                // broken connection: the other channels still work.
                Err(_) => continue,
            },
            Ok(Message::Close(_)) => break None,
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e)) if would_block(&e) => continue,
            Err(tungstenite::Error::ConnectionClosed) => break None,
            Err(e) => break Some(describe(e)),
        }
    };

    let _ = to_caller.send(Incoming::Ended(ended));
}

fn would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// The handshake's own error type, which wraps the one below once the attempt
/// has actually failed rather than merely not finished yet.
fn describe_handshake<S>(e: tungstenite::HandshakeError<S>) -> String
where
    S: tungstenite::handshake::HandshakeRole,
{
    match e {
        tungstenite::HandshakeError::Failure(e) => describe(e),
        tungstenite::HandshakeError::Interrupted(_) => "the handshake did not finish".into(),
    }
}

/// A tungstenite error, said in terms of what went wrong rather than which
/// layer noticed. The certificate one is the one worth recognising: it is a
/// server that was rebuilt, or one that is not the server that was paired with.
fn describe(e: tungstenite::Error) -> String {
    tidy(&e.to_string())
}

/// The wording, separated from where it came from.
///
/// Split out so it can be tested without asserting on an upstream crate's exact
/// phrasing, which is not a promise anyone made.
fn tidy(text: &str) -> String {
    if text.contains("certificate") || text.contains("paired with") {
        // rustls renders a verifier's own error behind `unexpected error:`,
        // which is noise in front of a message written to be read. The same
        // stripping the request path does.
        return text
            .strip_prefix("unexpected error: ")
            .unwrap_or(text)
            .to_string();
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read timeout is how a queued send gets noticed, so it must not be
    /// reported as the connection failing.
    #[test]
    fn a_read_timeout_is_not_a_failure() {
        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut] {
            assert!(would_block(&std::io::Error::new(kind, "slice")));
        }
        assert!(!would_block(&std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "gone"
        )));
    }

    /// A pinning failure is the one error here worth recognising, and its own
    /// wording is the useful part -- it names both fingerprints.
    #[test]
    fn a_pinning_failure_loses_the_prefix_and_nothing_else() {
        let pinned = "unexpected error: the server presented certificate aa, \
                      and this connection was paired with bb";
        let out = tidy(pinned);
        assert!(!out.starts_with("unexpected error:"), "{out}");
        assert!(out.contains("presented certificate aa"), "{out}");
        assert!(out.contains("paired with bb"), "{out}");
    }

    /// Anything else passes through. This is not the place to reword the
    /// network's own errors, and asserting on an upstream crate's exact
    /// phrasing is not a promise anyone made -- which is why `tidy` is tested
    /// here rather than `describe`.
    #[test]
    fn other_failures_are_left_alone() {
        for text in [
            "Connection reset by peer",
            "unexpected error: something else",
        ] {
            assert_eq!(tidy(text), text);
        }
    }
}

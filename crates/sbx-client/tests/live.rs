//! Tests that need a paired server and a real session.
//!
//! `#[ignore]`d, like the gateway contract tests in `openshell-client`: the
//! suite stays hermetic for anyone without a gateway, and these are run by hand
//! when the streaming half changes.
//!
//! ```sh
//! sbx new --repo <url> --task "..." --name live-stream
//! cargo test -p sbx-client -- --ignored --test-threads=1
//! ```
//!
//! The session name is `live-stream` unless `SBX_LIVE_SESSION` says otherwise,
//! and the server is whichever one is paired.

use std::time::{Duration, Instant};

use sbx_client::{Incoming, Remotes};
use sbx_proto::stream::{Channel, ClientFrame, ServerFrame, bytes};

fn session() -> String {
    std::env::var("SBX_LIVE_SESSION").unwrap_or_else(|_| "live-stream".into())
}

fn stream() -> sbx_client::Stream {
    let remotes = Remotes::load().expect("remotes");
    let remote = remotes.select(None).expect("one paired server");
    remote.stream().expect("the websocket opened")
}

/// Wait for a frame the predicate accepts, or give up.
fn wait_for<T>(
    stream: &sbx_client::Stream,
    within: Duration,
    mut f: impl FnMut(&ServerFrame) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match stream.frames().recv_timeout(Duration::from_millis(500)) {
            Ok(Incoming::Frame(frame)) => {
                if let Some(v) = f(&frame) {
                    return Some(v);
                }
            }
            Ok(Incoming::Ended(reason)) => panic!("the connection ended: {reason:?}"),
            Err(_) => continue,
        }
    }
    None
}

/// The channel this whole increment is for: bytes out of the agent's tmux.
///
/// The pty is at the sandbox end, so nothing on this side needs a terminal --
/// which is the property that lets a server host it at all, and the one worth
/// checking against a real gateway rather than assuming.
#[test]
#[ignore = "needs a paired server and a live session"]
fn a_terminal_channel_produces_the_agents_screen() {
    let stream = stream();
    stream.send(ClientFrame::Open {
        id: 1,
        channel: Channel::Terminal { session: session() },
    });
    stream.send(ClientFrame::Resize {
        id: 1,
        cols: 100,
        rows: 30,
    });

    let opened = wait_for(&stream, Duration::from_secs(10), |f| {
        matches!(f, ServerFrame::Opened { id: 1 }).then_some(())
    });
    assert!(opened.is_some(), "the channel never opened");

    let output = wait_for(&stream, Duration::from_secs(20), |f| match f {
        ServerFrame::Output { data, .. } => bytes::decode(data),
        ServerFrame::Closed { reason, .. } => panic!("the terminal closed: {reason:?}"),
        _ => None,
    })
    .expect("no output from the terminal");

    assert!(!output.is_empty());
    // tmux redraws on attach, so the first thing through is escape sequences.
    // Asserting on the agent's own text would be asserting on Claude Code's
    // banner, which is not a contract.
    assert!(
        output.contains(&0x1b) || output.iter().any(|b| b.is_ascii_graphic()),
        "output carried neither escapes nor text: {output:?}"
    );
}

/// Closing a terminal must detach, not kill. A killed `exec --tty` wedges the
/// exec path for the sandbox, so the check is that the *next* channel still
/// works -- which it cannot if the first one broke the path.
#[test]
#[ignore = "needs a paired server and a live session"]
fn a_terminal_can_be_opened_again_after_being_closed() {
    for attempt in 1..=2 {
        let stream = stream();
        stream.send(ClientFrame::Open {
            id: 1,
            channel: Channel::Terminal { session: session() },
        });

        let got = wait_for(&stream, Duration::from_secs(20), |f| match f {
            ServerFrame::Output { data, .. } => bytes::decode(data),
            _ => None,
        });
        assert!(got.is_some(), "attempt {attempt} produced no output");

        stream.send(ClientFrame::Close { id: 1 });
        drop(stream);
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The feed sends the recent log once and then only what is new. A second
/// batch repeating the first would fill a pane with duplicates within a minute.
#[test]
#[ignore = "needs a paired server and a live session"]
fn the_events_channel_does_not_repeat_itself() {
    let stream = stream();
    stream.send(ClientFrame::Open {
        id: 7,
        channel: Channel::Events { session: session() },
    });

    let mut keys: Vec<(u64, String, String)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if let Ok(Incoming::Frame(frame)) = stream.frames().recv_timeout(Duration::from_millis(500))
            && let ServerFrame::Events { events, .. } = *frame
        {
            for e in events {
                let key = e.key();
                assert!(!keys.contains(&key), "sent twice: {key:?}");
                keys.push(key);
            }
        }
    }
    assert!(!keys.is_empty(), "the feed said nothing at all");
}

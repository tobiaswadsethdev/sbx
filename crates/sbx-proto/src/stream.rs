//! The half of the protocol that pushes: one websocket, several channels.
//!
//! `/rpc` answers a question. This is for the three things a client wants
//! *told*: what the agent's screen is doing, what the gateway has just decided,
//! and the terminal itself. Polling all three over `/rpc` would be a request per
//! session per second per client, each one a TLS handshake, to say "nothing has
//! changed" nearly every time.
//!
//! **One connection, not one per channel.** A client watching four sessions
//! wants a terminal and a feed and a status for each; twelve sockets is twelve
//! handshakes, twelve token checks, and twelve things to notice have dropped.
//! Multiplexing costs an integer per frame and saves all of that -- and the
//! reconnect, when it comes, is one reconnect.
//!
//! Frames are JSON, like everything else here, so a connection stays readable in
//! a log. Terminal bytes are the exception that proves it: they are base64 in a
//! JSON string, because a PTY emits arbitrary bytes and a JSON string must be
//! valid UTF-8. A read that splits a multi-byte character mid-sequence -- which
//! happens constantly, since reads land wherever they land -- would otherwise be
//! unencodable, or worse, silently replaced.

use serde::{Deserialize, Serialize};

use sbx_core::events::Event;
use sbx_core::ops::Poll;

/// A channel within one connection, chosen by the client.
///
/// The client allocates these because it is the one that has to match a frame
/// to a pane. Ids are per connection and may be reused after a `Closed`.
pub type ChannelId = u32;

/// What a channel carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Channel {
    /// The agent's terminal: bytes in, bytes out.
    Terminal { session: String },
    /// The allow/deny feed, as decisions are made.
    Events { session: String },
    /// What the agent is doing, and how far the working copy has moved.
    Status { session: String },
}

impl Channel {
    pub fn session(&self) -> &str {
        match self {
            Channel::Terminal { session }
            | Channel::Events { session }
            | Channel::Status { session } => session,
        }
    }
}

/// Client to server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum ClientFrame {
    Open {
        id: ChannelId,
        channel: Channel,
    },
    Close {
        id: ChannelId,
    },
    /// Keystrokes, base64 of the raw bytes.
    Input {
        id: ChannelId,
        data: String,
    },
    /// The terminal's new size. Sent on open as well as on every change: the
    /// server has no other way to know how wide the client's view is, and a
    /// tmux window keeps whatever size its last client had.
    Resize {
        id: ChannelId,
        cols: u16,
        rows: u16,
    },
}

/// Server to client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "is", rename_all = "kebab-case")]
pub enum ServerFrame {
    /// The channel is live. Nothing before this belongs to it.
    Opened { id: ChannelId },
    /// Terminal output, base64 of the raw bytes.
    Output { id: ChannelId, data: String },
    /// Decisions the gateway has made since the last of these.
    ///
    /// Newest first, matching what `/rpc` answers, and only the new ones: the
    /// first frame after `Opened` carries the recent log, and every frame after
    /// that carries the difference.
    Events { id: ChannelId, events: Vec<Event> },
    /// The agent's state, when it has changed.
    Status { id: ChannelId, poll: Poll },
    /// The channel has ended, for a reason worth showing when there is one.
    Closed {
        id: ChannelId,
        reason: Option<String>,
    },
}

impl ServerFrame {
    pub fn id(&self) -> ChannelId {
        match self {
            ServerFrame::Opened { id }
            | ServerFrame::Output { id, .. }
            | ServerFrame::Events { id, .. }
            | ServerFrame::Status { id, .. }
            | ServerFrame::Closed { id, .. } => *id,
        }
    }
}

/// Raw bytes, as they travel inside a JSON string.
///
/// Standard base64 with padding. Shared by both ends rather than implemented at
/// each, because an encoder and a decoder that disagree about padding produce a
/// terminal that works until someone types a character of the wrong length.
pub mod bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    pub fn encode(raw: &[u8]) -> String {
        STANDARD.encode(raw)
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        STANDARD.decode(text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_says_what_it_is_by_name() {
        let f = ClientFrame::Open {
            id: 3,
            channel: Channel::Terminal {
                session: "readme-fix".into(),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&f).unwrap();
        assert_eq!(v["do"], "open");
        assert_eq!(v["channel"]["kind"], "terminal");
        assert_eq!(v["channel"]["session"], "readme-fix");
        assert_eq!(serde_json::from_value::<ClientFrame>(v).unwrap(), f);
    }

    #[test]
    fn every_server_frame_names_its_channel() {
        let frames = [
            ServerFrame::Opened { id: 1 },
            ServerFrame::Output {
                id: 2,
                data: String::new(),
            },
            ServerFrame::Events {
                id: 3,
                events: Vec::new(),
            },
            ServerFrame::Closed {
                id: 4,
                reason: None,
            },
        ];
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.id() as usize, i + 1, "{f:?}");
        }
    }

    #[test]
    fn a_channel_knows_which_session_it_is_about() {
        for c in [
            Channel::Terminal {
                session: "a".into(),
            },
            Channel::Events {
                session: "a".into(),
            },
            Channel::Status {
                session: "a".into(),
            },
        ] {
            assert_eq!(c.session(), "a");
        }
    }

    /// The reason terminal bytes are encoded at all: a PTY read can end in the
    /// middle of a multi-byte character, and that cannot go in a JSON string.
    #[test]
    fn arbitrary_bytes_survive_the_round_trip() {
        let cases: [&[u8]; 5] = [
            b"",
            b"hello",
            // A UTF-8 sequence cut in half, which is what a short read gives.
            &[0xe2, 0x8f],
            // Escapes, a NUL, and the top of the byte range.
            &[0x1b, b'[', b'3', b'1', b'm', 0x00, 0xff, 0xfe],
            &[0xf0, 0x9f, 0x92, 0xa9],
        ];
        for raw in cases {
            let encoded = bytes::encode(raw);
            assert!(
                encoded.is_ascii(),
                "the encoding has to be safe in a JSON string"
            );
            assert_eq!(bytes::decode(&encoded).as_deref(), Some(raw));
        }
    }

    #[test]
    fn something_that_is_not_base64_decodes_to_nothing_rather_than_panicking() {
        assert!(bytes::decode("not base64!!").is_none());
    }

    /// Input and output use the same encoding in both directions; a mismatch
    /// there is a terminal that echoes wrongly only for some characters.
    #[test]
    fn input_and_output_agree_about_the_encoding() {
        let typed = "ls -la\r".as_bytes();
        let sent = ClientFrame::Input {
            id: 1,
            data: bytes::encode(typed),
        };
        let json = serde_json::to_string(&sent).unwrap();
        let ClientFrame::Input { data, .. } = serde_json::from_str(&json).unwrap() else {
            panic!("not an input frame");
        };
        assert_eq!(bytes::decode(&data).as_deref(), Some(typed));

        let echoed = ServerFrame::Output {
            id: 1,
            data: bytes::encode(typed),
        };
        let json = serde_json::to_string(&echoed).unwrap();
        let ServerFrame::Output { data, .. } = serde_json::from_str(&json).unwrap() else {
            panic!("not an output frame");
        };
        assert_eq!(bytes::decode(&data).as_deref(), Some(typed));
    }
}

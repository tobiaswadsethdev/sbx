//! Talking to an `sbxd` somewhere else.
//!
//! A saved connection is a [`Pairing`] with a name on it, kept in the state
//! directory beside anything else secret this machine holds. `sbx connect`
//! writes one; `--server` picks one; everything else is [`Remote::call`].
//!
//! **A crate rather than a module in the CLI, because the desktop application
//! needs exactly this.** The certificate is pinned by fingerprint, and a webview
//! cannot do that -- `fetch` has no say in which certificate it will accept -- so
//! the connection has to be made on the Rust side of Tauri, by the same code
//! the CLI uses. Two clients that pin differently would be one client that
//! pins.
//!
//! It is also the second implementation of the protocol, which was the point of
//! it existing before there was a UI. A wire format only has one consumer until
//! it has two, and the shortcuts it has taken are invisible until the second one
//! tries to use it.

mod http;
mod pin;
mod ws;

pub use ws::{Incoming, Sink, Stream};

use std::io;
use std::path::PathBuf;

use sbx_core::state;
use sbx_proto::{Failure, Hello, Outcome, Pairing, Reply, Request};
use serde::{Deserialize, Serialize};

/// A server this machine has been paired with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub fingerprint: String,
}

impl Remote {
    pub fn from_pairing(name: &str, p: Pairing) -> Self {
        Self {
            name: name.to_string(),
            host: p.host,
            port: p.port,
            token: p.token,
            fingerprint: p.fingerprint,
        }
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// What the server says it is, without presenting a token.
    ///
    /// The first thing `connect` asks, because it separates the three ways a
    /// pairing can be wrong -- nothing listening, something else listening, and
    /// a certificate that is not the paired one -- from the fourth, a token
    /// that is not accepted. One error each beats one error for all four.
    pub fn hello(&self) -> Result<Hello, Error> {
        let response = http::request(
            &self.host,
            self.port,
            &self.fingerprint,
            "GET",
            "/version",
            None,
            None,
        )?;
        if response.status != 200 {
            return Err(Error::Server(format!(
                "asked for the version and got HTTP {}",
                response.status
            )));
        }
        serde_json::from_str(&response.body).map_err(|_| {
            // The fingerprint matched, so this really is the paired server --
            // which makes an unparseable answer a version problem, not an
            // impostor.
            Error::Server(
                "that is not an sbxd, or it is one this build is too old to understand".into(),
            )
        })
    }

    /// Make one request.
    pub fn call(&self, request: Request) -> Result<Reply, Error> {
        let body = serde_json::to_string(&request).map_err(|e| Error::Server(e.to_string()))?;
        let response = http::request(
            &self.host,
            self.port,
            &self.fingerprint,
            "POST",
            "/rpc",
            Some(&self.token),
            Some(&body),
        )?;

        match response.status {
            200 => {}
            401 => {
                return Err(Error::Server(format!(
                    "`{}` did not accept this token. It may have been revoked; \
                     run `sbxd pair` on the server and `sbx connect` here again",
                    self.name
                )));
            }
            400 => {
                return Err(Error::Server(
                    "the server could not read the request, which means these two \
                     builds disagree about the protocol"
                        .into(),
                ));
            }
            other => return Err(Error::Server(format!("HTTP {other}"))),
        }

        let outcome: Outcome = serde_json::from_str(&response.body)
            .map_err(|e| Error::Server(format!("could not read the reply: {e}")))?;
        outcome.into_result().map_err(Error::Failed)
    }
}

#[derive(Debug)]
pub enum Error {
    /// The connection itself, before any request was answered.
    Transport(http::Error),
    /// The server answered, but not with a reply.
    Server(String),
    /// The server answered with a failure, which is a normal outcome.
    Failed(Failure),
}

impl From<http::Error> for Error {
    fn from(e: http::Error) -> Self {
        Error::Transport(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "{e}"),
            Error::Server(e) => write!(f, "{e}"),
            Error::Failed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The servers this machine knows about.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Remotes {
    #[serde(default)]
    remotes: Vec<Remote>,
}

impl Remotes {
    pub fn default_path() -> PathBuf {
        state::dir().join("remotes.json")
    }

    pub fn load() -> io::Result<Self> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl Into<PathBuf>) -> io::Result<Self> {
        match std::fs::read_to_string(path.into()) {
            Ok(text) => serde_json::from_str(&text).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(Self::default_path())
    }

    pub fn save_to(&self, path: impl Into<PathBuf>) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        state::write_private(&path.into(), &text)
    }

    pub fn list(&self) -> &[Remote] {
        &self.remotes
    }

    pub fn get(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|r| r.name == name)
    }

    /// Add one, replacing any with the same name.
    ///
    /// Replacing rather than refusing: pairing again is what you do when a
    /// server was rebuilt and its certificate changed, and having to remove the
    /// old one first would make the common repair a two-step one.
    pub fn insert(&mut self, remote: Remote) {
        self.remotes.retain(|r| r.name != remote.name);
        self.remotes.push(remote);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.remotes.len();
        self.remotes.retain(|r| r.name != name);
        before != self.remotes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.remotes.is_empty()
    }

    /// Which server a command should use.
    ///
    /// A name if one was given. Otherwise the only one there is, because a
    /// machine with exactly one paired server has no ambiguity worth making
    /// somebody type -- and nothing at all when there are several, because
    /// guessing which of two servers to create a session on is the wrong kind
    /// of convenience.
    pub fn select(&self, name: Option<&str>) -> Result<&Remote, String> {
        match name {
            Some(name) => self.get(name).ok_or_else(|| {
                format!("no server named `{name}`; `sbx remotes` lists the paired ones")
            }),
            None => match self.remotes.as_slice() {
                [] => {
                    Err("no server is paired; run `sbxd pair` there and `sbx connect` here".into())
                }
                [only] => Ok(only),
                many => Err(format!(
                    "several servers are paired ({}); say which with --server",
                    many.iter()
                        .map(|r| r.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            },
        }
    }
}

/// Pair with a server: check the string, check what answers, and save it.
///
/// `sbx connect` is this and a `println!`, and so is the desktop application's
/// connect dialog. Shared rather than written twice because the three checks
/// *are* the pairing: a string that names nothing, something that is not an
/// `sbxd`, and one whose protocol this build cannot speak all produce a saved
/// connection that fails on every request afterwards, and a client that skipped
/// one of them would fail later and somewhere less obvious. The name defaults
/// to the host, which is what `sbx connect` has always done.
pub fn pair(pairing: &str, name: Option<&str>) -> Result<(Remote, Hello), String> {
    // Whatever was pasted may carry the token, so a parse error says what is
    // wrong with the shape of a pairing string and never echoes the string back.
    let pairing: Pairing = pairing
        .trim()
        .parse()
        .map_err(|e: sbx_proto::pairing::ParseError| e.to_string())?;
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&pairing.host)
        .to_string();
    let candidate = Remote::from_pairing(&name, pairing);

    // Tried before it is saved, so a mistyped address or a fingerprint from the
    // wrong server is an error now rather than on every command afterwards.
    let hello = candidate.hello().map_err(|e| e.to_string())?;
    if !hello.is_sbxd() {
        return Err(format!(
            "`{}` is a {}, not an sbxd",
            candidate.address(),
            hello.server
        ));
    }
    if !hello.speaks(sbx_proto::VERSION) {
        return Err(format!(
            "`{}` speaks protocol {} and this sbx speaks {}. Update whichever is older.",
            candidate.address(),
            hello.protocol,
            sbx_proto::VERSION
        ));
    }

    let mut remotes = Remotes::load().map_err(|e| e.to_string())?;
    remotes.insert(candidate.clone());
    remotes.save().map_err(|e| e.to_string())?;

    Ok((candidate, hello))
}

/// One `doctor` line per paired server.
///
/// Asked in two steps, because the answers point in different directions: a
/// `/version` that does not come back is the address or the certificate, and a
/// request that comes back 401 after it is the token. One error apiece is worth
/// two round trips on a command whose whole job is saying what is wrong.
pub fn checks() -> Vec<sbx_core::doctor::Check> {
    let remotes = match Remotes::load() {
        Ok(r) => r,
        Err(e) => {
            return vec![sbx_core::doctor::Check::warn(
                "servers",
                format!("could not read the paired servers: {e}"),
                format!("look at {}", Remotes::default_path().display()),
            )];
        }
    };

    // No line at all when nothing is paired. Running everything locally is the
    // ordinary case and does not need a check saying so.
    if remotes.is_empty() {
        return Vec::new();
    }

    remotes.list().iter().map(check_one).collect()
}

fn check_one(remote: &Remote) -> sbx_core::doctor::Check {
    let hello = match remote.hello() {
        Ok(h) => h,
        Err(e) => {
            return sbx_core::doctor::Check::fail(
                "servers",
                format!("{}: {e}", remote.name),
                format!(
                    "check it is running, and that `{}` is the address this machine \
                     should dial. `sbx remotes --forget {}` drops it",
                    remote.address(),
                    remote.name
                ),
            );
        }
    };

    if !hello.speaks(sbx_proto::VERSION) {
        return sbx_core::doctor::Check::fail(
            "servers",
            format!(
                "{}: speaks protocol {}, this sbx speaks {}",
                remote.name,
                hello.protocol,
                sbx_proto::VERSION
            ),
            "update whichever of the two is older".to_string(),
        );
    }

    match remote.call(Request::Ls) {
        Ok(_) => sbx_core::doctor::Check::ok(
            "servers",
            format!(
                "{}: {} (sbxd {})",
                remote.name,
                remote.address(),
                hello.version
            ),
        ),
        Err(e) => sbx_core::doctor::Check::fail(
            "servers",
            format!("{}: reachable, but {e}", remote.name),
            "`sbxd pair` on the server, then `sbx connect` here with the new string".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "3b1f0a9c4d2e8b7a6f5c4d3e2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0998";

    fn remote(name: &str) -> Remote {
        Remote {
            name: name.into(),
            host: "localhost".into(),
            port: 17671,
            token: "tok".into(),
            fingerprint: FP.into(),
        }
    }

    /// The one half of [`pair`] that needs no server: a string that is not a
    /// pairing string is refused before anything is dialled, and the refusal
    /// does not repeat what was pasted -- which may be a token.
    #[test]
    fn a_string_that_is_not_a_pairing_is_refused_without_dialling_anything() {
        let err = pair("box.lan:17671", None).expect_err("that is not a pairing string");
        assert!(
            err.contains("sbx://"),
            "the error says what one looks like: {err}"
        );
        assert!(
            !err.contains("box.lan"),
            "a pairing string carries a credential and must not be echoed back: {err}"
        );
    }

    #[test]
    fn a_pairing_string_becomes_a_named_remote() {
        let pairing: Pairing = format!("sbx://box.lan:17671/abc#{FP}").parse().unwrap();
        let r = Remote::from_pairing("work", pairing);
        assert_eq!(r.name, "work");
        assert_eq!(r.address(), "box.lan:17671");
        assert_eq!(r.token, "abc");
        assert_eq!(r.fingerprint, FP);
    }

    #[test]
    fn one_paired_server_needs_no_naming_and_several_do() {
        let mut remotes = Remotes::default();
        assert!(remotes.select(None).is_err(), "none paired");

        remotes.insert(remote("wsl"));
        assert_eq!(remotes.select(None).unwrap().name, "wsl");
        assert_eq!(remotes.select(Some("wsl")).unwrap().name, "wsl");

        remotes.insert(remote("cloud"));
        let err = remotes.select(None).unwrap_err();
        assert!(err.contains("--server"), "{err}");
        // Both are named, so the message is also the list.
        assert!(err.contains("wsl") && err.contains("cloud"), "{err}");
        assert_eq!(remotes.select(Some("cloud")).unwrap().name, "cloud");
    }

    #[test]
    fn a_name_that_is_not_paired_says_where_to_look() {
        let remotes = Remotes::default();
        let err = remotes.select(Some("nope")).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("sbx remotes"), "{err}");
    }

    /// Pairing again is the repair for a rebuilt server, so it replaces rather
    /// than adding a second entry under the same name.
    #[test]
    fn pairing_again_replaces_rather_than_duplicating() {
        let mut remotes = Remotes::default();
        remotes.insert(remote("wsl"));
        let mut updated = remote("wsl");
        updated.fingerprint = "ff".repeat(32);
        remotes.insert(updated);

        assert_eq!(remotes.list().len(), 1);
        assert_eq!(remotes.get("wsl").unwrap().fingerprint, "ff".repeat(32));
    }

    #[test]
    fn remotes_survive_a_round_trip_through_the_file() {
        let path = std::env::temp_dir().join(format!("sbx-remotes-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut remotes = Remotes::default();
        remotes.insert(remote("wsl"));
        remotes.save_to(&path).unwrap();

        let back = Remotes::load_from(&path).unwrap();
        assert_eq!(back.list().len(), 1);
        assert_eq!(back.get("wsl").unwrap().token, "tok");

        // A token is a credential, so the file is the owner's alone.
        //
        // Unix only, and not because Windows is exempt: there is no mode there
        // to assert on. `state::write_private` says as much -- a file under a
        // Windows profile inherits an ACL that already denies every other
        // non-administrative account, which is the property 0600 is asked for.
        // The `cfg` is what was missing: `--all-targets` on the Windows job
        // checks tests too, and this one had `use std::os::unix::fs` in it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode was {mode:o}");
        }

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_file_is_no_servers_rather_than_an_error() {
        let path = std::env::temp_dir().join("sbx-remotes-definitely-absent.json");
        let _ = std::fs::remove_file(&path);
        assert!(Remotes::load_from(&path).unwrap().is_empty());
    }

    #[test]
    fn removing_says_whether_there_was_anything_to_remove() {
        let mut remotes = Remotes::default();
        remotes.insert(remote("wsl"));
        assert!(remotes.remove("wsl"));
        assert!(!remotes.remove("wsl"));
        assert!(remotes.is_empty());
    }
}

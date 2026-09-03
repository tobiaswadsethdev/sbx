//! One request, one response, one connection.
//!
//! Enough HTTP/1.1 to talk to an `sbxd` and nothing else. A client crate would
//! be the obvious thing, and the reason there is not one is [`super::pin`]: the
//! fingerprint check needs a `rustls::ClientConfig` of this crate's own, and the
//! blocking clients that are otherwise a good fit take a TLS configuration of
//! their own choosing rather than one handed to them.
//!
//! What makes this small enough to own is `Connection: close` on every request.
//! There is no keep-alive to manage, no chunked body to reassemble and no pool
//! to invalidate: the response is everything up to end of stream. It costs a
//! handshake per request, which against a server on the same machine, or one
//! being asked something once a second, is not the cost worth optimising first.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};

/// How long to wait for a server that has accepted the connection but is not
/// answering.
///
/// A gateway call behind the server can legitimately take seconds, so this is
/// generous.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the connection itself.
///
/// Separate, and much shorter, because it is a different failure: a host that
/// refuses says so at once, and a host that is simply not there drops the
/// packets and would otherwise take the kernel's own two minutes to give up on.
/// A wrong address in a pairing string is common enough -- a WSL box whose
/// address moved, a port forward that is not up -- that waiting that long for
/// it is the difference between an error and a hang.
///
/// Setting the read and write timeouts is not enough on its own: they apply to
/// a socket that is already connected, which this one never becomes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to give one address when there is another to try.
///
/// **A hostname with several addresses must not cost the full timeout for each
/// of them.** `localhost` on Windows resolves to `::1` before `127.0.0.1`, and
/// `sbxd` binds one address -- so every request pays for the wrong one first.
/// When the wrong one refuses, that is microseconds; when something drops the
/// packets instead, which is what a mirrored-networking WSL loopback appears to
/// do, it is the whole ten seconds. Every three seconds. The window stops
/// repainting and Windows writes *(not responding)* in the title bar.
///
/// So the addresses get a fast pass first and the patient one only after all of
/// them have refused quickly. A slow link whose first address is simply far
/// away still connects: it is skipped in the fast pass and picked up in the
/// second.
const FIRST_PASS: Duration = Duration::from_secs(1);

pub struct Response {
    pub status: u16,
    pub body: String,
}

#[derive(Debug)]
pub enum Error {
    Connect(String),
    Tls(String),
    Io(String),
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "could not reach the server: {e}"),
            Error::Tls(e) => write!(f, "{e}"),
            Error::Io(e) => write!(f, "the connection failed: {e}"),
            Error::Malformed(e) => write!(f, "the server's answer made no sense: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Make one request and read the whole answer.
///
/// `body` is `None` for a GET.
pub fn request(
    host: &str,
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Result<Response, Error> {
    let config = super::pin::client_config(fingerprint).map_err(|e| Error::Tls(e.to_string()))?;

    // The name is only what goes in SNI; the certificate is judged by its
    // fingerprint. A dialled IP address has no valid `ServerName`, so it falls
    // back to a placeholder rather than refusing to connect -- see `pin`.
    let server_name = ServerName::try_from(strip_brackets(host).to_string())
        .unwrap_or_else(|_| ServerName::try_from("sbxd").unwrap());

    let tcp = connect(host, port)?;
    tcp.set_read_timeout(Some(TIMEOUT)).ok();
    tcp.set_write_timeout(Some(TIMEOUT)).ok();

    let connection = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| Error::Tls(e.to_string()))?;
    let mut stream = StreamOwned::new(connection, tcp);

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: sbx/{}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    match body {
        Some(body) => {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            ));
            head.push_str(body);
        }
        None => head.push_str("\r\n"),
    }

    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| tls_or_io(&stream, e))?;

    let mut raw = Vec::new();
    // End of stream is the end of the body, which is what `Connection: close`
    // buys. A close during the TLS session rather than at the socket is not an
    // error here for the same reason.
    if let Err(e) = stream.read_to_end(&mut raw)
        && !is_clean_close(&e)
    {
        return Err(tls_or_io(&stream, e));
    }

    parse(&raw)
}

/// Distinguish a TLS failure -- which is nearly always the fingerprint -- from
/// a network one, since the two send you looking in different places.
fn tls_or_io(stream: &StreamOwned<ClientConnection, TcpStream>, e: std::io::Error) -> Error {
    let _ = stream;
    let text = e.to_string();
    if text.contains("certificate") || text.contains("paired with") || text.contains("HandshakeF") {
        // rustls renders a verifier's own error as `unexpected error: ...`,
        // which is noise in front of a message written to be read. Cosmetic
        // only: if the prefix ever changes, it comes back rather than breaking.
        Error::Tls(
            text.strip_prefix("unexpected error: ")
                .unwrap_or(&text)
                .to_string(),
        )
    } else {
        Error::Io(text)
    }
}

/// A server that closes the socket without a TLS `close_notify` is not a
/// problem for a response that is already complete, and is what several
/// perfectly good servers do.
fn is_clean_close(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::UnexpectedEof
        || e.to_string().contains("close_notify")
        || e.to_string().contains("peer closed connection")
}

/// Resolve and dial, with a bound on how long that may take.
///
/// Every address the name resolves to is tried, because a host with both an
/// IPv6 and an IPv4 address where only one is reachable is the ordinary case
/// for `localhost` on a machine with IPv6 half-configured.
/// Dial a host, in the one way this crate dials anything.
///
/// `pub(crate)` because the websocket needs it too: it used to call
/// `TcpStream::connect((host, port))`, which walks the addresses with the
/// *kernel's* retry behind each -- twenty-odd seconds on Windows for a SYN that
/// is dropped rather than refused, with nothing to shorten it. One dialling
/// policy, one address memory, both halves.
pub(crate) fn connect(host: &str, port: u16) -> Result<TcpStream, Error> {
    // The one that answered last time, patiently: it has already proved it is
    // the right address, so a slow link deserves the full timeout rather than
    // being skipped for the one that is wrong.
    if let Some(addr) = recall(host, port) {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(tcp) => return Ok(tcp),
            // It answered before and does not now. Forget it and look properly;
            // the alternative is a client that can never recover from a server
            // moving between stacks.
            Err(_) => forget(host, port),
        }
    }

    let addrs: Vec<SocketAddr> = (strip_brackets(host), port)
        .to_socket_addrs()
        .map_err(|e| Error::Connect(format!("{host}: {e}")))?
        .collect();

    if addrs.is_empty() {
        return Err(Error::Connect(format!("{host} resolves to no address")));
    }

    // Two passes: every address quickly, then every address patiently. One
    // address is one pass -- there is nothing to be quick for.
    let mut last = None;
    for timeout in passes(addrs.len()) {
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, timeout) {
                Ok(tcp) => {
                    remember(host, port, *addr);
                    return Ok(tcp);
                }
                Err(e) => last = Some(e),
            }
        }
    }
    Err(Error::Connect(format!(
        "{host}:{port}: {}",
        last.map(|e| e.to_string())
            .unwrap_or_else(|| "no address answered".into())
    )))
}

/// The address that last answered, per host.
///
/// **A discovery worth making once.** Without this, every request pays the fast
/// pass again: on Windows `localhost` puts `::1` first, nothing there answers,
/// and each file listing and each file read costs a second before it even
/// starts. A file tree is a request per folder, so "a bit slow" is that second,
/// once per click.
///
/// Self-correcting, and it has to be: a remembered address that stops answering
/// -- the server rebound, the machine moved -- is forgotten on the first
/// failure and the full search runs again. What it can cost when wrong is one
/// connect timeout, which is what the first request would have cost anyway.
fn known() -> &'static Mutex<HashMap<(String, u16), SocketAddr>> {
    static KNOWN: OnceLock<Mutex<HashMap<(String, u16), SocketAddr>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

fn recall(host: &str, port: u16) -> Option<SocketAddr> {
    known().lock().ok()?.get(&(host.to_string(), port)).copied()
}

fn remember(host: &str, port: u16, addr: SocketAddr) {
    if let Ok(mut map) = known().lock() {
        map.insert((host.to_string(), port), addr);
    }
}

fn forget(host: &str, port: u16) {
    if let Ok(mut map) = known().lock() {
        map.remove(&(host.to_string(), port));
    }
}

/// The timeout of each pass over the addresses.
///
/// Pure so the schedule can be asserted on: what matters is that a single
/// address is still given the full timeout -- a wrong address in a pairing
/// string is common, and failing it in a second would turn "that host is not
/// there" into "that host is slow".
fn passes(addresses: usize) -> Vec<Duration> {
    if addresses <= 1 {
        vec![CONNECT_TIMEOUT]
    } else {
        vec![FIRST_PASS, CONNECT_TIMEOUT]
    }
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

fn parse(raw: &[u8]) -> Result<Response, Error> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::Malformed("no header break".into()))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = String::from_utf8_lossy(&raw[split + 4..]).into_owned();

    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| Error::Malformed("no status line".into()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| Error::Malformed(format!("no status code in `{status_line}`")))?;

    Ok(Response { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address that answered is remembered, so the fast pass is a cost paid
    /// once rather than per request -- which for a file tree is per folder you
    /// click.
    #[test]
    fn the_address_that_answered_is_remembered_and_forgotten_when_it_stops() {
        let host = "test.invalid";
        let addr: SocketAddr = "127.0.0.1:17671".parse().unwrap();
        assert_eq!(recall(host, 1), None, "nothing is known to begin with");

        remember(host, 1, addr);
        assert_eq!(recall(host, 1), Some(addr));
        // Keyed by port as well: one host can serve two of these.
        assert_eq!(recall(host, 2), None);

        // Forgotten on failure, or a server that moves between stacks could
        // never be reached again.
        forget(host, 1);
        assert_eq!(recall(host, 1), None);
    }

    /// The schedule that keeps a window responsive on Windows without turning
    /// "that host is not there" into "that host is slow".
    #[test]
    fn one_address_is_patient_and_several_are_tried_quickly_first() {
        assert_eq!(passes(1), [CONNECT_TIMEOUT], "nothing else to try");
        assert_eq!(passes(0), [CONNECT_TIMEOUT]);
        // `localhost` on Windows: `::1` then `127.0.0.1`, and only one of them
        // is listening.
        assert_eq!(passes(2), [FIRST_PASS, CONNECT_TIMEOUT]);
        assert!(FIRST_PASS < CONNECT_TIMEOUT);
        // The worst case is still bounded by what a single address costs, plus
        // the fast pass -- not by the number of addresses times ten seconds.
        assert!(passes(4).iter().sum::<Duration>() < CONNECT_TIMEOUT * 2);
    }

    #[test]
    fn a_response_splits_into_a_status_and_a_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":1}";
        let r = parse(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"ok\":1}");
    }

    #[test]
    fn a_body_containing_the_header_break_survives_intact() {
        // The split is on the *first* break, and a JSON body can contain the
        // same four bytes inside a string.
        let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"body\":\"a\\r\\n\\r\\nb\"}";
        let r = parse(raw).unwrap();
        assert_eq!(r.body, "{\"body\":\"a\\r\\n\\r\\nb\"}");
    }

    #[test]
    fn an_empty_body_is_a_body() {
        let r = parse(b"HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap();
        assert_eq!(r.status, 401);
        assert_eq!(r.body, "");
    }

    #[test]
    fn something_that_is_not_a_response_is_refused_rather_than_guessed_at() {
        assert!(matches!(parse(b"hello"), Err(Error::Malformed(_))));
        assert!(matches!(
            parse(b"not a status line\r\n\r\nbody"),
            Err(Error::Malformed(_))
        ));
    }

    /// A bracketed IPv6 literal is right in a URL and wrong in a `connect`.
    #[test]
    fn brackets_come_off_the_host_before_it_is_dialled() {
        assert_eq!(strip_brackets("[::1]"), "::1");
        assert_eq!(strip_brackets("localhost"), "localhost");
        assert_eq!(strip_brackets("10.0.0.1"), "10.0.0.1");
    }

    /// A name that resolves to nothing must be an error, not a hang, and must
    /// say which name.
    #[test]
    fn a_host_that_does_not_resolve_says_so() {
        let err = connect("no-such-host.invalid", 17671).unwrap_err();
        assert!(
            matches!(err, Error::Connect(_)),
            "{err} should be a connection failure"
        );
        assert!(err.to_string().contains("no-such-host.invalid"), "{err}");
    }

    /// The bound on connecting is separate from the bound on reading, because
    /// they are different failures -- and the shorter one is the one that keeps
    /// a wrong address in a pairing string from looking like a hang.
    #[test]
    fn connecting_is_bounded_more_tightly_than_reading() {
        assert!(CONNECT_TIMEOUT < TIMEOUT);
        // Well under the kernel's own giving-up time, which is the point.
        assert!(CONNECT_TIMEOUT <= Duration::from_secs(15));
    }

    /// A server closing without `close_notify` after a complete response is
    /// normal, and treating it as a failure would break every other request.
    #[test]
    fn an_abrupt_close_after_a_complete_response_is_not_a_failure() {
        assert!(is_clean_close(&std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "eof"
        )));
        assert!(!is_clean_close(&std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused"
        )));
    }
}

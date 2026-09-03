//! The write-back half of the task inbox, against a tracker on loopback.
//!
//! The parsers are unit-tested against captured answers; what this covers is
//! everything between them and the wire -- the curl configuration, the
//! credential going in on stdin, the Atlassian Document Format a Jira comment
//! has to be, and a transition looked up by name rather than by id.
//!
//! It stands up thirty lines of HTTP on `127.0.0.1` rather than mocking the
//! module, because the questions worth asking are "does curl send what we
//! think" and "is the body the shape Jira wants", and neither survives being
//! answered by a fake in the same process.
//!
//! Its own file so it gets its own process: it sets `XDG_STATE_HOME`, which is
//! how the secret store is found, and an environment variable is not something
//! to change under the rest of the suite.
//!
//! **And one directory for the whole file, set once.** A file's tests share a
//! process and run on threads, so two of them each pointing `XDG_STATE_HOME` at
//! a directory of their own is a race: whichever sets it last decides where
//! *both* look, and the other reads a directory with no secret in it. It passed
//! here and failed on the first CI run, which is the only kind of luck this
//! sort of test has.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use sbx_core::secrets;
use sbx_core::tracker::{Kind, Source, Ticket, on_publish};

/// The one state directory this file uses, with the credential already in it.
///
/// Set once however many tests ask for it: `set_var` is process-global, and the
/// point is that every test in this process agrees about where the secret store
/// is. Storing the token here too means neither test writes it, so neither can
/// race the other into writing it somewhere the other is not looking.
fn state() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("sbx-tracker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Safety: once, and before anything in this process reads the
        // environment -- which is what `OnceLock` is here to guarantee.
        unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };
        secrets::set("JIRA_TOKEN", "a-token").expect("stored");
        dir
    })
}

/// One request the stand-in received.
#[derive(Debug)]
struct Seen {
    method: String,
    path: String,
    auth: String,
    body: String,
}

/// A tracker on loopback: answers the two GETs Jira's transition lookup makes
/// and records every POST.
fn stand_in(seen: Sender<Seen>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Three requests and then done: the comment, the transition list, and
        // the transition itself.
        for _ in 0..3 {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            serve(stream, &seen);
        }
    });
    port
}

fn serve(mut stream: TcpStream, seen: &Sender<Seen>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut length = 0usize;
    let mut auth = String::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(v) = header.strip_prefix("Content-Length: ") {
            length = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = header.strip_prefix("Authorization: ") {
            auth = v.trim().to_string();
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    // The transition list is the only thing that has to answer with anything.
    let payload = if path.contains("/transitions") && method == "GET" {
        r#"{"transitions":[
             {"id":"21","name":"Ready for Review","to":{"name":"Ready for Review"}},
             {"id":"31","name":"Done","to":{"name":"Done"}}
           ]}"#
    } else {
        r#"{"id":"10001"}"#
    };
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(answer.as_bytes());
    let _ = stream.flush();
    let _ = seen.send(Seen {
        method,
        path,
        auth,
        body,
    });
}

#[test]
fn publishing_comments_on_the_ticket_and_moves_it() {
    state();

    let (tx, rx) = channel();
    let port = stand_in(tx);
    let source = Source {
        kind: Kind::Jira,
        name: "inet-jira".into(),
        secret: "JIRA_TOKEN".into(),
        repo: None,
        org: None,
        project: None,
        site: Some(format!("http://127.0.0.1:{port}")),
        email: Some("you@example.com".into()),
        query: None,
        on_publish: Some("Ready for Review".into()),
    };
    let ticket = Ticket {
        tracker: "inet-jira".into(),
        kind: Kind::Jira,
        id: "INET-4821".into(),
        key: "INET-4821".into(),
        url: format!("http://127.0.0.1:{port}/browse/INET-4821"),
        repo: None,
    };

    let warnings = on_publish(
        std::slice::from_ref(&source),
        &ticket,
        "https://github.com/o/r/pull/7",
    );
    assert!(warnings.is_empty(), "{warnings:?}");

    let comment = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(comment.method, "POST");
    assert_eq!(comment.path, "/rest/api/3/issue/INET-4821/comment");
    // Basic, with the email as the username: a bearer token authenticates as
    // nobody on Jira Cloud. And it arrived, which is the whole point of putting
    // it on curl's stdin rather than in its arguments.
    assert_eq!(
        comment.auth,
        format!(
            "Basic {}",
            sbx_core::skills::base64(b"you@example.com:a-token")
        )
    );
    // Atlassian Document Format: a plain string body is a 400.
    let sent: serde_json::Value = serde_json::from_str(&comment.body).expect("json");
    assert_eq!(sent["body"]["type"], "doc");
    assert_eq!(sent["body"]["version"], 1);
    assert_eq!(
        sent["body"]["content"][0]["content"][0]["text"],
        "Pull request: https://github.com/o/r/pull/7"
    );

    // The transition is looked up by name, because Jira moves an issue by id
    // and which ids exist depends on the workflow and where the issue is.
    let listed = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(listed.method, "GET");
    assert_eq!(listed.path, "/rest/api/3/issue/INET-4821/transitions");

    let moved = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(moved.method, "POST");
    let sent: serde_json::Value = serde_json::from_str(&moved.body).expect("json");
    assert_eq!(sent["transition"]["id"], "21", "matched by name");
}

/// A status nothing can transition to is a configuration mistake, and the
/// message has to name what the issue *can* do -- Jira's own answer to a bad
/// transition names neither.
#[test]
fn a_transition_that_does_not_exist_says_what_does() {
    state();

    let (tx, _rx) = channel();
    let port = stand_in(tx);
    let source = Source {
        kind: Kind::Jira,
        name: "inet-jira".into(),
        secret: "JIRA_TOKEN".into(),
        repo: None,
        org: None,
        project: None,
        site: Some(format!("http://127.0.0.1:{port}")),
        email: Some("you@example.com".into()),
        query: None,
        on_publish: Some("In Review".into()),
    };
    let ticket = Ticket {
        tracker: "inet-jira".into(),
        kind: Kind::Jira,
        id: "INET-4821".into(),
        key: "INET-4821".into(),
        url: "http://example.invalid".into(),
        repo: None,
    };

    let warnings = on_publish(std::slice::from_ref(&source), &ticket, "https://pr");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let said = &warnings[0];
    assert!(said.contains("In Review"), "{said}");
    assert!(said.contains("Ready for Review"), "{said}");
    assert!(said.contains("Done"), "{said}");
}

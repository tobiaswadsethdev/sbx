//! The listener: TLS, one token check, and `/rpc`.
//!
//! Two routes for now. `GET /version` answers [`sbx_proto::Hello`] to anyone,
//! because a client that cannot tell whether it has reached an `sbxd` at all
//! has nothing useful to say; `POST /rpc` needs a bearer token and is
//! everything else.
//!
//! **The core is blocking and this is not.** Every dispatch runs a subprocess
//! against the gateway for a few hundred milliseconds, so each one goes to
//! [`tokio::task::spawn_blocking`] rather than onto a runtime thread. Without
//! that, two clients and one slow `openshell` call are enough to stall the
//! whole server, including the `/version` that would have explained why.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use sbx_proto::{Failure, Hello, Outcome, Request};

use crate::auth::Tokens;
use crate::rpc;

pub struct Server {
    /// Behind a lock because it is re-read when the file changes, and the
    /// change can be `sbxd revoke` in another terminal while a request is in
    /// flight.
    pub tokens: RwLock<Tokens>,
}

type Shared = Arc<Server>;

pub fn app(server: Shared) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/rpc", post(rpc_route))
        .route("/ws", get(ws_route))
        .with_state(server)
}

/// The streaming half: status, the events feed, and the terminal.
///
/// Authenticated by the same bearer header as `/rpc`, and deliberately not by a
/// token in the query string -- which is the usual shortcut for websockets,
/// because a browser cannot set a header on one. There is no browser here: the
/// client is `sbx-client`, on the Rust side of the desktop application, and it
/// can. A token in a URL ends up in logs.
async fn ws_route(
    State(server): State<Shared>,
    headers: HeaderMap,
    upgrade: axum::extract::WebSocketUpgrade,
) -> Result<axum::response::Response, StatusCode> {
    if !authorised(&server, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(upgrade.on_upgrade(crate::stream::run))
}

async fn version() -> Json<Hello> {
    Json(Hello::current())
}

async fn rpc_route(
    State(server): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<Outcome>, StatusCode> {
    if !authorised(&server, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // A body that is not a request at all is a transport-level problem -- a
    // client speaking the wrong protocol, or nothing -- so it is a status
    // rather than an `Outcome`. An `op` this build does not have is *not*:
    // that is a newer client talking to an older server, which the envelope
    // explains far better than a 400 does.
    let request: Request = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) if is_unknown_op(&body) => {
            return Ok(Json(Failure::unsupported(&op_of(&body)).into()));
        }
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let outcome = tokio::task::spawn_blocking(move || rpc::dispatch(&rpc::backends(), request))
        .await
        .unwrap_or_else(|e| Failure::failed(format!("the server dropped the request: {e}")).into());

    Ok(Json(outcome))
}

/// Whether the request carries a token this server minted.
///
/// `Authorization: Bearer <token>` and nothing else. No cookie, no query
/// parameter: a token in a URL ends up in logs and in a browser's history, and
/// there is no browser here that would need it to.
fn authorised(server: &Server, headers: &HeaderMap) -> bool {
    let Some(token) = bearer_token(headers) else {
        return false;
    };

    // Checked per request, so `sbxd pair` reaches a running server and -- the
    // direction that matters -- `sbxd revoke` does too. It is a `stat`, and
    // this is not a server anyone is making thousands of requests a second to.
    let changed = server
        .tokens
        .read()
        .is_ok_and(|tokens| tokens.changed_on_disk());
    if let (true, Ok(mut tokens)) = (changed, server.tokens.write()) {
        let _ = tokens.reload();
    }

    server
        .tokens
        .read()
        .is_ok_and(|tokens| tokens.verify(&token).is_some())
}

/// The token out of an `Authorization: Bearer` header, if there is one.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Whether a body that failed to parse looks like a request with an `op` this
/// build has never heard of, rather than something that is not a request.
fn is_unknown_op(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_string))
        .is_some()
}

fn op_of(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("op")
                .and_then(|o| o.as_str())
                .map(|s| s.chars().take(40).collect())
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Where the server is listening, and how loudly to say so.
pub fn describe(addr: SocketAddr) -> String {
    if addr.ip().is_loopback() {
        format!("listening on https://{addr} (this machine only)")
    } else {
        format!(
            "listening on https://{addr} -- reachable from the network. \
             An authenticated client can create containers on this host, \
             so treat a token as a login to it"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// A store with one token in it, the token itself, and the file behind it.
    ///
    /// Minted through the real loader rather than assembled by hand, so the
    /// hashing these tests exercise is the hashing a request goes through --
    /// and the file is left in place, because `authorised` re-reads it.
    fn paired(name: &str) -> (Tokens, String, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("sbxd-serve-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut tokens = Tokens::load_from(&path).unwrap();
        let token = tokens.create(name).unwrap();
        (tokens, token, path)
    }

    fn bearer(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(v).unwrap(),
        );
        h
    }

    #[test]
    fn a_request_with_no_token_is_not_authorised() {
        let (tokens, _, path) = paired("nobody");
        let server = Server {
            tokens: RwLock::new(tokens),
        };
        assert!(!authorised(&server, &HeaderMap::new()));
        assert!(!authorised(&server, &bearer("Bearer wrong")));
        // A token in the right place but the wrong scheme is still no.
        assert!(!authorised(&server, &bearer("Basic wrong")));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_minted_token_is_authorised_and_survives_surrounding_space() {
        let (tokens, token, path) = paired("laptop");
        let server = Server {
            tokens: RwLock::new(tokens),
        };

        assert!(authorised(&server, &bearer(&format!("Bearer {token}"))));
        assert!(authorised(&server, &bearer(&format!("Bearer {token} "))));
        assert!(!authorised(&server, &bearer(&format!("Bearer {token}x"))));

        let _ = std::fs::remove_file(&path);
    }

    /// The whole reason the set is behind a lock: `sbxd pair` and `sbxd revoke`
    /// run in another process, and a running server has to see both.
    #[test]
    fn pairing_and_revoking_reach_a_running_server() {
        let (tokens, first, path) = paired("running");
        let server = Server {
            tokens: RwLock::new(tokens),
        };
        assert!(authorised(&server, &bearer(&format!("Bearer {first}"))));

        // Another process pairs a second client.
        let second = Tokens::load_from(&path).unwrap().create("phone").unwrap();
        assert!(
            authorised(&server, &bearer(&format!("Bearer {second}"))),
            "a token minted while the server ran was not accepted"
        );

        // And revokes the first.
        Tokens::load_from(&path).unwrap().revoke("running").unwrap();
        assert!(
            !authorised(&server, &bearer(&format!("Bearer {first}"))),
            "a revoked token was still accepted"
        );
        assert!(authorised(&server, &bearer(&format!("Bearer {second}"))));

        let _ = std::fs::remove_file(&path);
    }

    /// Deleting the file is a legitimate way to revoke everything at once, and
    /// reads the same as a server nobody has paired with yet.
    #[test]
    fn deleting_the_token_file_revokes_every_token() {
        let (tokens, token, path) = paired("deleted");
        let server = Server {
            tokens: RwLock::new(tokens),
        };
        assert!(authorised(&server, &bearer(&format!("Bearer {token}"))));

        std::fs::remove_file(&path).unwrap();
        assert!(!authorised(&server, &bearer(&format!("Bearer {token}"))));
    }

    /// A server nobody has paired with accepts nothing, rather than everything.
    /// The inverted version of this check is a plausible bug and a total one.
    #[test]
    fn a_server_with_no_tokens_accepts_nothing() {
        let path =
            std::env::temp_dir().join(format!("sbxd-serve-empty-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let tokens = Tokens::load_from(&path).unwrap();
        assert!(tokens.is_empty());
        let server = Server {
            tokens: RwLock::new(tokens),
        };
        assert!(!authorised(&server, &bearer("Bearer ")));
        assert!(!authorised(&server, &bearer("Bearer anything")));
    }

    /// A newer client's request must come back as `unsupported` rather than a
    /// 400, because the client can explain the first and not the second.
    #[test]
    fn a_request_shaped_like_one_with_an_unknown_op_is_recognised_as_such() {
        assert!(is_unknown_op(r#"{"op":"attach","name":"a"}"#));
        assert_eq!(op_of(r#"{"op":"attach","name":"a"}"#), "attach");

        // Not a request at all: no `op` to report.
        assert!(!is_unknown_op("garbage"));
        assert!(!is_unknown_op(r#"{"hello":true}"#));
    }

    /// An op long enough to fill a log line is a client being hostile or
    /// broken; the reply quotes it, so it is cut.
    #[test]
    fn an_absurd_op_is_truncated_before_it_is_quoted_back() {
        let body = format!(r#"{{"op":"{}"}}"#, "a".repeat(500));
        assert_eq!(op_of(&body).len(), 40);
    }

    #[test]
    fn binding_off_loopback_says_what_that_means() {
        let local = describe("127.0.0.1:17671".parse().unwrap());
        assert!(local.contains("this machine only"), "{local}");

        let wide = describe("0.0.0.0:17671".parse().unwrap());
        assert!(wide.contains("reachable from the network"), "{wide}");
        assert!(wide.contains("create containers"), "{wide}");
    }
}

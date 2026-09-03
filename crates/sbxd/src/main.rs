//! `sbxd` - the sbx server.
//!
//! Everything a client can ask for, over one authenticated TLS port. The
//! sessions it serves are the same ones `sbx` on this machine works with: one
//! cache, one gateway, one set of sandboxes, whether the thing driving them is
//! a terminal here or an application somewhere else.

mod auth;
mod rpc;
mod serve;
mod stream;
mod tls;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use sbx_proto::Pairing;

use auth::Tokens;
use sbx_core::state;

#[derive(Parser)]
#[command(
    name = "sbxd",
    version,
    about = "The sbx server: sessions and their sandboxes, over one authenticated port"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Listen. The default when no command is given.
    Serve(Serve),
    /// Print a pairing string for a new client, once.
    Pair {
        /// What the client is, so this token can be revoked without the others.
        #[arg(default_value = "client")]
        name: String,
        /// The host a client should dial, when it is not this machine's own
        /// name -- a WSL server reached from Windows, or a box behind a
        /// forwarded port.
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// The tokens this server accepts.
    Tokens,
    /// Stop accepting a token.
    Revoke { name: String },
}

#[derive(clap::Args)]
struct Serve {
    /// Address to listen on. Loopback by default: anything else is reachable
    /// from the network, and an authenticated client can create containers here.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// An extra name or address to put in the certificate. Repeatable.
    #[arg(long = "san")]
    sans: Vec<String>,
}

use sbx_proto::DEFAULT_PORT;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => serve(Serve {
            bind: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            sans: Vec::new(),
        }),
        Some(Command::Serve(s)) => serve(s),
        Some(Command::Pair { name, host, port }) => pair(&name, host, port),
        Some(Command::Tokens) => list_tokens(),
        Some(Command::Revoke { name }) => revoke(&name),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sbxd: {e}");
            ExitCode::FAILURE
        }
    }
}

type Fallible = Result<(), Box<dyn std::error::Error>>;

fn serve(opts: Serve) -> Fallible {
    let dir = state::dir();
    state::private_dir(&dir)?;

    let identity = tls::ensure(&dir, &tls::default_sans(&opts.sans))?;
    let tokens = Tokens::load()?;

    if tokens.is_empty() {
        // Starting with nothing that can connect is legal and almost never
        // meant. Said at start rather than left for the first client to
        // discover as an unexplained 401.
        eprintln!("sbxd: no tokens yet, so nothing can connect. Run `sbxd pair` to make one.");
    }

    let addr: SocketAddr = format!("{}:{}", opts.bind, opts.port).parse()?;
    println!("{}", serve::describe(addr));
    println!("certificate {}", identity.fingerprint);

    // Once, at startup rather than per request: repairing a record left mid
    // lifecycle costs an exec per session, and what it fixes is a create that
    // died, which cannot happen again while the server is down.
    match sbx_core::ops::refresh_with(&rpc::backends(), true) {
        Ok(r) => {
            for name in &r.adopted {
                println!("adopted `{name}`");
            }
            for warning in &r.warnings {
                eprintln!("sbxd: {warning}");
            }
        }
        // Not fatal. The gateway can come back, and a server that refuses to
        // start without it is one you cannot reach to find out why.
        Err(e) => eprintln!("sbxd: the gateway did not answer at startup: {e}"),
    }

    let server = Arc::new(serve::Server {
        tokens: std::sync::RwLock::new(tokens),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let config = axum_server::tls_rustls::RustlsConfig::from_pem(
            identity.cert_pem.into_bytes(),
            identity.key_pem.into_bytes(),
        )
        .await?;

        axum_server::bind_rustls(addr, config)
            .serve(serve::app(server).into_make_service())
            .await
    })?;

    Ok(())
}

fn pair(name: &str, host: Option<String>, port: u16) -> Fallible {
    let dir = state::dir();
    state::private_dir(&dir)?;

    // Generated now if the server has not run yet, so a pairing string can be
    // made before the first start -- and so the fingerprint in it is the one
    // that server will actually present.
    let identity = tls::ensure(&dir, &tls::default_sans(&[]))?;

    let mut tokens = Tokens::load()?;
    let token = tokens.create(name)?;

    let host = host.unwrap_or_else(default_host);
    let pairing = Pairing {
        host,
        port,
        token,
        fingerprint: identity.fingerprint,
    };

    println!("{pairing}");
    println!();
    println!("Paste that into the client once. It is shown here and nowhere else --");
    println!("the server keeps only a hash, so a lost token is replaced, not recovered.");
    if let Some(hint) = wsl_hint() {
        println!();
        println!("{hint}");
    }
    Ok(())
}

fn default_host() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// Said at pairing time, because this is the one place the address in the
/// string is chosen, and the WSL answer is not the machine's own hostname.
fn wsl_hint() -> Option<String> {
    if !is_wsl() {
        return None;
    }
    Some(
        "This is WSL. If the client is on Windows, the host above is probably not\n\
         what it should dial: with mirrored networking use `localhost`, and with the\n\
         default NAT use the address `hostname -I` prints. `sbx doctor` says which."
            .to_string(),
    )
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

fn list_tokens() -> Fallible {
    let tokens = Tokens::load()?;
    if tokens.is_empty() {
        println!("no tokens. `sbxd pair` makes one.");
        return Ok(());
    }
    println!("{:<24} {:<12} CREATED", "NAME", "HASH");
    for entry in tokens.list() {
        println!(
            "{:<24} {:<12} {}",
            entry.name,
            &entry.hash[..12],
            entry.created_at
        );
    }
    Ok(())
}

fn revoke(name: &str) -> Fallible {
    let mut tokens = Tokens::load()?;
    match tokens.revoke(name)? {
        0 => Err(format!("no token named `{name}`; see `sbxd tokens`").into()),
        1 => {
            println!("revoked `{name}`");
            Ok(())
        }
        n => {
            println!("revoked {n} tokens named `{name}`");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_host_is_a_name_and_never_empty() {
        let host = default_host();
        assert!(!host.is_empty());
        assert!(!host.contains(char::is_whitespace), "{host}");
    }

    /// The hint is only useful where it applies, and misleading everywhere
    /// else -- it tells you to dial something other than what was printed.
    #[test]
    fn the_wsl_hint_appears_only_under_wsl() {
        assert_eq!(wsl_hint().is_some(), is_wsl());
    }
}

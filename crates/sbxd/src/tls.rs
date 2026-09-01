//! The server's certificate, and the fingerprint a client pins it by.
//!
//! Self-signed, generated once on first run and kept in the state directory.
//! There is no certificate authority in this picture and there should not be: a
//! server on a LAN or inside WSL has no name a public CA will vouch for, and
//! `mkcert`-style local authorities solve the problem by installing a trust
//! anchor that then vouches for *everything* signed by it.
//!
//! What replaces the authority is the fingerprint in the pairing string. The
//! client is told which certificate to expect before it connects, so the first
//! connection is verified like every later one -- which is the hole in ordinary
//! trust-on-first-use, where the first connection is the one that cannot be.

use std::io;
use std::net::IpAddr;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::auth::hex;
use sbx_core::state;

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    /// Lowercase hex SHA-256 of the DER certificate. What goes in the pairing
    /// string, and what a client compares the presented certificate against.
    pub fingerprint: String,
}

/// Load the certificate, generating one if there is none.
///
/// `sans` are the names and addresses a client might dial this server by. They
/// matter more here than they would behind a CA: a certificate without the
/// address in it fails verification at the client for a reason that reads as a
/// network fault, and the WSL case dials an address the server would never have
/// guessed for itself.
pub fn ensure(dir: &Path, sans: &[String]) -> io::Result<Identity> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");

    if let (Ok(cert_pem), Ok(key_pem)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        let fingerprint = fingerprint_of(&cert_pem)?;
        return Ok(Identity {
            cert_pem,
            key_pem,
            fingerprint,
        });
    }

    let generated = rcgen::generate_simple_self_signed(sans.to_vec()).map_err(io::Error::other)?;
    let cert_pem = generated.cert.pem();
    let key_pem = generated.signing_key.serialize_pem();
    let fingerprint = hex(&Sha256::digest(generated.cert.der()));

    state::write_private(&key_path, &key_pem)?;
    // The certificate is public by nature, but it lives in a 0700 directory
    // and there is nothing to gain from a wider mode on the file.
    state::write_private(&cert_path, &cert_pem)?;

    Ok(Identity {
        cert_pem,
        key_pem,
        fingerprint,
    })
}

/// The fingerprint of a PEM certificate, over its DER body.
///
/// Over the DER and not the PEM text, because the PEM is line-wrapped
/// base64 with a header, and a re-wrapped copy of the same certificate would
/// otherwise fingerprint differently.
pub fn fingerprint_of(pem: &str) -> io::Result<String> {
    let der = der_of(pem).ok_or_else(|| io::Error::other("not a PEM certificate"))?;
    Ok(hex(&Sha256::digest(&der)))
}

fn der_of(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE-----"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END CERTIFICATE-----"))
        .collect();
    base64_decode(body.trim())
}

/// Just enough base64 to read a PEM body.
///
/// A dependency for this would be reasonable and there is nearly one in the
/// tree already; forty lines that only ever sees a certificate this process
/// wrote seemed the smaller thing to own.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Every name and address this server can reasonably be reached by.
///
/// `localhost` and the loopback addresses are always in, because the local case
/// is the common one. The hostname is in because a LAN client dials it. The
/// interface addresses are in because WSL and cloud hosts are reached by
/// address far more often than by a name that resolves.
pub fn default_sans(extra: &[String]) -> Vec<String> {
    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Ok(hostname) = std::fs::read_to_string("/etc/hostname") {
        let hostname = hostname.trim();
        if !hostname.is_empty() {
            sans.push(hostname.to_string());
            sans.push(format!("{hostname}.local"));
        }
    }
    for addr in local_addresses() {
        sans.push(addr.to_string());
    }
    sans.extend(extra.iter().cloned());
    sans.sort();
    sans.dedup();
    sans
}

/// The addresses of this machine's interfaces.
///
/// Read from `/proc/net/fib_trie`, which needs no dependency and no syscall
/// wrapper. Best effort by design: a missing address costs a name in the
/// certificate, which `--san` can put back, and is not worth failing a start
/// over.
fn local_addresses() -> Vec<IpAddr> {
    let Ok(text) = std::fs::read_to_string("/proc/net/fib_trie") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(addr) = line.trim().strip_prefix("|-- ") else {
            continue;
        };
        // Only the host's own addresses, which are the ones tagged LOCAL on the
        // line after -- the rest of the trie is networks and broadcasts.
        if !lines.peek().is_some_and(|n| n.contains("LOCAL")) {
            continue;
        }
        if let Ok(ip) = addr.trim().parse::<IpAddr>()
            && !ip.is_loopback()
        {
            out.push(ip);
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("sbxd-tls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        state::private_dir(&p).unwrap();
        p
    }

    #[test]
    fn a_certificate_is_generated_once_and_then_reused() {
        let dir = scratch("reuse");
        let first = ensure(&dir, &["localhost".into()]).unwrap();
        let second = ensure(&dir, &["localhost".into()]).unwrap();

        assert_eq!(
            first.fingerprint, second.fingerprint,
            "a restart must not invalidate every paired client"
        );
        assert_eq!(first.cert_pem, second.cert_pem);
        assert!(first.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_eq!(first.fingerprint.len(), 64);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The fingerprint read back from the file has to equal the one handed out
    /// at generation, or a paired client stops connecting after a restart.
    #[test]
    fn the_fingerprint_is_the_same_computed_either_way() {
        let dir = scratch("fingerprint");
        let id = ensure(&dir, &["localhost".into()]).unwrap();
        assert_eq!(fingerprint_of(&id.cert_pem).unwrap(), id.fingerprint);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_servers_do_not_share_a_certificate() {
        let a = scratch("distinct-a");
        let b = scratch("distinct-b");
        let one = ensure(&a, &["localhost".into()]).unwrap();
        let two = ensure(&b, &["localhost".into()]).unwrap();
        assert_ne!(one.fingerprint, two.fingerprint);

        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn the_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        ensure(&dir, &["localhost".into()]).unwrap();
        let mode = std::fs::metadata(dir.join("key.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "key mode was {mode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_local_names_are_always_offered_and_extras_are_added_once() {
        let sans = default_sans(&["wsl.local".into(), "localhost".into()]);
        assert!(sans.contains(&"localhost".to_string()));
        assert!(sans.contains(&"127.0.0.1".to_string()));
        assert!(sans.contains(&"wsl.local".to_string()));
        assert_eq!(
            sans.iter().filter(|s| *s == "localhost").count(),
            1,
            "a name given twice must not appear twice: {sans:?}"
        );
    }

    #[test]
    fn base64_reads_what_pem_writes() {
        // "hello world" through the alphabet, padding included.
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("not base64!").is_none());
    }
}

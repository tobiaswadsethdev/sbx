//! Who may talk to this server.
//!
//! One bearer token per client, named so it can be revoked without revoking the
//! others, and stored as a SHA-256 hash so the file is not itself a set of
//! credentials.
//!
//! **SHA-256 and not argon2, deliberately.** A password stretcher exists because
//! people choose passwords out of a space small enough to search; these tokens
//! are 32 bytes from the OS, so there is nothing to search and the cost would
//! buy nothing but a slower request. What does matter is comparing in constant
//! time, which is why [`subtle`] is here: a comparison that returns early on the
//! first wrong byte leaks the prefix, and a token is guessable one byte at a
//! time if you can measure that.

use std::io;
use std::path::PathBuf;

use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::state;

/// A token, as it is kept: named, hashed, dated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// What it is for, so a person revoking one knows which. Not unique --
    /// two laptops called `laptop` is a mistake to live with, not to reject at
    /// two in the morning.
    pub name: String,
    /// Lowercase hex SHA-256 of the token.
    pub hash: String,
    /// Epoch seconds, matching the session record's convention.
    pub created_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    tokens: Vec<Entry>,
}

/// The tokens this server accepts.
///
/// Reloaded when the file changes, which is what makes `sbxd pair` work against
/// a server that is already running -- and, the direction that actually
/// matters, makes `sbxd revoke` take effect without a restart. A set read once
/// at startup would have meant a revoked token stayed good until someone
/// remembered to restart, which is the opposite of what revoking is for.
#[derive(Debug)]
pub struct Tokens {
    path: PathBuf,
    entries: Vec<Entry>,
    stamp: Option<Stamp>,
}

/// Enough of a file's identity to notice it has been rewritten.
///
/// Modified time *and* length: a filesystem whose timestamps have one-second
/// granularity can have two writes land in the same tick, and a token minted
/// and revoked inside one second is exactly the case a test produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: std::time::SystemTime,
    len: u64,
}

fn stamp_of(path: &std::path::Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok()?,
        len: meta.len(),
    })
}

impl Tokens {
    pub fn default_path() -> PathBuf {
        state::dir().join("tokens.json")
    }

    pub fn load() -> io::Result<Self> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<File>(&text)
                    .map_err(io::Error::other)?
                    .tokens
            }
            // No file is a server nobody has paired with yet, which is the
            // normal first run rather than a fault.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let stamp = stamp_of(&path);
        Ok(Tokens {
            path,
            entries,
            stamp,
        })
    }

    /// Whether the file has been written since it was last read.
    pub fn changed_on_disk(&self) -> bool {
        stamp_of(&self.path) != self.stamp
    }

    /// Re-read the file. A read failure leaves the set as it was: a server that
    /// forgot every token because the disk hiccupped would lock out every
    /// client at once, which is worse than briefly honouring a revoked one.
    pub fn reload(&mut self) -> io::Result<()> {
        let fresh = Self::load_from(self.path.clone())?;
        self.entries = fresh.entries;
        self.stamp = fresh.stamp;
        Ok(())
    }

    fn save(&mut self) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&File {
            tokens: self.entries.clone(),
        })
        .map_err(io::Error::other)?;
        state::write_private(&self.path, &text)?;
        // So this process's own write does not read back as somebody else's.
        self.stamp = stamp_of(&self.path);
        Ok(())
    }

    pub fn list(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mint a token, keep its hash, and hand back the token itself.
    ///
    /// The only time the token exists in full. There is no way to ask for it
    /// again, which is the property the hash is for -- and the reason `pair`
    /// prints it rather than saving it somewhere convenient.
    pub fn create(&mut self, name: &str) -> io::Result<String> {
        let token = mint()?;
        self.entries.push(Entry {
            name: name.to_string(),
            hash: hash(&token),
            created_at: now_epoch(),
        });
        self.save()?;
        Ok(token)
    }

    /// Remove every token with this name. Returns how many went.
    pub fn revoke(&mut self, name: &str) -> io::Result<usize> {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        let gone = before - self.entries.len();
        if gone > 0 {
            self.save()?;
        }
        Ok(gone)
    }

    /// The entry a presented token belongs to, if any.
    ///
    /// Every entry is compared even after one matches. Stopping early would
    /// make the time taken depend on the position of the match, which says
    /// something about the file to anyone timing it.
    pub fn verify(&self, presented: &str) -> Option<&Entry> {
        let want = hash(presented);
        let mut found = None;
        for entry in &self.entries {
            if entry.hash.as_bytes().ct_eq(want.as_bytes()).into() {
                found = Some(entry);
            }
        }
        found
    }
}

/// 32 bytes from the OS, hex encoded.
///
/// Hex rather than base64 so it survives being pasted through anything that
/// treats `+` or `/` as special -- a URL, a shell, a QR code's alphanumeric
/// mode -- for the sake of a string twice as long as it needs to be.
fn mint() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(io::Error::other)?;
    Ok(hex(&bytes))
}

pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sbxd-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p.join("tokens.json")
    }

    #[test]
    fn a_minted_token_verifies_and_a_wrong_one_does_not() {
        let path = scratch("verify");
        let mut tokens = Tokens::load_from(&path).unwrap();
        assert!(tokens.is_empty());

        let token = tokens.create("laptop").unwrap();
        assert_eq!(tokens.verify(&token).unwrap().name, "laptop");
        assert!(tokens.verify("not-the-token").is_none());
        // Off by one character is the case a comparison bug would let through.
        let mut nearly = token.clone();
        nearly.pop();
        nearly.push(if token.ends_with('a') { 'b' } else { 'a' });
        assert!(tokens.verify(&nearly).is_none());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// The property the whole module is for: what is on disk cannot be
    /// presented as a credential.
    #[test]
    fn the_file_holds_no_token_that_would_work() {
        let path = scratch("hashed");
        let mut tokens = Tokens::load_from(&path).unwrap();
        let token = tokens.create("desktop").unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(&token),
            "the token itself was written out"
        );
        assert!(on_disk.contains(&hash(&token)));

        // And the hash is not accepted in place of the token it hashes.
        assert!(tokens.verify(&hash(&token)).is_none());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn tokens_survive_a_restart_and_can_be_revoked_by_name() {
        let path = scratch("reload");
        let (a, b) = {
            let mut tokens = Tokens::load_from(&path).unwrap();
            (
                tokens.create("laptop").unwrap(),
                tokens.create("phone").unwrap(),
            )
        };

        let mut reloaded = Tokens::load_from(&path).unwrap();
        assert_eq!(reloaded.list().len(), 2);
        assert!(reloaded.verify(&a).is_some());

        assert_eq!(reloaded.revoke("laptop").unwrap(), 1);
        assert!(reloaded.verify(&a).is_none(), "revoked and still accepted");
        assert!(reloaded.verify(&b).is_some(), "revoked the wrong one");
        assert_eq!(reloaded.revoke("laptop").unwrap(), 0);

        // And the revocation is what a restarted server reads.
        assert!(Tokens::load_from(&path).unwrap().verify(&a).is_none());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn two_tokens_are_never_the_same_and_look_like_256_bits_of_hex() {
        let path = scratch("mint");
        let mut tokens = Tokens::load_from(&path).unwrap();
        let a = tokens.create("one").unwrap();
        let b = tokens.create("two").unwrap();

        assert_ne!(a, b);
        assert_eq!(a.len(), 64, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// `sbxd pair` and `sbxd revoke` both run in a *different process* from
    /// the server. If the running server did not notice, revoking would not
    /// revoke until somebody restarted it, which is the failure this guards.
    #[test]
    fn a_write_by_another_process_is_noticed_and_reread() {
        let path = scratch("reload-on-change");
        let mut server_side = Tokens::load_from(&path).unwrap();
        assert!(!server_side.changed_on_disk(), "nothing has happened yet");

        // Another process pairs a client.
        let token = Tokens::load_from(&path).unwrap().create("laptop").unwrap();

        assert!(server_side.changed_on_disk());
        assert!(
            server_side.verify(&token).is_none(),
            "not until it has reloaded"
        );
        server_side.reload().unwrap();
        assert!(server_side.verify(&token).is_some());
        assert!(!server_side.changed_on_disk());

        // And the same in the direction that matters: another process revokes.
        Tokens::load_from(&path).unwrap().revoke("laptop").unwrap();
        assert!(server_side.changed_on_disk());
        server_side.reload().unwrap();
        assert!(
            server_side.verify(&token).is_none(),
            "a revoked token stayed good"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// The server writes this file too, through `pair`. Its own write must not
    /// read back as somebody else's, or every mint would cost a needless reread.
    #[test]
    fn a_write_of_our_own_is_not_seen_as_a_change() {
        let path = scratch("own-write");
        let mut tokens = Tokens::load_from(&path).unwrap();
        tokens.create("laptop").unwrap();
        assert!(!tokens.changed_on_disk());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A file that cannot be read leaves the set alone. Forgetting every token
    /// because of a transient disk error would lock out every client at once.
    #[test]
    fn a_failed_reload_keeps_the_tokens_it_had() {
        let path = scratch("bad-reload");
        let mut tokens = Tokens::load_from(&path).unwrap();
        let token = tokens.create("laptop").unwrap();

        std::fs::write(&path, "{ not json").unwrap();
        assert!(tokens.reload().is_err());
        assert!(
            tokens.verify(&token).is_some(),
            "a garbled file must not unauthenticate everyone"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_hash_is_the_one_sha256_everything_else_agrees_on() {
        // The empty string's SHA-256, so a change of algorithm cannot pass
        // unnoticed -- it would invalidate every token file in existence.
        assert_eq!(
            hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}

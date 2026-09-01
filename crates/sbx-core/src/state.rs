//! Where secrets live: keys, tokens, and the connections that carry them.
//!
//! Used by the server for its certificate and its tokens, and by a client for
//! the servers it has been paired with. Both are the same kind of thing: a
//! secret this machine holds.
//!
//! `$XDG_STATE_HOME/sbx`, falling back to `~/.local/state/sbx` -- deliberately
//! not `$XDG_CONFIG_HOME/sbx`, where the session cache and `config.toml`
//! already live and where one fewer directory would have been convenient. A
//! private key and a file of token hashes are state a machine generated, not
//! configuration a person edits; config directories are the ones people copy
//! between machines and check into dotfile repositories, and a private key or a
//! bearer token following someone onto a second machine is the whole failure.

use std::io;
use std::path::{Path, PathBuf};

/// `$XDG_STATE_HOME/sbx`, or `~/.local/state/sbx`.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sbx")
}

/// Create a directory that only its owner may read.
///
/// The mode is set at creation rather than after it, because the window between
/// the two is one another process can read the key in -- short, but this is the
/// one directory where that matters.
pub fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if path.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Write a file only its owner may read, replacing whatever was there.
///
/// Via a temporary file and a rename, like the session cache: a key half
/// written by an interrupted start is worse than no key, because it looks like
/// a key.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sbxd-state-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn the_state_dir_is_not_the_config_dir() {
        // The distinction this module exists for. Asserted rather than trusted
        // to the comment, because "put it next to sessions.json" is the obvious
        // simplification for someone who has not read the reason.
        let dir = dir();
        assert!(dir.ends_with("sbx"), "{dir:?}");
        assert!(
            !dir.to_string_lossy().contains("/.config/"),
            "a private key does not belong in a config directory: {dir:?}"
        );
    }

    #[test]
    fn xdg_state_home_wins_when_it_is_set() {
        // Read through the same env the process would; no other test in this
        // crate sets it.
        let before = std::env::var_os("XDG_STATE_HOME");
        unsafe { std::env::set_var("XDG_STATE_HOME", "/tmp/statehome") };
        assert_eq!(dir(), PathBuf::from("/tmp/statehome/sbx"));
        match before {
            Some(v) => unsafe { std::env::set_var("XDG_STATE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }

    #[test]
    fn a_written_secret_is_readable_only_by_its_owner() {
        let dir = scratch("write");
        let path = dir.join("nested").join("key.pem");
        write_private(&path, "secret").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode was {mode:o}");
        let dmode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dmode, 0o700, "directory mode was {dmode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writing_again_replaces_and_leaves_no_temporary_behind() {
        let dir = scratch("replace");
        let path = dir.join("tokens.json");
        write_private(&path, "one").unwrap();
        write_private(&path, "two").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("tmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

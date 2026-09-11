//! Reading a worktree's files, from outside it.
//!
//! The working copy is inside the sandbox, so every one of these is an exec.
//! That shapes the interface: directories are listed one at a time as a tree is
//! expanded rather than walked in one go, because a repository is tens of
//! thousands of files and a tree only ever shows the few that are open.
//!
//! **Reading is all there is.** The agent owns the working copy -- it is the
//! thing editing it, and two writers with no shared lock is how a file gets
//! half of each. What a person wants here is to see what the agent did, and to
//! say something about it, which is what the review is for.
//!
//! Paths are relative to the repository root and are checked here rather than
//! trusted: a client sending `../../etc/shadow` is asking to read the sandbox
//! outside the working copy, and while the sandbox is the isolation boundary
//! rather than this code, a file browser that will fetch anything on the
//! filesystem is not what this claims to be.

use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::seed::sh_quote;
use crate::session::{Session, State};

/// Sentinel a script prints when the working copy is not there at all.
///
/// The same machine-readable channel [`crate::publish`] uses, and for the same
/// reason: it has to be told apart from anything the shell might say on its own.
pub(crate) const NO_REPO: &str = "@@sbx-no-repo@@";

/// What every pane says when a session has no working copy.
///
/// The tree, the file viewer and the git pane all ask the sandbox about the
/// same directory, and each of them used to answer with whatever `sh` said
/// about it -- `ls: cannot access '/sandbox/repo': No such file or directory`
/// in the tree, `sh: 1: cd: can't cd to /sandbox/repo` in git. That is the
/// shell answering a question nobody asked, in a path the reader has no reason
/// to have heard of.
///
/// Two sentences, because there are two ways to have no working copy and the
/// right thing to do differs: one is to wait, the other is to start again.
pub fn no_working_copy(session: &Session) -> String {
    match session.state {
        State::Creating | State::Seeding => format!(
            "`{}` is still being prepared; its files appear once the clone finishes",
            session.name
        ),
        _ => format!(
            "`{}` has no working copy: its clone never finished, so there is nothing here to \
             show. Create the session again to retry it.",
            session.name
        ),
    }
}

/// How much of a file is read. A viewer showing half a megabyte is already
/// showing more than anyone reads; the rest is a scroll bar lying about how
/// much there is.
const CAP: u64 = 512 * 1024;

/// One entry in a directory.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// A directory's contents, directories first and then by name -- which is how a
/// tree is read rather than how `ls` happens to sort.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dir {
    /// Relative to the repository root; empty for the root itself.
    pub path: String,
    pub entries: Vec<Entry>,
}

/// A file, as much of it as is worth showing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileText {
    pub path: String,
    /// Empty when `binary`. Lossy where the bytes are not UTF-8, which is what
    /// a viewer wants: one replacement character beats refusing the file.
    pub text: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes: u64,
    /// Whether `text` is only the first [`CAP`] bytes of it.
    pub truncated: bool,
    /// Decided by a NUL in the first block, which is what `git` does and is
    /// right far more often than any extension list.
    pub binary: bool,
}

/// Reject anything that is not a path inside the working copy.
///
/// Checked by component rather than by looking for `..` in the string: `a/../b`
/// is fine and `..%2f` is not a thing a path has in it, but a component that
/// *is* `..` is the one case that escapes.
pub fn clean(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_start_matches('/');
    let mut parts = Vec::new();
    for part in trimmed.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err("that path leaves the working copy".into()),
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

/// A cleaned relative path, under the working copy of *this* session.
///
/// The root is the backend's: `/sandbox/repo` for a sandboxed session and the
/// worktree's own directory for the other. The path was already checked
/// component by component by [`clean`], which is what makes joining it safe.
fn absolute(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{rel}")
    }
}

/// A shell's complaint, turned into a sentence when it is one of the two that
/// have a better wording, and kept as it is when it is not.
///
/// `No such file or directory` means the tree is showing something that has
/// since been deleted, which is worth saying in those words rather than in
/// `ls`'s. Anything else is a genuine surprise, and hiding what the sandbox
/// said about it would leave nothing to investigate.
fn gone_or(stderr: &str, gone: &str) -> String {
    let said = stderr.trim();
    if said.contains("No such file or directory") {
        return gone.to_string();
    }
    if said.is_empty() {
        return "the sandbox would not read it, and said nothing about why".to_string();
    }
    format!("the sandbox could not read it: {said}")
}

/// List one directory.
pub fn list(backend: &dyn Backend, session: &Session, path: &str) -> Result<Dir, String> {
    let rel = clean(path)?;
    let root = backend.paths(session).repo;
    // `-p` marks directories with a trailing slash, `-A` includes dotfiles but
    // not `.` and `..`. A filename with a newline in it would split into two
    // entries; git will not track one, and the cost of handling it is a format
    // busybox's `ls` does not have.
    let script = format!(
        "[ -d {root} ] || {{ printf '%s\\n' {mark}; exit 0; }}\nls -Ap -- {dir}",
        root = sh_quote(&root),
        mark = sh_quote(NO_REPO),
        dir = sh_quote(&absolute(&root, &rel)),
    );
    let out = backend
        .exec(session, &["sh", "-c", &script])
        .map_err(|e| e.to_string())?;
    // The root, rather than the directory asked for: a session whose clone
    // never happened has no tree at all, and saying that once is worth more
    // than saying `ls: cannot access` about every path in it.
    if out.trimmed() == NO_REPO {
        return Err(no_working_copy(session));
    }
    if !out.ok() {
        return Err(gone_or(&out.stderr, "that directory is not there any more"));
    }

    let mut entries: Vec<Entry> = out
        .trimmed()
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(|l| match l.strip_suffix('/') {
            Some(name) => Entry {
                name: name.to_string(),
                dir: true,
            },
            None => Entry {
                name: l.to_string(),
                dir: false,
            },
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));

    Ok(Dir { path: rel, entries })
}

/// Read one file.
pub fn read(backend: &dyn Backend, session: &Session, path: &str) -> Result<FileText, String> {
    let rel = clean(path)?;
    let root = backend.paths(session).repo;
    if rel.is_empty() {
        return Err("that is the repository, not a file".into());
    }
    // Size first, then the capped head, base64 so the bytes survive a transport
    // that hands back a `String`: an exec's stdout is already lossy UTF-8, and a
    // source file with a stray byte in it would come back altered.
    let script = format!(
        "[ -d {root} ] || {{ printf '%s\\n' {mark}; exit 0; }}\np={path}; [ -f \"$p\" ] || {{ echo missing; exit 3; }}; wc -c < \"$p\"; head -c {CAP} \"$p\" | base64 | tr -d '\\n'",
        root = sh_quote(&root),
        mark = sh_quote(NO_REPO),
        path = sh_quote(&absolute(&root, &rel)),
    );
    let out = backend
        .exec(session, &["sh", "-c", &script])
        .map_err(|e| e.to_string())?;
    if out.trimmed() == NO_REPO {
        return Err(no_working_copy(session));
    }
    // Exit 3 is the script's own way of saying the file is gone, and it says it
    // on stdout -- so the old wording put an empty stderr after a colon.
    if out.exit_code == 3 || out.trimmed() == "missing" {
        return Err("that file is not there any more".into());
    }
    if !out.ok() {
        return Err(gone_or(&out.stderr, "that file is not there any more"));
    }

    let mut lines = out.trimmed().splitn(2, '\n');
    let bytes: u64 = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .ok_or("the sandbox did not say how big that file is")?;
    let encoded = lines.next().unwrap_or("").trim();
    let raw = decode_base64(encoded).ok_or("the file came back malformed")?;

    let binary = raw.contains(&0);
    Ok(FileText {
        path: rel,
        text: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&raw).into_owned()
        },
        bytes,
        truncated: bytes > CAP,
        binary,
    })
}

/// Base64, without a dependency for it.
///
/// The encoder is the sandbox's `base64`; this is the other half. Sixty lines
/// of crate for one function that has to exist anyway on the client side of the
/// terminal channel is not a trade worth making twice.
pub(crate) fn decode_base64(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A component that *is* `..` escapes; one that merely contains dots does
    /// not, and a viewer that refused `..config` would be wrong.
    #[test]
    fn a_path_cannot_leave_the_working_copy() {
        assert!(clean("../../etc/shadow").is_err());
        assert!(clean("src/../../..").is_err());
        assert_eq!(clean("src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(clean("/src//main.rs").unwrap(), "src/main.rs");
        assert_eq!(clean("./src/./x").unwrap(), "src/x");
        assert_eq!(clean("..config").unwrap(), "..config");
        assert_eq!(clean("").unwrap(), "");
    }

    /// The root is the repository itself, not a path under it.
    #[test]
    fn the_empty_path_is_the_repository_root() {
        let root = crate::session::REPO_PATH;
        assert_eq!(absolute(root, ""), root);
        assert_eq!(absolute(root, "a/b"), format!("{root}/a/b"));
        // And the same rule wherever the working copy is.
        assert_eq!(absolute("/srv/worktrees/x", "a"), "/srv/worktrees/x/a");
    }

    #[test]
    fn base64_round_trips_what_the_sandbox_would_send() {
        // `printf 'hello\n' | base64` -- and a byte that is not UTF-8, which is
        // the case the encoding exists for.
        assert_eq!(decode_base64("aGVsbG8K").unwrap(), b"hello\n");
        assert_eq!(decode_base64("/w==").unwrap(), vec![0xff]);
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        assert!(decode_base64("not base64!").is_none());
    }
}

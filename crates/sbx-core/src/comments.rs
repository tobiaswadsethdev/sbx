//! Review comments on a session's diff, kept until they are sent to the agent.
//!
//! A review is written a line at a time and delivered in one go: telling the
//! agent about each remark as it is written would interrupt it six times to say
//! six things that belong in one message, and the second interruption lands
//! while it is acting on the first.
//!
//! Kept on the **server**, per session, beside the events feed and for the same
//! reason: a client is a window onto a session, and a review half-written when
//! the window closes is work. It also means the review is the session's rather
//! than the window's, so a second client sees it and the agent is told once
//! whichever one sends it.
//!
//! What is stored is what the person wrote plus enough to say where they wrote
//! it. Not a line *identity*: the working copy moves under a review -- the agent
//! is still going -- and a comment that tried to follow a line through later
//! edits would either be wrong or need a diff of the diff. The excerpt is kept
//! verbatim instead, so the message quotes what was actually being looked at
//! even when the file has moved on.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remark against one line of a diff.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Unique within a session, and the handle for removing one. Assigned from
    /// the highest already present rather than from the count, so removing the
    /// middle of a review cannot make the next comment reuse an id.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub id: u64,
    /// Path as the diff gives it, without the `a/` or `b/` prefix.
    pub file: String,
    /// Line number in the file, as the diff's hunk header counts it. Zero for a
    /// comment against a file rather than a line -- an untracked file has no
    /// line numbers to point at.
    pub line: u32,
    /// The diff line being commented on, verbatim, `+`/`-`/space and all.
    pub excerpt: String,
    pub body: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub at: u64,
}

/// A comment a client is asking to add.
///
/// The id and the timestamp are the server's to assign: two clients writing at
/// once would otherwise agree on an id and one of them would vanish.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewComment {
    pub file: String,
    pub line: u32,
    pub excerpt: String,
    pub body: String,
}

/// Where a session's unsent review lives.
fn path(session: &str) -> PathBuf {
    // Beside the events feed, under a directory of its own so a session name can
    // never collide with `sessions.json`.
    dir().join(format!("{session}.jsonl"))
}

fn dir() -> PathBuf {
    crate::store::Store::default_path().with_file_name("comments")
}

/// The unsent review, oldest first -- which is reading order for a diff.
pub fn list(session: &str) -> Vec<Comment> {
    list_at(&path(session))
}

/// Each of these has a twin taking the file, for the reason
/// [`crate::store::Store::load_from`] does: a review lives in the config
/// directory, and a test that had to move `XDG_CONFIG_HOME` to reach it would
/// be a test that races every other test doing the same.
pub fn list_at(path: &Path) -> Vec<Comment> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        // A line that will not parse is dropped rather than failing the read:
        // one corrupt record should cost that comment, not the review.
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn write(path: &Path, comments: &[Comment]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut text = String::new();
    for c in comments {
        let line = serde_json::to_string(c).map_err(|e| e.to_string())?;
        text.push_str(&line);
        text.push('\n');
    }
    fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Add one, and answer with the review as it now stands.
pub fn add(session: &str, new: NewComment) -> Result<Vec<Comment>, String> {
    add_at(&path(session), new)
}

pub fn add_at(path: &Path, new: NewComment) -> Result<Vec<Comment>, String> {
    if new.body.trim().is_empty() {
        return Err("a comment with nothing in it".into());
    }
    let mut all = list_at(path);
    let id = all.iter().map(|c| c.id).max().unwrap_or(0) + 1;
    all.push(Comment {
        id,
        file: new.file,
        line: new.line,
        excerpt: new.excerpt,
        body: new.body,
        at: now(),
    });
    write(path, &all)?;
    Ok(all)
}

/// Remove one by id. Removing one that is not there is not an error: two
/// clients deleting the same comment should both end up with it gone.
pub fn remove(session: &str, id: u64) -> Result<Vec<Comment>, String> {
    remove_at(&path(session), id)
}

pub fn remove_at(path: &Path, id: u64) -> Result<Vec<Comment>, String> {
    let mut all = list_at(path);
    all.retain(|c| c.id != id);
    write(path, &all)?;
    Ok(all)
}

/// Drop the whole review. Called once it has been delivered.
pub fn clear(session: &str) -> Result<(), String> {
    let path = path(session);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not clear {}: {e}", path.display())),
    }
}

/// The review as one message for the agent.
///
/// Grouped by file and in line order, because that is how it will be acted on;
/// the excerpt is quoted so the agent does not have to trust that the line
/// numbers still mean what they meant when the review was written.
///
/// Plain text with no markup of its own. Whatever is on the other end is a
/// terminal agent reading a paste, and a format it has to parse is a format it
/// can get wrong.
pub fn message(comments: &[Comment]) -> String {
    let mut sorted: Vec<&Comment> = comments.iter().collect();
    sorted.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.id.cmp(&b.id))
    });

    let count = sorted.len();
    let mut out = format!(
        "Here {} {} review comment{} on the diff:\n",
        if count == 1 { "is" } else { "are" },
        count,
        if count == 1 { "" } else { "s" },
    );

    let mut current = "";
    for c in sorted {
        if c.file != current {
            out.push_str(&format!("\n{}\n", c.file));
            current = &c.file;
        }
        if c.line > 0 {
            out.push_str(&format!("  line {}: {}\n", c.line, c.excerpt.trim_end()));
        } else {
            out.push_str(&format!("  {}\n", c.excerpt.trim_end()));
        }
        for line in c.body.trim().lines() {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: u64, file: &str, line: u32, excerpt: &str, body: &str) -> Comment {
        Comment {
            id,
            file: file.into(),
            line,
            excerpt: excerpt.into(),
            body: body.into(),
            at: 0,
        }
    }

    /// Grouped by file and in line order, whatever order they were written in:
    /// a review is acted on file by file, not in the order someone happened to
    /// notice things.
    #[test]
    fn the_message_is_grouped_by_file_and_in_line_order() {
        let msg = message(&[
            comment(1, "src/b.rs", 10, "+    let x = 1;", "unused"),
            comment(2, "src/a.rs", 30, "+    bar()", "second"),
            comment(3, "src/a.rs", 4, "+    foo()", "first"),
        ]);
        let a = msg.find("src/a.rs").unwrap();
        let b = msg.find("src/b.rs").unwrap();
        assert!(a < b, "{msg}");
        assert!(
            msg.find("first").unwrap() < msg.find("second").unwrap(),
            "{msg}"
        );
        assert!(msg.starts_with("Here are 3 review comments"), "{msg}");
    }

    /// One comment reads as one, not as "1 comments".
    #[test]
    fn one_comment_reads_as_one() {
        let msg = message(&[comment(1, "a.rs", 1, "+x", "no")]);
        assert!(
            msg.starts_with("Here is 1 review comment on the diff:"),
            "{msg}"
        );
    }

    /// A comment against a file rather than a line -- an untracked file has no
    /// line numbers -- says so instead of claiming line zero.
    #[test]
    fn a_comment_with_no_line_does_not_claim_line_zero() {
        let msg = message(&[comment(1, "notes.md", 0, "notes.md", "why is this here")]);
        assert!(!msg.contains("line 0"), "{msg}");
        assert!(msg.contains("why is this here"), "{msg}");
    }

    /// A body of several lines stays several lines, indented under its comment,
    /// so the agent can tell where one remark ends and the next begins.
    #[test]
    fn a_multi_line_body_stays_indented_under_its_comment() {
        let msg = message(&[comment(1, "a.rs", 2, "+x", "one\ntwo")]);
        assert!(msg.contains("    one\n    two\n"), "{msg}");
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sbx-comments-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("s.jsonl")
    }

    fn draft(file: &str, line: u32, body: &str) -> NewComment {
        NewComment {
            file: file.into(),
            line,
            excerpt: "+ something".into(),
            body: body.into(),
        }
    }

    /// A review survives being written and read back, which is the whole reason
    /// it is on the server rather than in the window.
    #[test]
    fn a_review_round_trips() {
        let path = scratch("roundtrip");
        assert!(list_at(&path).is_empty());
        add_at(&path, draft("a.rs", 1, "first")).unwrap();
        let all = add_at(&path, draft("b.rs", 2, "second")).unwrap();
        assert_eq!(all.len(), 2);
        let read = list_at(&path);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].body, "first");
        assert_eq!(read[1].file, "b.rs");
    }

    /// Ids come from the highest already present, not from the count: removing
    /// the middle of a review must not make the next comment reuse an id, or
    /// deleting one would delete two.
    #[test]
    fn removing_the_middle_does_not_let_an_id_be_reused() {
        let path = scratch("ids");
        add_at(&path, draft("a.rs", 1, "one")).unwrap();
        add_at(&path, draft("a.rs", 2, "two")).unwrap();
        add_at(&path, draft("a.rs", 3, "three")).unwrap();
        remove_at(&path, 2).unwrap();
        let all = add_at(&path, draft("a.rs", 4, "four")).unwrap();
        let ids: Vec<u64> = all.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 3, 4]);
    }

    /// Removing one that has already gone is not an error: two windows on the
    /// same session may both be told to remove it.
    #[test]
    fn removing_one_that_is_not_there_is_not_an_error() {
        let path = scratch("gone");
        add_at(&path, draft("a.rs", 1, "one")).unwrap();
        let all = remove_at(&path, 99).unwrap();
        assert_eq!(all.len(), 1);
    }

    /// An empty body is refused where it is written rather than turning into a
    /// blank line in the message the agent is sent.
    #[test]
    fn a_comment_with_nothing_in_it_is_refused() {
        let path = scratch("empty");
        assert!(add_at(&path, draft("a.rs", 1, "   ")).is_err());
        assert!(list_at(&path).is_empty());
    }

    /// One unreadable line costs that comment, not the review around it.
    #[test]
    fn a_corrupt_line_does_not_lose_the_rest_of_the_review() {
        let path = scratch("corrupt");
        add_at(&path, draft("a.rs", 1, "kept")).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{not json at all\n");
        fs::write(&path, text).unwrap();
        let all = list_at(&path);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "kept");
    }
}

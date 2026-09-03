//! Git, as the working copy inside a sandbox sees it.
//!
//! Every one of these is an exec, and execs against one sandbox are serialised,
//! so this is deliberately a small number of coarse calls rather than a git
//! library's worth of fine ones.
//!
//! **The agent is editing while you look at this.** That is the fact that
//! shapes the interface: a status is a snapshot that is already slightly out of
//! date, staging a file records a version of it that may change before the
//! commit, and discarding one races whatever the agent is doing to it. None of
//! that is fixable from here -- git's index is the only lock there is, and the
//! agent does not take it -- so what this does instead is never pretend
//! otherwise: every mutation reports what git actually said, and the caller
//! re-reads the status afterwards rather than assuming its own change landed.

use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::seed::sh_quote;
use crate::session::Session;

/// What happened to one path.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// In the working copy and in nothing else. Only ever unstaged.
    Untracked,
    /// Both sides of a merge touched it. Shown, not resolved: resolving one is
    /// editing, and editing is the agent's.
    Conflicted,
}

/// One path, and what happened to it.
///
/// Not `Entry`: `files::Entry` is already that, and every exported type lands
/// in one flat directory of generated TypeScript, where the second one to be
/// written silently replaces the first.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub change: Change,
}

/// The working copy, as git describes it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub branch: String,
    /// The remote-tracking branch, when the branch has one. `None` is a branch
    /// that has never been pushed, which is a different thing from being in
    /// sync and is why pushing says "publish" rather than nothing.
    pub upstream: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ahead: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub behind: u32,
    /// Ready to be committed: the index against `HEAD`.
    pub staged: Vec<ChangedFile>,
    /// Not yet: the working copy against the index, plus everything untracked.
    pub unstaged: Vec<ChangedFile>,
}

/// Parse `git status --porcelain=v1 --branch --untracked-files=all`.
///
/// Separated from the exec so the shape of git's output -- which is the part
/// that is easy to get subtly wrong and impossible to notice -- can be tested
/// without a sandbox.
pub fn parse_status(out: &str) -> Status {
    let mut status = Status::default();

    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            parse_branch(rest, &mut status);
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let (codes, path) = line.split_at(2);
        let path = unquote(path.trim_start());
        // A rename reads `old -> new`; the new name is the one that exists and
        // the one anything else here would ask about.
        let path = match path.split_once(" -> ") {
            Some((_, new)) => new.to_string(),
            None => path,
        };
        let mut codes = codes.chars();
        let index = codes.next().unwrap_or(' ');
        let worktree = codes.next().unwrap_or(' ');

        // Both sides marked, or either marked `U`, is a conflict -- not one
        // staged change and one unstaged one.
        if index == 'U' || worktree == 'U' || (index == 'A' && worktree == 'A') {
            status.unstaged.push(ChangedFile {
                path,
                change: Change::Conflicted,
            });
            continue;
        }
        if index == '?' {
            status.unstaged.push(ChangedFile {
                path,
                change: Change::Untracked,
            });
            continue;
        }
        if let Some(change) = code(index) {
            status.staged.push(ChangedFile {
                path: path.clone(),
                change,
            });
        }
        if let Some(change) = code(worktree) {
            status.unstaged.push(ChangedFile { path, change });
        }
    }

    status
}

fn code(c: char) -> Option<Change> {
    match c {
        'A' => Some(Change::Added),
        'M' => Some(Change::Modified),
        'D' => Some(Change::Deleted),
        'R' | 'C' => Some(Change::Renamed),
        _ => None,
    }
}

/// `main...origin/main [ahead 1, behind 2]`, and the shapes it takes when there
/// is no upstream or no divergence.
fn parse_branch(rest: &str, status: &mut Status) {
    let (names, counts) = match rest.split_once(" [") {
        Some((n, c)) => (n, c.trim_end_matches(']')),
        None => (rest, ""),
    };
    match names.split_once("...") {
        Some((branch, upstream)) => {
            status.branch = branch.to_string();
            status.upstream = Some(upstream.to_string());
        }
        None => {
            // `## HEAD (no branch)` on a detached HEAD, and a plain name on a
            // branch that has never been pushed.
            status.branch = names.to_string();
            status.upstream = None;
        }
    }
    for part in counts.split(", ").filter(|p| !p.is_empty()) {
        let (word, n) = part.split_once(' ').unwrap_or((part, "0"));
        let n = n.trim().parse().unwrap_or(0);
        match word {
            "ahead" => status.ahead = n,
            "behind" => status.behind = n,
            _ => {}
        }
    }
}

/// git quotes a path with anything unusual in it, C-style. Undone here so the
/// name shown is the name on disk.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Which two versions of a file a diff is between.
///
/// Named for the list the file was clicked in, because that is the question the
/// person asking has in mind. A file under "staged" means "what will this
/// commit contain"; one under "changes" means "what have I not staged yet".
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Against {
    /// `HEAD` against the index.
    Staged,
    /// The index against the working copy.
    Worktree,
    /// The base branch against the working copy: everything this worktree has
    /// done, which is the review question rather than the commit one.
    Base,
}

/// Two versions of one file, for a side-by-side diff.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// Empty when the file did not exist on that side, which is what an added
    /// file looks like and is a diff of nothing against everything.
    pub original: String,
    pub modified: String,
    /// What the two sides are, for the editor's headings.
    pub original_label: String,
    pub modified_label: String,
    pub binary: bool,
}

/// Fetch both sides of one file's diff.
///
/// One exec, not two: they are serialised per sandbox, and a diff of a file is
/// the sort of thing someone clicks through twenty of.
pub fn file_diff(
    backend: &dyn Backend,
    session: &Session,
    path: &str,
    against: Against,
) -> Result<FileDiff, String> {
    let rel = crate::files::clean(path)?;
    if rel.is_empty() {
        return Err("that is the repository, not a file".into());
    }
    let quoted = sh_quote(&rel);

    // Each side is a `git show` of a ref, or the file itself, and each may not
    // exist -- an added file has no original and a deleted one has no modified.
    // `|| true` keeps a missing side an empty string rather than a failed exec.
    let (left, right, left_label, right_label) = match against {
        Against::Staged => (
            format!("git show HEAD:{quoted} 2>/dev/null || true"),
            format!("git show :{quoted} 2>/dev/null || true"),
            "HEAD".to_string(),
            "staged".to_string(),
        ),
        Against::Worktree => (
            format!("git show :{quoted} 2>/dev/null || true"),
            format!("cat -- {quoted} 2>/dev/null || true"),
            "staged".to_string(),
            "working copy".to_string(),
        ),
        Against::Base => (
            format!("git show \"$base:\"{quoted} 2>/dev/null || true"),
            format!("cat -- {quoted} 2>/dev/null || true"),
            session
                .base_branch
                .clone()
                .map(|b| format!("origin/{b}"))
                .unwrap_or_else(|| "the base branch".into()),
            "working copy".to_string(),
        ),
    };

    // Base64 with a marker between the halves: an exec hands back a lossy
    // `String`, so a source file with a stray byte in it would come back
    // altered, and a plain concatenation could not be split on content that
    // contains anything.
    let script = format!(
        "{resolve}\n         {{ {left}; }} | base64 | tr -d '\\n'; echo; \
         {{ {right}; }} | base64 | tr -d '\\n'",
        resolve = if matches!(against, Against::Base) {
            crate::ops::resolve_base_script(session)
        } else {
            String::new()
        },
    );

    let out = run(backend, session, &script)?;
    let mut halves = out.trim_end_matches('\n').splitn(2, '\n');
    let original = decode(halves.next().unwrap_or(""))?;
    let modified = decode(halves.next().unwrap_or(""))?;
    let binary = original.contains(&0) || modified.contains(&0);

    Ok(FileDiff {
        path: rel,
        original: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&original).into_owned()
        },
        modified: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&modified).into_owned()
        },
        original_label: left_label,
        modified_label: right_label,
        binary,
    })
}

fn decode(s: &str) -> Result<Vec<u8>, String> {
    crate::files::decode_base64(s.trim()).ok_or_else(|| "the file came back malformed".to_string())
}

fn run(backend: &dyn Backend, session: &Session, script: &str) -> Result<String, String> {
    let full = format!(
        "cd {repo} && {script}",
        repo = sh_quote(&backend.paths(session).repo)
    );
    let out = backend
        .exec(session, &["sh", "-c", &full])
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        // git's own words. A push rejected for being behind, a commit with
        // nothing staged, a pull with a conflict -- all things the person
        // asking needs to read rather than a sentence written here about them.
        let said = out.stderr.trim();
        return Err(if said.is_empty() {
            out.trimmed().to_string()
        } else {
            said.to_string()
        });
    }
    Ok(out.stdout)
}

pub fn status(backend: &dyn Backend, session: &Session) -> Result<Status, String> {
    let out = run(
        backend,
        session,
        "git status --porcelain=v1 --branch --untracked-files=all",
    )?;
    Ok(parse_status(&out))
}

pub fn stage(backend: &dyn Backend, session: &Session, path: &str) -> Result<(), String> {
    let path = crate::files::clean(path)?;
    // `add -A` on the path so a deletion stages as one; plain `add` would only
    // ever stage content that is there.
    run(
        backend,
        session,
        &format!("git add -A -- {}", sh_quote(&path)),
    )
    .map(|_| ())
}

pub fn unstage(backend: &dyn Backend, session: &Session, path: &str) -> Result<(), String> {
    let path = crate::files::clean(path)?;
    run(
        backend,
        session,
        &format!("git restore --staged -- {}", sh_quote(&path)),
    )
    .map(|_| ())
}

/// Throw away a file's changes.
///
/// The one destructive thing here, and it races the agent by definition: it can
/// be part-way through writing the file this is restoring. The caller asks
/// first; this only does as it is told.
pub fn discard(backend: &dyn Backend, session: &Session, path: &str) -> Result<(), String> {
    let path = crate::files::clean(path)?;
    let quoted = sh_quote(&path);
    // An untracked file has nothing to restore it to, so it is removed instead
    // -- which is what every git client means by discarding one.
    run(
        backend,
        session,
        &format!(
            "if git ls-files --error-unmatch -- {quoted} >/dev/null 2>&1; \
             then git restore --staged --worktree -- {quoted}; \
             else rm -rf -- {quoted}; fi"
        ),
    )
    .map(|_| ())
}

/// Split out so the guard in [`commit`] can be tested without a sandbox.
fn commit_message_is_empty(message: &str) -> bool {
    message.trim().is_empty()
}

pub fn commit(backend: &dyn Backend, session: &Session, message: &str) -> Result<String, String> {
    if commit_message_is_empty(message) {
        return Err("a commit needs a message".into());
    }
    // `-F -` rather than `-m`: a message is free text with newlines and quotes
    // in it, and the heredoc keeps it out of the command line entirely.
    let script = format!(
        "git commit -F - <<'SBX_COMMIT_MSG'\n{message}\nSBX_COMMIT_MSG",
        message = message.trim(),
    );
    run(backend, session, &script)
}

pub fn push(backend: &dyn Backend, session: &Session) -> Result<String, String> {
    // `-u` on every push, not only the first: it is a no-op once set, and
    // without it a branch that has never been pushed has no upstream to report
    // ahead and behind against afterwards.
    run(
        backend,
        session,
        &format!(
            "git push -u origin HEAD:{branch}",
            branch = sh_quote(&session.work_branch)
        ),
    )
}

pub fn pull(backend: &dyn Backend, session: &Session) -> Result<String, String> {
    // `--ff-only`: a merge commit made behind the agent's back, in a working
    // copy it is still editing, is not something to do on a button press.
    run(backend, session, "git pull --ff-only")
}

pub fn fetch(backend: &dyn Backend, session: &Session) -> Result<String, String> {
    run(backend, session, "git fetch --prune")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_line_carries_the_upstream_and_the_divergence() {
        let mut s = Status::default();
        parse_branch("sbx/x...origin/sbx/x [ahead 2, behind 3]", &mut s);
        assert_eq!(s.branch, "sbx/x");
        assert_eq!(s.upstream.as_deref(), Some("origin/sbx/x"));
        assert_eq!((s.ahead, s.behind), (2, 3));
    }

    /// A branch that has never been pushed has no upstream, which is a
    /// different thing from being in sync with one.
    #[test]
    fn a_branch_with_no_upstream_says_so() {
        let mut s = Status::default();
        parse_branch("sbx/new", &mut s);
        assert_eq!(s.branch, "sbx/new");
        assert_eq!(s.upstream, None);
        assert_eq!((s.ahead, s.behind), (0, 0));
    }

    /// The case that makes staging worth having: one file edited, staged, and
    /// then edited again appears in both lists.
    #[test]
    fn a_file_staged_and_then_edited_again_is_in_both_lists() {
        let s = parse_status("## b...origin/b\nMM src/a.rs\n");
        assert_eq!(
            s.staged,
            vec![ChangedFile {
                path: "src/a.rs".into(),
                change: Change::Modified
            }]
        );
        assert_eq!(
            s.unstaged,
            vec![ChangedFile {
                path: "src/a.rs".into(),
                change: Change::Modified
            }]
        );
    }

    #[test]
    fn the_two_columns_are_the_index_and_the_working_copy() {
        let s = parse_status("## b\nA  added.rs\n D deleted.rs\n M edited.rs\n?? new.rs\n");
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].change, Change::Added);
        let unstaged: Vec<(&str, Change)> = s
            .unstaged
            .iter()
            .map(|e| (e.path.as_str(), e.change))
            .collect();
        assert_eq!(
            unstaged,
            vec![
                ("deleted.rs", Change::Deleted),
                ("edited.rs", Change::Modified),
                ("new.rs", Change::Untracked),
            ]
        );
    }

    /// A conflict is one entry, not a staged change and an unstaged one: `UU`
    /// read column by column would claim the file was both.
    #[test]
    fn a_conflict_is_one_entry_and_not_two() {
        let s = parse_status("## b\nUU both.rs\nAA also.rs\n");
        assert!(s.staged.is_empty(), "{:?}", s.staged);
        assert_eq!(s.unstaged.len(), 2);
        assert!(s.unstaged.iter().all(|e| e.change == Change::Conflicted));
    }

    /// A rename is reported as `old -> new`, and everything downstream wants
    /// the name that exists now.
    #[test]
    fn a_rename_is_listed_under_its_new_name() {
        let s = parse_status("## b\nR  old.rs -> new.rs\n");
        assert_eq!(s.staged[0].path, "new.rs");
        assert_eq!(s.staged[0].change, Change::Renamed);
    }

    /// git quotes a path with a space or a newline in it; the name shown should
    /// be the name on disk.
    #[test]
    fn a_quoted_path_is_unquoted() {
        let s = parse_status("## b\n?? \"a file\\twith\\ttabs.rs\"\n");
        assert_eq!(s.unstaged[0].path, "a file\twith\ttabs.rs");
        assert_eq!(unquote("plain.rs"), "plain.rs");
    }

    #[test]
    fn a_commit_with_no_message_is_refused_before_a_sandbox_is_touched() {
        // Nothing to exec against here; the guard is before the call.
        assert!(super::commit_message_is_empty("   \n  "));
        assert!(!super::commit_message_is_empty("fix the thing"));
    }
}

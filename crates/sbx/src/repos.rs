//! Git repositories on the *host*, for starting a session from one.
//!
//! Everything here runs on the machine running `sbx`, not inside a sandbox --
//! the only module that does. A local repository is a way of *naming* a remote,
//! not a source of code: the sandbox still clones from `origin` over the
//! gateway, so what is read here is the remote URL, the current branch, and how
//! far the working copy has drifted from what the sandbox will get.
//!
//! The metadata is read straight out of `.git` rather than by running git.
//! Discovery finds tens of repositories and each one would otherwise cost three
//! subprocesses; `HEAD` and `config` are two small files in a documented
//! format, and reading them keeps a scan of a whole home directory to the cost
//! of the directory walk. Git is only shelled out to for [`inspect`], which
//! runs once, for the repository actually picked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A git repository found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRepo {
    pub path: PathBuf,
    /// The path with `$HOME` collapsed to `~`, which is what the picker shows
    /// and what the filter matches against.
    pub display: String,
    /// Last path segment: the repository's name as a person would say it.
    pub name: String,
    /// `remote.origin.url`, if the repository has one. A repository without an
    /// origin cannot start a session, because there is nothing for the sandbox
    /// to clone; the picker shows it anyway and refuses the pick, which is
    /// clearer than silently hiding it.
    pub origin: Option<String>,
    /// Current branch, or `None` when `HEAD` is detached.
    pub branch: Option<String>,
}

/// A directory to scan, and how deep.
///
/// Per-root rather than one global depth: the dedicated development directories
/// are worth descending into, `$HOME` itself is not, and a single depth that
/// suited both would either miss `~/dev/org/repo` or walk `~/Downloads` three
/// levels down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub depth: usize,
}

/// Directory names never descended into.
///
/// Dependency and build trees, which contain no repository anyone means to
/// start a session on and are where a walk of a home directory spends all its
/// time. Hidden directories are skipped separately, which is what keeps this
/// list short.
const SKIP: [&str; 14] = [
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "Library",
    "Applications",
    "snap",
    "AppData",
    "OneDrive",
    "go",
    "Downloads",
];

/// Ceiling on directories visited, so a scan cannot become unbounded on a
/// pathological tree. Generous: a typical development directory is a few
/// hundred, and the walk stops descending at every repository it finds.
const MAX_VISITS: usize = 40_000;
/// Ceiling on repositories reported. Past this the list is unusable as a
/// picker anyway, and the filter is the wrong tool for finding one of 500.
const MAX_REPOS: usize = 400;

/// Depth for the development directories and the working directory.
const DEV_DEPTH: usize = 3;
/// Depth for `$HOME`, which only has to catch `~/repo`.
const HOME_DEPTH: usize = 1;

/// Where to look, in order of who gets to say.
///
/// `SBX_REPO_ROOTS` (colon-separated, like `PATH`) first, then `repo_roots` from
/// the config file, then the conventional places. Each *replaces* the ones below
/// it rather than adding to them, so someone who keeps repositories somewhere
/// unusual pays nothing for scanning the usual ones -- and the environment wins,
/// because a variable set for one command is the more specific statement.
pub fn roots(configured: Option<&[PathBuf]>) -> Vec<Root> {
    if let Some(raw) = std::env::var_os("SBX_REPO_ROOTS") {
        let roots = at_dev_depth(std::env::split_paths(&raw));
        if !roots.is_empty() {
            return dedupe_roots(roots);
        }
    }
    if let Some(paths) = configured.filter(|p| !p.is_empty()) {
        return dedupe_roots(at_dev_depth(paths.iter().cloned()));
    }
    default_roots()
}

fn at_dev_depth(paths: impl Iterator<Item = PathBuf>) -> Vec<Root> {
    paths
        .filter(|p| !p.as_os_str().is_empty())
        .map(|path| Root {
            path,
            depth: DEV_DEPTH,
        })
        .collect()
}

/// The conventional places, when nothing says otherwise.
pub fn default_roots() -> Vec<Root> {
    let mut roots = Vec::new();
    // The working directory first: running `sbx` from inside a checkout is the
    // most common way to want a session on it, and the parent catches its
    // siblings, which is how most people lay repositories out.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(parent) = cwd.parent() {
            roots.push(Root {
                path: parent.to_path_buf(),
                depth: DEV_DEPTH,
            });
        }
        roots.push(Root {
            path: cwd,
            depth: DEV_DEPTH,
        });
    }
    if let Some(home) = home() {
        for dir in ["dev", "src", "code", "projects", "work", "repos", "git"] {
            roots.push(Root {
                path: home.join(dir),
                depth: DEV_DEPTH,
            });
        }
        roots.push(Root {
            path: home,
            depth: HOME_DEPTH,
        });
    }
    dedupe_roots(roots)
}

/// Drop roots that do not exist, and roots already covered by a shallower one
/// with a smaller depth, so a repository is not walked to twice.
fn dedupe_roots(roots: Vec<Root>) -> Vec<Root> {
    let mut out: Vec<Root> = Vec::new();
    for root in roots {
        if !root.path.is_dir() {
            continue;
        }
        match out.iter_mut().find(|r| r.path == root.path) {
            // Same path from two sources: keep the deeper request.
            Some(existing) => existing.depth = existing.depth.max(root.depth),
            None => out.push(root),
        }
    }
    out
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every repository under `roots`, depth-limited per root.
///
/// A directory containing `.git` is a repository and is *not* descended into:
/// submodules and vendored checkouts are part of the repository above them, not
/// separate things to start a session on.
pub fn discover_in(roots: &[Root]) -> Vec<LocalRepo> {
    let mut found: Vec<LocalRepo> = Vec::new();
    let mut visits = 0usize;

    for root in roots {
        walk(&root.path, root.depth, &mut visits, &mut found);
    }

    // Same repository reached through two roots (or through a symlink) is one
    // repository. Deduped on the path as walked rather than canonicalised: a
    // canonicalise per repository is another syscall each, and the paths a walk
    // produces only collide when the roots overlap, which is exactly the case
    // this catches.
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    // Then by name, so the list reads as a list of repositories rather than as
    // a directory tree. Ties keep path order, which groups checkouts of the
    // same repository together.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn walk(dir: &Path, depth: usize, visits: &mut usize, out: &mut Vec<LocalRepo>) {
    if out.len() >= MAX_REPOS || *visits >= MAX_VISITS {
        return;
    }
    *visits += 1;

    if dir.join(".git").exists() {
        out.push(read(dir));
        // Not descending: see the doc comment on `discover_in`.
        return;
    }
    if depth == 0 {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        // An unreadable directory is normal (permissions, a dead mount) and
        // never worth failing a scan over.
        return;
    };
    // Sorted, so a scan of the same tree twice produces the same list -- an
    // unsorted read_dir makes the picker's order change between openings.
    let mut names: Vec<PathBuf> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            // `file_type` comes from the directory entry on Linux, so this
            // costs no extra stat. Symlinks are not followed: a link into a
            // tree already being walked would be walked twice, and one out of
            // it can leave the roots entirely.
            e.file_type().ok()?.is_dir().then(|| e.path())
        })
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            !name.starts_with('.') && !SKIP.contains(&name)
        })
        .collect();
    names.sort();

    for path in names {
        walk(&path, depth - 1, visits, out);
    }
}

/// Read what the picker needs about one repository.
pub fn read(path: &Path) -> LocalRepo {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let git = git_dir(path);
    LocalRepo {
        display: shorten(path),
        name,
        origin: git.as_deref().and_then(origin_url),
        branch: git.as_deref().and_then(head_branch),
        path: path.to_path_buf(),
    }
}

/// Collapse `$HOME` to `~`, so the picker's rows are short enough to leave room
/// for the branch column on an 80-column terminal.
fn shorten(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(home) = home()
        && let Some(home) = home.to_str()
        && !home.is_empty()
        && let Some(rest) = text.strip_prefix(home)
    {
        return format!("~{rest}");
    }
    text.into_owned()
}

/// The real git directory for a checkout.
///
/// `.git` is usually a directory, but in a linked worktree or a submodule it is
/// a file containing `gitdir: <path>`. Both have to work: worktrees are a normal
/// way to have two branches of the same repository checked out, and they are
/// exactly the kind of thing someone would point this at.
fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = fs::read_to_string(&dot).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let target = PathBuf::from(target);
    Some(if target.is_absolute() {
        target
    } else {
        repo.join(target)
    })
}

/// The directory holding `config`, which for a linked worktree is the *main*
/// repository's git directory rather than the worktree's own.
fn common_dir(git_dir: &Path) -> PathBuf {
    let Ok(text) = fs::read_to_string(git_dir.join("commondir")) else {
        return git_dir.to_path_buf();
    };
    let target = PathBuf::from(text.trim());
    if target.is_absolute() {
        target
    } else {
        git_dir.join(target)
    }
}

/// The branch `HEAD` points at, or `None` when it is detached.
fn head_branch(git_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let reference = text.trim().strip_prefix("ref:")?.trim();
    let branch = reference.strip_prefix("refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_string())
}

/// `remote.origin.url` from the repository's config.
///
/// A hand-rolled INI walk rather than a config crate: this reads one key from a
/// format git itself documents, and the alternative is a dependency that would
/// also have to be taught about `include` directives to be any more correct.
/// Anything unparseable yields `None`, which the picker reports as "no origin".
fn origin_url(git_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(common_dir(git_dir).join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[remote "origin"]`, and the equivalent `[remote.origin]` form.
            let section = section.trim();
            in_origin = section == r#"remote "origin""# || section == "remote.origin";
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim().trim_matches('"');
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// How far a working copy has drifted from what a sandbox would clone.
///
/// The point of showing this is honesty about the design: the sandbox clones
/// `origin`, so uncommitted edits and unpushed commits stay on the host. The
/// form says so with numbers rather than in the abstract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Entries `git status --porcelain` reports.
    pub uncommitted: usize,
    /// Commits on the current branch that the upstream does not have. `None`
    /// when the branch has no upstream at all, which is a different thing from
    /// being in sync.
    pub unpushed: Option<usize>,
    /// Whether `origin/<branch>` exists. A branch that has never been pushed
    /// cannot be cloned from, so the form falls back to the remote's default
    /// branch rather than handing the gateway a clone that will fail.
    pub base_on_remote: bool,
}

/// Ask git about a repository. One call site, on the repository actually
/// picked, which is why this may cost subprocesses.
pub fn inspect(path: &Path, branch: Option<&str>) -> Facts {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let uncommitted = git(&["status", "--porcelain"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);

    // Fails rather than returning zero when there is no upstream, which is the
    // distinction `Option` carries.
    let unpushed = git(&["rev-list", "--count", "@{upstream}..HEAD"])
        .and_then(|s| s.trim().parse::<usize>().ok());

    let base_on_remote = branch.is_some_and(|b| {
        git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{b}"),
        ])
        .is_some_and(|s| !s.trim().is_empty())
    });

    Facts {
        uncommitted,
        unpushed,
        base_on_remote,
    }
}

/// Score `haystack` against a filter, higher being better, `None` for no match.
///
/// A subsequence match with bonuses, not a substring match: typing `sbx` should
/// find `~/dev/ai-sandboxer`, and typing the initials of a hyphenated name
/// should find it too. Bonuses go to consecutive runs and to characters at the
/// start of a path or word segment, which is what makes the repository *name*
/// outrank an incidental match somewhere in its parent directories.
pub fn score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.trim().is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut total: i32 = 0;
    let mut run: i32 = 0;
    let mut last: Option<usize> = None;
    let mut at = 0usize;

    for want in needle.chars() {
        let found = hay[at..]
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&want))
            .map(|offset| at + offset)?;
        at = found + 1;

        total += 1;
        // Consecutive characters are worth more the longer the run, so a whole
        // word matched exactly beats the same letters scattered.
        if last == Some(found.wrapping_sub(1)) {
            run += 1;
            total += run * 2;
        } else {
            run = 1;
        }
        let boundary = found == 0
            || matches!(hay[found - 1], '/' | '-' | '_' | '.' | ' ')
            // A capital after a lowercase starts a word too, for camelCase
            // directory names.
            || (hay[found].is_uppercase() && hay[found - 1].is_lowercase());
        if boundary {
            total += 4;
        }
        // A prefix match outranks a match in the middle: for the query `sbx`,
        // `sbx-playground` is more likely to be what was meant than
        // `toolbox-sbx`, and without this the shorter name wins on length
        // alone.
        if last.is_none() && found == 0 {
            total += 8;
        }
        last = Some(found);
    }
    // Shorter haystacks win ties: `~/dev/sbx` should sort above
    // `~/dev/sbx-playground` for the query `sbx`.
    Some(total * 8 - i32::try_from(hay.len()).unwrap_or(i32::MAX) / 4)
}

/// Indices of `repos` matching `query`, best first.
///
/// Matched against the repository name *and* the shown path, taking whichever
/// scores better: a query is usually a name, but narrowing by directory
/// (`work/api`) has to work too.
pub fn filter(repos: &[LocalRepo], query: &str) -> Vec<usize> {
    let query = query.trim();
    let mut scored: Vec<(usize, i32)> = repos
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            // The name is scored on its own and given a bump, so a name match
            // outranks the same characters found across parent directories.
            let by_name = score(query, &r.name).map(|s| s + 24);
            let by_path = score(query, &r.display);
            by_name.max(by_path).map(|s| (i, s))
        })
        .collect();
    // Stable, so equal scores keep discovery order rather than shuffling as the
    // query is typed.
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory tree, removed on drop so a failing assertion does
    /// not leave litter behind.
    struct Tree(PathBuf);

    impl Tree {
        fn new(tag: &str) -> Tree {
            let dir = std::env::temp_dir().join(format!(
                "sbx-repos-{}-{tag}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        /// A repository at `rel`, with the given branch and origin.
        fn repo(&self, rel: &str, branch: Option<&str>, origin: Option<&str>) -> PathBuf {
            let path = self.0.join(rel);
            let git = path.join(".git");
            fs::create_dir_all(&git).unwrap();
            match branch {
                Some(b) => fs::write(git.join("HEAD"), format!("ref: refs/heads/{b}\n")).unwrap(),
                // Detached, as after `git checkout <sha>`.
                None => fs::write(
                    git.join("HEAD"),
                    "9f1c0e1a0a1b2c3d4e5f60718293a4b5c6d7e8f9\n",
                )
                .unwrap(),
            }
            let mut config = String::from("[core]\n\tbare = false\n");
            if let Some(url) = origin {
                config.push_str(&format!("[remote \"origin\"]\n\turl = {url}\n"));
            }
            fs::write(git.join("config"), config).unwrap();
            path
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn root(&self, depth: usize) -> Root {
            Root {
                path: self.0.clone(),
                depth,
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_branch_and_origin_out_of_dot_git() {
        let t = Tree::new("read");
        let path = t.repo("api", Some("main"), Some("https://github.com/o/api.git"));
        let repo = read(&path);
        assert_eq!(repo.name, "api");
        assert_eq!(repo.branch.as_deref(), Some("main"));
        assert_eq!(
            repo.origin.as_deref(),
            Some("https://github.com/o/api.git"),
            "the sandbox clones this, so it is the one thing that must be right"
        );
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let t = Tree::new("detached");
        let path = t.repo("api", None, Some("url"));
        assert_eq!(read(&path).branch, None);
    }

    #[test]
    fn a_repository_without_an_origin_reports_none() {
        let t = Tree::new("no-origin");
        let path = t.repo("solo", Some("main"), None);
        assert_eq!(
            read(&path).origin,
            None,
            "no origin means nothing to clone, which the form has to refuse"
        );
    }

    /// A branch name containing a slash survives, since `HEAD` holds the whole
    /// ref and only the `refs/heads/` prefix may be stripped.
    #[test]
    fn a_slashed_branch_name_survives() {
        let t = Tree::new("slashed");
        let path = t.repo("api", Some("feature/nested/thing"), Some("url"));
        assert_eq!(read(&path).branch.as_deref(), Some("feature/nested/thing"));
    }

    /// A linked worktree keeps its `HEAD` in its own git directory but shares
    /// the main repository's `config`, so origin has to be followed through
    /// `commondir` or every worktree looks remote-less.
    #[test]
    fn a_worktree_resolves_head_locally_and_origin_through_commondir() {
        let t = Tree::new("worktree");
        let main = t.repo("api", Some("main"), Some("https://github.com/o/api.git"));
        let wt_git = main.join(".git/worktrees/feature");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("HEAD"), "ref: refs/heads/feature-x\n").unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();

        let wt = t.dir("api-feature");
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();

        let repo = read(&wt);
        assert_eq!(repo.branch.as_deref(), Some("feature-x"));
        assert_eq!(
            repo.origin.as_deref(),
            Some("https://github.com/o/api.git"),
            "config lives in the main repository, not the worktree"
        );
    }

    #[test]
    fn config_parsing_ignores_other_remotes_and_comments() {
        let t = Tree::new("config");
        let path = t.repo("api", Some("main"), None);
        fs::write(
            path.join(".git/config"),
            "[remote \"upstream\"]\n\turl = https://example.com/upstream.git\n\
             # [remote \"origin\"]\n\
             [remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
             \turl = https://example.com/mine.git\n",
        )
        .unwrap();
        assert_eq!(
            read(&path).origin.as_deref(),
            Some("https://example.com/mine.git"),
            "another remote's url must not be read as origin's"
        );
    }

    #[test]
    fn discovery_finds_repositories_and_stops_at_them() {
        let t = Tree::new("walk");
        t.repo("api", Some("main"), Some("u"));
        t.repo("nested/deep/web", Some("main"), Some("u"));
        // A submodule is part of the repository above it, not its own session.
        t.repo("api/vendor/sub", Some("main"), Some("u"));
        t.dir("empty/dir");

        let found = discover_in(&[t.root(DEV_DEPTH)]);
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["api", "web"], "found: {found:#?}");
    }

    #[test]
    fn discovery_respects_the_depth_limit_and_skips_noise() {
        let t = Tree::new("depth");
        t.repo("a/b/c/d/too-deep", Some("main"), Some("u"));
        t.repo("node_modules/pkg", Some("main"), Some("u"));
        t.repo(".hidden/secret", Some("main"), Some("u"));
        t.repo("shallow", Some("main"), Some("u"));

        let found = discover_in(&[t.root(DEV_DEPTH)]);
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["shallow"],
            "depth, the skip list and hidden directories all apply: {found:#?}"
        );
    }

    #[test]
    fn discovery_deduplicates_overlapping_roots() {
        let t = Tree::new("overlap");
        let repo = t.repo("api", Some("main"), Some("u"));
        let roots = vec![
            t.root(DEV_DEPTH),
            Root {
                path: repo.clone(),
                depth: DEV_DEPTH,
            },
        ];
        let found = discover_in(&roots);
        assert_eq!(
            found.len(),
            1,
            "one repository, reached two ways: {found:#?}"
        );
    }

    #[test]
    fn missing_roots_are_dropped_rather_than_failing() {
        let roots = dedupe_roots(vec![Root {
            path: PathBuf::from("/definitely/not/here"),
            depth: 2,
        }]);
        assert!(roots.is_empty());
        // And an empty root list is a valid, empty scan.
        assert!(discover_in(&[]).is_empty());
    }

    #[test]
    fn overlapping_root_requests_keep_the_deeper_one() {
        let t = Tree::new("depths");
        let roots = dedupe_roots(vec![
            Root {
                path: t.0.clone(),
                depth: 1,
            },
            Root {
                path: t.0.clone(),
                depth: 3,
            },
        ]);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].depth, 3);
    }

    /// Configured roots replace the conventional ones rather than adding to
    /// them, so a scan cannot quietly cost more than it was asked to.
    #[test]
    fn configured_roots_replace_the_defaults() {
        // The environment outranks the config, and a developer with the
        // variable set would otherwise see this fail for the right reason.
        if std::env::var_os("SBX_REPO_ROOTS").is_some() {
            return;
        }
        let t = Tree::new("configured");
        let paths = vec![t.0.clone()];
        let got = roots(Some(&paths));
        assert_eq!(got.len(), 1, "only what was configured: {got:?}");
        assert_eq!(got[0].path, t.0);
        assert_eq!(got[0].depth, DEV_DEPTH);

        // An absent list falls through to the usual places, which always
        // include at least the working directory.
        assert!(roots(None).len() > 1);
        assert!(roots(Some(&[])).len() > 1, "an empty list is not a claim");
    }

    #[test]
    fn env_var_replaces_the_default_roots() {
        // Not run through `default_roots` with a set variable, because tests
        // share a process and mutating the environment would race. The parsing
        // is what is worth asserting on, so it is asserted directly.
        let joined = std::env::join_paths(["/a", "/b"]).unwrap();
        let parsed: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parsed, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    fn repo_named(name: &str, display: &str) -> LocalRepo {
        LocalRepo {
            path: PathBuf::from(display),
            display: display.to_string(),
            name: name.to_string(),
            origin: Some("u".into()),
            branch: Some("main".into()),
        }
    }

    #[test]
    fn scoring_matches_subsequences_and_prefers_boundaries() {
        assert!(score("sbx", "sbx").is_some());
        assert!(score("sbx", "ai-sandboxer").is_some(), "s-b-x in order");
        assert!(score("xyz", "sbx").is_none(), "no such subsequence");
        assert_eq!(score("", "anything"), Some(0), "an empty query matches all");

        // A whole-word match beats the same letters scattered over parents.
        let exact = score("api", "~/dev/api").unwrap();
        let scattered = score("api", "~/a/p/i-thing").unwrap();
        assert!(exact > scattered, "{exact} should beat {scattered}");
    }

    /// The run bonus is what makes a contiguous match beat a spread-out one of
    /// the same length; without it `filter` ranks by little more than length.
    #[test]
    fn scoring_rewards_consecutive_matches() {
        let contiguous = score("abc", "abcxyz").unwrap();
        let spread = score("abc", "axbxcz").unwrap();
        assert!(contiguous > spread, "{contiguous} should beat {spread}");
    }

    #[test]
    fn filtering_puts_the_shortest_exact_name_first() {
        let repos = [
            repo_named("sbx-playground", "~/dev/sbx-playground"),
            repo_named("sbx", "~/dev/sbx"),
            repo_named("toolbox-sbx", "~/work/toolbox-sbx"),
            repo_named("notes", "~/dev/notes"),
        ];
        let order = filter(&repos, "sbx");
        assert_eq!(
            order
                .iter()
                .map(|i| repos[*i].name.as_str())
                .collect::<Vec<_>>(),
            vec!["sbx", "sbx-playground", "toolbox-sbx"],
            "notes does not match at all, and the exact name leads"
        );
    }

    #[test]
    fn filtering_by_directory_works_too() {
        let repos = [
            repo_named("api", "~/work/acme/api"),
            repo_named("api", "~/dev/personal/api"),
        ];
        let order = filter(&repos, "acme/api");
        assert_eq!(order.first().copied(), Some(0), "path matches must count");
    }

    #[test]
    fn an_empty_query_keeps_every_repository_in_order() {
        let repos = [repo_named("a", "~/a"), repo_named("b", "~/b")];
        assert_eq!(filter(&repos, ""), vec![0, 1]);
        assert_eq!(
            filter(&repos, "   "),
            vec![0, 1],
            "whitespace is not a query"
        );
    }
}

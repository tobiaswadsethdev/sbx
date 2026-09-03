//! The worktree backend against a real git repository.
//!
//! Not `#[ignore]`d, unlike the gateway and server tests: what this needs is
//! `git`, `sh` and `setsid`, which is what the machine running the suite
//! already has. It is here rather than in a `#[cfg(test)]` module because it
//! wants a directory layout and a detached seeder, and both read better as one
//! story than as assertions on a script.
//!
//! Nothing here touches the session cache, which is why it can run beside every
//! other test: [`Worktree`] is constructed with its two directories rather than
//! reading them from the environment, and `Store`'s path is process-global.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use sbx_core::backend::{Backend, Torn, Worktree};
use sbx_core::seed::{self, SeedState};
use sbx_core::session::{Kind, Session, State};

/// A checkout with one commit on `main`, and no remote.
///
/// Deliberately no remote: it is the case a sandboxed session cannot have -- a
/// clone always has an origin -- and the one that broke the diff, because
/// resolving a base through `origin/HEAD` finds nothing.
fn checkout(root: &Path) -> PathBuf {
    let dir = root.join("src");
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&dir)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "test"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    dir
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sbx-worktree-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn session(name: &str, repo: &Path) -> Session {
    let mut s = Session::new(
        name.to_string(),
        repo.display().to_string(),
        "write a changelog".to_string(),
    );
    s.backend = Kind::Worktree;
    s
}

/// Follow the detached seeder to its end, like `ops::create` does.
fn wait_for_seed(backend: &Worktree, s: &Session) -> SeedState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match seed::seed_state(backend, s) {
            SeedState::Running { .. } | SeedState::Unknown if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// The whole lifecycle: place, seed, list, remove.
#[test]
fn a_worktree_session_is_placed_seeded_and_taken_away() {
    let root = temp("lifecycle");
    let src = checkout(&root);
    let backend = Worktree::new(root.join("worktrees"), root.join("state"));
    let mut s = session("changelog", &src);

    backend.place(&mut s, &Default::default()).expect("placed");
    let dir = PathBuf::from(s.workdir.clone().expect("a worktree has a directory"));
    assert_eq!(dir, root.join("worktrees").join("changelog"));
    assert_eq!(
        s.tmux, "sbx-changelog",
        "the tmux name has to be the session's: one tmux server holds them all"
    );
    assert_eq!(
        s.policy, None,
        "a record must not claim a policy it does not have"
    );
    assert_eq!(
        s.base_branch.as_deref(),
        Some("main"),
        "the base is read from the checkout, since there is no origin to ask"
    );

    // `false`: no agent. Starting one would run whatever `claude` is on this
    // machine, and what is under test is the seeding.
    seed::launch(&backend, &s, false).expect("the seeder started");
    assert_eq!(wait_for_seed(&backend, &s), SeedState::Done);

    assert!(dir.join("README.md").is_file(), "the worktree has the work");
    let branch = Command::new("git")
        .current_dir(&dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "sbx/changelog"
    );

    // The record is outside the working copy, which is the whole point: inside
    // it, it would show up in every `git status` the agent runs.
    let paths = backend.paths(&s);
    assert!(!paths.meta().starts_with(&paths.repo), "{paths:?}");
    assert!(Path::new(&paths.meta()).is_file(), "{paths:?}");
    let status = Command::new("git")
        .current_dir(&dir)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "",
        "seeding must leave the working copy clean"
    );

    // What the record says it is, read back the way a lost cache would read it.
    let read = backend
        .read_meta("changelog")
        .expect("the record reads back");
    assert_eq!(read.name, "changelog");
    assert_eq!(read.backend, Kind::Worktree);
    assert_eq!(read.workdir, s.workdir);

    let live = backend.live(vec![s.clone()]).expect("live");
    assert!(live.dead.is_empty(), "{live:?}");
    assert!(
        live.orphans.is_empty(),
        "a session it was handed is not an orphan: {live:?}"
    );

    assert_eq!(
        backend.tear_down("changelog", Some(&s)).unwrap(),
        Torn::Removed
    );
    assert!(!dir.exists(), "the worktree is gone");
    assert!(
        !Path::new(&paths.meta()).exists(),
        "and so is its record: it describes a session that has ended"
    );
    // Removed from git's own list too, or `git worktree add` would refuse the
    // same path next time.
    let list = Command::new("git")
        .current_dir(&src)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&list.stdout);
    assert!(!list.contains("changelog"), "{list}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Re-seeding is how a half-created session is repaired, so neither the
/// worktree nor the branch may make a second attempt fail.
#[test]
fn seeding_twice_is_the_same_as_seeding_once() {
    let root = temp("reseed");
    let src = checkout(&root);
    let backend = Worktree::new(root.join("worktrees"), root.join("state"));
    let mut s = session("again", &src);
    backend.place(&mut s, &Default::default()).unwrap();

    seed::launch(&backend, &s, false).unwrap();
    assert_eq!(wait_for_seed(&backend, &s), SeedState::Done);
    seed::launch(&backend, &s, false).unwrap();
    assert_eq!(
        wait_for_seed(&backend, &s),
        SeedState::Done,
        "the second seeding has to reuse the worktree and the branch"
    );

    backend.tear_down("again", Some(&s)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// A worktree deleted from under the session is the way one ends, and the only
/// way this backend can tell. It must read as dead rather than as an error.
#[test]
fn a_worktree_deleted_from_under_a_session_is_dead() {
    let root = temp("dead");
    let src = checkout(&root);
    let backend = Worktree::new(root.join("worktrees"), root.join("state"));
    let mut s = session("vanishing", &src);
    backend.place(&mut s, &Default::default()).unwrap();
    seed::launch(&backend, &s, false).unwrap();
    assert_eq!(wait_for_seed(&backend, &s), SeedState::Done);
    s.state = State::Ready;

    std::fs::remove_dir_all(s.workdir.clone().unwrap()).unwrap();
    let live = backend.live(vec![s.clone()]).expect("live");
    assert_eq!(live.dead, ["vanishing"]);
    assert_eq!(live.sessions[0].state, State::Dead);

    // And back again, because a disk that had not finished mounting reported
    // every session gone and they were all still there.
    std::fs::create_dir_all(s.workdir.clone().unwrap()).unwrap();
    let mut gone = s.clone();
    gone.state = State::Dead;
    let live = backend.live(vec![gone]).expect("live");
    assert!(live.dead.is_empty());
    assert_eq!(live.sessions[0].state, State::Ready);

    // The record is what claims a session, so with the cache empty this is an
    // orphan to adopt -- which is how a worktree session survives losing the
    // cache, given there is no sandbox to hold its metadata.
    let live = backend.live(Vec::new()).expect("live");
    assert_eq!(live.orphans, ["vanishing"]);

    backend.tear_down("vanishing", Some(&s)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// A session whose record the cache has lost cannot be located, because unlike
/// a sandbox name a worktree's directory is not a function of the session's
/// name. Saying so beats deleting a directory that was guessed at.
#[test]
fn removing_a_session_with_no_record_takes_only_the_record() {
    let root = temp("norecord");
    let backend = Worktree::new(root.join("worktrees"), root.join("state"));
    assert_eq!(
        backend.tear_down("unknown", None).unwrap(),
        Torn::RecordOnly
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The shells beside the agent, on the *server's* tmux.
///
/// `#[ignore]`d, unlike the rest of this file: it starts a tmux server on the
/// machine running it, which is more than a test should do without being asked.
///
/// ```sh
/// cargo test -p sbx-core --test worktree -- --ignored
/// ```
///
/// What it is guarding is the naming. Every sandbox has a tmux server to
/// itself and can call a shell `shell-1`; here they share one with each other
/// and with whatever the person at the machine is running, so a shell that was
/// not named after its session would be offered as another session's -- and
/// `kill_shell` would close it.
#[test]
#[ignore]
fn shells_are_this_session_s_and_nobody_else_s() {
    use sbx_core::ops;

    let root = temp("shells");
    let src = checkout(&root);
    let backend = Worktree::new(root.join("worktrees"), root.join("state"));
    let mut mine = session("mine", &src);
    let mut theirs = session("theirs", &src);
    backend.place(&mut mine, &Default::default()).unwrap();
    backend.place(&mut theirs, &Default::default()).unwrap();
    for s in [&mine, &theirs] {
        seed::launch(&backend, s, false).unwrap();
        assert_eq!(wait_for_seed(&backend, s), SeedState::Done);
    }

    assert_eq!(ops::shells(&backend, &mine).unwrap(), Vec::<String>::new());
    let opened = ops::new_shell(&backend, &mine).expect("a shell opened");
    assert_eq!(opened, "sbx-mine-shell-1");
    assert_eq!(
        ops::shells(&backend, &mine).unwrap(),
        std::slice::from_ref(&opened)
    );
    assert_eq!(
        ops::shells(&backend, &theirs).unwrap(),
        Vec::<String>::new(),
        "one session's shell is not another's, even on one tmux server"
    );

    // The agent's own tmux session is not a shell and closing a tab must not
    // stop it; nor is a session belonging to somebody else.
    assert!(ops::kill_shell(&backend, &mine, &mine.tmux).is_err());
    assert!(ops::kill_shell(&backend, &theirs, &opened).is_err());
    ops::kill_shell(&backend, &mine, &opened).expect("closed");
    assert_eq!(ops::shells(&backend, &mine).unwrap(), Vec::<String>::new());

    for (name, s) in [("mine", &mine), ("theirs", &theirs)] {
        backend.tear_down(name, Some(s)).unwrap();
    }
    let _ = std::fs::remove_dir_all(&root);
}

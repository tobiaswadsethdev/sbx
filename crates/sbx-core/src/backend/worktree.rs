//! A session that is a `git worktree` on the server, with no sandbox around it.
//!
//! **It runs with the server's own rights, and every part of this file is
//! shaped by saying so.** An authenticated `sbxd` can already create containers
//! on its host; a worktree session goes further in one specific way -- the agent
//! is an ordinary process under the server's account, reading its files, using
//! its git credentials, reaching whatever the network lets it reach. There is no
//! policy to show and no decisions to feed, so those are refused with a sentence
//! rather than answered with an empty pane. See [`super::Isolation`].
//!
//! Why have it at all: a sandbox is a clone, and a clone is minutes and a
//! network for a repository the server already has a checkout of. A worktree is
//! seconds and shares the object store. For work on a machine that is already
//! yours -- and for the case the sandbox cannot serve, a toolchain or a daemon
//! the image does not carry -- it is the difference between using the tool and
//! not.
//!
//! ## The record does not live in the worktree
//!
//! A sandbox is the source of truth about itself: `meta.json` sits inside it, so
//! a session survives losing the local cache entirely. A worktree has nowhere
//! equivalent. Writing `.sbx/` into the working copy would put it in every
//! `git status` the agent runs, in every diff it is asked to review, and one
//! `git clean -fdx` from being deleted. So the record lives beside the server's
//! other state, at `$XDG_STATE_HOME/sbx/worktrees/<name>/`, and adoption after
//! a lost cache is that directory reconciled against the worktrees that still
//! exist on disk.
//!
//! ## tmux is the server's
//!
//! Every sandbox has its own tmux server and can call its agent's session
//! `agent`. Here they share one, so the names have to be the session's: the
//! agent is `sbx-<name>` and its shells are `sbx-<name>-shell-N`. Without that,
//! two worktree sessions would attach to each other's agent -- and a person's
//! own tmux sessions on the same machine would show up as shells of whichever
//! session they happened to open.

use std::path::PathBuf;
use std::process::Command;

use openshell_client::ExecOutput;

use super::{Backend, Error, Isolation, Paths, Result, Torn};
use crate::config::Config;
use crate::ops::Draft;
use crate::projects;
use crate::seed::sh_quote;
use crate::session::{self, Session, State};
use crate::store;

pub struct Worktree {
    /// Where the working copies go. One directory per session.
    root: PathBuf,
    /// Where the records go. Deliberately not under [`Self::root`]; see the
    /// module comment.
    state: PathBuf,
}

impl Worktree {
    pub fn new(root: PathBuf, state: PathBuf) -> Self {
        Worktree { root, state }
    }

    pub fn from_config(cfg: &Config) -> Self {
        Worktree::new(
            cfg.worktree_root
                .clone()
                .unwrap_or_else(Worktree::default_root),
            Worktree::default_state(),
        )
    }

    /// `$XDG_DATA_HOME/sbx/worktrees`, or `~/.local/share/sbx/worktrees`.
    ///
    /// Data rather than state: a worktree is work, with commits in it that may
    /// not have been pushed anywhere. State is what a machine can regenerate,
    /// and this is the one directory here that it cannot.
    pub fn default_root() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sbx")
            .join("worktrees")
    }

    fn default_state() -> PathBuf {
        crate::state::dir().join("worktrees")
    }

    /// The directory this session's working copy is in, whether or not it
    /// exists yet.
    ///
    /// The record's `workdir` wins, because a session created under a different
    /// configured root has to keep working after the root is changed: the
    /// convention says where a *new* one goes, and the record says where an old
    /// one went.
    fn dir(&self, session: &Session) -> PathBuf {
        match &session.workdir {
            Some(p) => PathBuf::from(p),
            None => self.root.join(&session.name),
        }
    }

    fn record_dir(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }

    /// The checkout a worktree is added to, which is the one thing a worktree
    /// session needs that a sandboxed one does not.
    ///
    /// A project is the ordinary answer: it stores the path of the checkout it
    /// was named from, which is exactly the repository to add to. The fallback
    /// is a `repo` that is itself a path on this machine, which is what `sbx new
    /// --worktree ~/dev/thing` means. A clone URL alone cannot answer it -- the
    /// point of a worktree is to share an object store that already exists, and
    /// nothing here will clone one to make that true.
    fn source_checkout(&self, session: &Session) -> Result<PathBuf> {
        if let Some(name) = &session.project
            && let Some(p) = projects::list().into_iter().find(|p| &p.name == name)
        {
            return Ok(PathBuf::from(p.path));
        }
        let direct = PathBuf::from(&session.repo);
        if direct.join(".git").exists() {
            return Ok(direct);
        }
        Err(Error::Local(format!(
            "a worktree session needs a checkout on the server to add to. \
             `{}` is not one, and {}. Start it in a project, or give a path \
             on this machine.",
            session.repo,
            match &session.project {
                Some(p) => format!("project `{p}` is not there either"),
                None => "no project was named".to_string(),
            }
        )))
    }

    /// Run something on the server, and report it the way an exec reports.
    ///
    /// The two failures are kept apart on purpose. A command that ran and
    /// returned non-zero is an [`ExecOutput`] with that code, exactly as a
    /// sandbox exec would be -- every caller above already reads git's own words
    /// out of one. A command that could not be *started* is an error, because
    /// no git ran and there is nothing to quote.
    fn run(&self, argv: &[&str]) -> Result<ExecOutput> {
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| Error::Local("nothing to run".into()))?;
        let out = Command::new(bin)
            // An explicit directory rather than whatever the server was started
            // in: sbxd is long-lived and its working directory may have been
            // deleted under it, which makes every spawn fail for a reason that
            // has nothing to do with the command. Every script says where it
            // wants to be anyway.
            .current_dir(root_dir())
            .args(args)
            .output()
            .map_err(|e| Error::Local(format!("could not run `{bin}`: {e}")))?;
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: out.status.code().unwrap_or(-1),
        })
    }

    /// Stop the agent and every shell beside it.
    ///
    /// Before the worktree goes, because they are running *in* it: a tmux
    /// session whose directory has been deleted is a pane that cannot be used
    /// and will not go away on its own.
    fn kill_tmux(&self, session: &Session) {
        let prefix = self.shell_prefix(session);
        let script = format!(
            "for s in $({tmux} list-sessions -F '#{{session_name}}' 2>/dev/null); do \
               case \"$s\" in {agent}|{prefix}*) {tmux} kill-session -t \"$s\" 2>/dev/null || true;; esac; \
             done",
            tmux = self.tmux(),
            agent = session.tmux,
            prefix = prefix,
        );
        let _ = self.run(&["sh", "-c", &script]);
    }
}

/// The directory a server-side command is spawned in. Its own function so the
/// one place it is decided is visible; nothing runs here, every script cds.
fn root_dir() -> PathBuf {
    PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
}

/// The tmux session name for a worktree session's agent.
///
/// `sbx-` prefixed for the same reason the sandbox names are: on a machine
/// where someone also uses tmux themselves, ours have to be recognisable in
/// their `tmux ls` -- and theirs must not be mistaken for ours.
pub fn tmux_name(name: &str) -> String {
    format!("sbx-{name}")
}

impl Backend for Worktree {
    fn isolation(&self) -> Isolation {
        Isolation::None
    }

    fn kind(&self) -> session::Kind {
        session::Kind::Worktree
    }

    fn paths(&self, session: &Session) -> Paths {
        Paths {
            repo: self.dir(session).display().to_string(),
            sbx: self.record_dir(&session.name).display().to_string(),
        }
    }

    fn exec(&self, _session: &Session, argv: &[&str]) -> Result<ExecOutput> {
        self.run(argv)
    }

    fn interactive_argv(&self, _session: &Session, argv: &[&str]) -> Result<Vec<String>> {
        // Nothing to wrap it in: what a terminal attaches to here is the
        // command itself, spawned under a pty by whoever asked.
        Ok(argv.iter().map(|a| (*a).to_string()).collect())
    }

    /// No `-f`: this is the server user's own tmux, and overriding their config
    /// would be this tool deciding how their terminal behaves. The locale is
    /// still set, because sbxd's environment is not a login shell's and tmux
    /// with no UTF-8 locale draws an agent's glyphs as `_`.
    fn tmux(&self) -> &'static str {
        "LANG=C.UTF-8 LC_ALL=C.UTF-8 COLORTERM=truecolor tmux -u"
    }

    fn shell_prefix(&self, session: &Session) -> String {
        format!("{}-shell-", session.tmux)
    }

    fn place(&self, session: &mut Session, _draft: &Draft) -> Result<()> {
        // Named before anything else, because everything below is derived from
        // it and because `agent` -- what a sandboxed session calls its tmux
        // session -- would be one name shared by every worktree on the machine.
        session.tmux = tmux_name(&session.name);
        // There is no policy. Left as `None` rather than filled with the
        // draft's, which would be a record claiming a guarantee that is not
        // there.
        session.policy = None;
        session.providers.clear();

        let checkout = self.source_checkout(session)?;
        let dir = self.root.join(&session.name);
        if dir.exists() {
            return Err(Error::Local(format!(
                "`{}` already exists; remove it or pick another name",
                dir.display()
            )));
        }
        std::fs::create_dir_all(&self.root).map_err(Error::local)?;
        std::fs::create_dir_all(self.record_dir(&session.name)).map_err(Error::local)?;

        session.workdir = Some(dir.display().to_string());
        // Checked here so a checkout that has gone fails against the request
        // that named it, rather than inside a detached seeder.
        if !checkout.join(".git").exists() {
            return Err(Error::Local(format!(
                "`{}` is not a git checkout",
                checkout.display()
            )));
        }

        // The branch the checkout is on, when nothing else said.
        //
        // A sandboxed session can leave this `None` and mean "whatever the
        // remote's default is", because a clone writes `origin/HEAD` and the
        // diff resolves it later. A worktree is added to a checkout that may
        // have no remote at all, so the base has to be decided here, from the
        // thing being branched from -- otherwise there is nothing for the diff
        // to measure against and the pane says so on every session.
        if session.base_branch.is_none() {
            let out = self.run(&[
                "git",
                "-C",
                &checkout.display().to_string(),
                "rev-parse",
                "--abbrev-ref",
                "HEAD",
            ])?;
            let branch = out.trimmed().trim().to_string();
            // Not a detached HEAD, and not the branch this session is about to
            // cut: either would make the diff measure work against itself.
            if out.ok() && !branch.is_empty() && branch != "HEAD" && branch != session.work_branch {
                session.base_branch = Some(branch);
            }
        }
        Ok(())
    }

    fn configure(&self, _s: &Session, _d: &Draft, _w: &mut Vec<String>) -> Result<()> {
        // Nothing to impose. The endpoint lists, the MCP grants and the
        // toolchain registries are all instructions to a gateway, and there is
        // no gateway in this path -- which is what `Isolation::None` says.
        Ok(())
    }

    /// Add the worktree, on the branch this session works on.
    ///
    /// Idempotent in both directions, because re-seeding is how a half-created
    /// session is repaired: an existing worktree is reused, and an existing
    /// branch is checked out rather than re-created.
    ///
    /// The fetch is best-effort and deliberate: without it the base is whatever
    /// the checkout last pulled, and a session branched from a week-old `main`
    /// is a merge conflict scheduled for later. It touches the person's own
    /// checkout, which is why it is a fetch and never anything that moves a ref
    /// they have.
    fn fetch_script(&self, session: &Session) -> String {
        let checkout = match self.source_checkout(session) {
            Ok(p) => p.display().to_string(),
            // The seeder is a script and has nowhere to return an error to, so
            // the failure is the script's: it exits non-zero with the reason,
            // and the seeder's own handler reports it as a failed step.
            Err(e) => {
                return format!("printf '%s\\n' {} >&2\nexit 1\n", sh_quote(&e.to_string()));
            }
        };
        let base = match &session.base_branch {
            Some(b) => format!("origin/{b}"),
            None => String::new(),
        };
        format!(
            r#"if [ -e {dir}/.git ]; then
  cd {dir}
  git checkout {branch} 2>/dev/null || git checkout -b {branch}
else
  cd {checkout}
  git fetch --prune origin >/dev/null 2>&1 || true
  base={base}
  if [ -z "$base" ]; then
    # `|| true`, because the seeder runs under `set -e` and this legitimately
    # finds nothing: a checkout with no remote -- or one whose origin has no
    # HEAD -- has no default branch to name, and the fallback below is HEAD.
    # Without it the script dies here having printed nothing at all.
    base=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)
  fi
  if [ -n "$base" ]; then
    git rev-parse --verify --quiet "$base" >/dev/null 2>&1 || base=''
  fi
  if [ -z "$base" ]; then base=HEAD; fi
  if git show-ref --verify --quiet refs/heads/{branch}; then
    git worktree add {dir} {branch}
  else
    git worktree add -b {branch} {dir} "$base"
  fi
fi
"#,
            dir = sh_quote(&self.dir(session).display().to_string()),
            checkout = sh_quote(&checkout),
            branch = sh_quote(&session.work_branch),
            base = sh_quote(&base),
        )
    }

    /// Take the worktree away, and the agent with it.
    ///
    /// `--force` because the point of removing a session is removing it: git
    /// refuses a worktree with modifications, and a worktree with modifications
    /// is what every session that did any work is. The branch is left alone --
    /// it is where the commits are, and this is not the command for deleting
    /// work that was already pushed or is still wanted.
    fn tear_down(&self, name: &str, session: Option<&Session>) -> Result<Torn> {
        // A record is the only way to know where the worktree is: unlike a
        // sandbox name, the directory is not a pure function of the session's
        // name once a root has been reconfigured.
        let Some(session) = session else {
            return Ok(Torn::RecordOnly);
        };
        self.kill_tmux(session);

        let dir = self.dir(session);
        let record = self.record_dir(name);
        let existed = dir.exists();
        if existed {
            // From inside the worktree: `git -C <worktree> worktree remove .`
            // is refused, so the removal is asked of the repository the worktree
            // belongs to -- which git itself resolves from the worktree, so no
            // record of the original checkout is needed.
            let script = format!(
                "main=$(git -C {dir} rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 1
git -C \"$main/..\" worktree remove --force {dir} || rm -rf {dir}
git -C \"$main/..\" worktree prune >/dev/null 2>&1 || true",
                dir = sh_quote(&dir.display().to_string()),
            );
            let out = self.run(&["sh", "-c", &script])?;
            if !out.ok() {
                // A directory that is no longer a worktree of anything -- the
                // checkout was deleted, or someone pruned it -- is still this
                // session's to remove.
                std::fs::remove_dir_all(&dir).map_err(Error::local)?;
            }
        }
        // The record goes either way: it describes a session that is ending.
        let _ = std::fs::remove_dir_all(&record);
        Ok(if existed {
            Torn::Removed
        } else {
            Torn::RecordOnly
        })
    }

    /// Alive is "the directory is still there".
    ///
    /// Weaker than the gateway's phase and honestly so: nothing is watching a
    /// worktree, and the ways it ends are someone deleting it, a `git worktree
    /// remove` elsewhere, or a disk that was never mounted. All three look the
    /// same from here, and all three mean the session is gone.
    fn live(&self, cached: Vec<Session>) -> Result<store::Reconciliation> {
        let mut out = store::Reconciliation::default();
        for mut session in cached {
            if self.dir(&session).exists() {
                // Back from the dead is a real case: a server restarted with a
                // volume that had not finished mounting reported every session
                // gone, and they were all still there.
                if session.state == State::Dead {
                    session.state = State::Ready;
                }
            } else {
                if session.state != State::Dead {
                    out.dead.push(session.name.clone());
                }
                session.state = State::Dead;
            }
            out.sessions.push(session);
        }

        let known: Vec<&str> = out.sessions.iter().map(|s| s.name.as_str()).collect();
        // The records are what is scanned, not the worktrees: a directory under
        // the root says nothing about who made it, and the record is the thing
        // that claims a session.
        if let Ok(entries) = std::fs::read_dir(&self.state) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !known.contains(&name.as_str()) && entry.path().join("meta.json").is_file() {
                    out.orphans.push(name);
                }
            }
        }
        Ok(out)
    }

    fn read_meta(&self, name: &str) -> Result<Session> {
        let path = self.record_dir(name).join("meta.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Local(format!("could not read {}: {e}", path.display())))?;
        serde_json::from_str(&text)
            .map_err(|e| Error::Local(format!("{} is not a session record: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn session(name: &str, dir: &Path) -> Session {
        let mut s = Session::new(name.to_string(), "/nowhere".into(), String::new());
        s.backend = session::Kind::Worktree;
        s.tmux = tmux_name(name);
        s.workdir = Some(dir.display().to_string());
        s
    }

    fn backend(tmp: &Path) -> Worktree {
        Worktree::new(tmp.join("worktrees"), tmp.join("state"))
    }

    /// The whole reason the tmux name is not `agent`: one tmux server holds
    /// every worktree session on the machine, so a shared name is two sessions
    /// attaching to one agent.
    #[test]
    fn shells_are_named_per_session() {
        let tmp = std::env::temp_dir();
        let b = backend(&tmp);
        let one = session("alpha", &tmp);
        let two = session("beta", &tmp);
        assert_eq!(b.shell_prefix(&one), "sbx-alpha-shell-");
        assert_eq!(b.shell_prefix(&two), "sbx-beta-shell-");
        assert_ne!(one.tmux, two.tmux);
    }

    /// The record is deliberately not in the working copy: it would show up in
    /// every `git status` the agent runs and go with the first `git clean`.
    #[test]
    fn the_record_lives_outside_the_worktree() {
        let tmp = std::env::temp_dir().join("sbx-test-record");
        let b = backend(&tmp);
        let s = session("alpha", &tmp.join("worktrees").join("alpha"));
        let paths = b.paths(&s);
        assert!(
            !paths.meta().starts_with(&paths.repo),
            "{} is inside {}",
            paths.meta(),
            paths.repo
        );
        assert!(paths.meta().ends_with("state/alpha/meta.json"), "{paths:?}");
    }

    /// A worktree session promises nothing about the network, and saying so is
    /// the whole point of the labelling.
    #[test]
    fn it_says_it_is_not_isolated() {
        let b = backend(&std::env::temp_dir());
        assert_eq!(b.isolation(), Isolation::None);
        assert_eq!(b.kind(), session::Kind::Worktree);
        assert!(
            !b.seeds_tooling(),
            "a worktree agent reads the server's own"
        );
    }

    /// The fallback that makes `sbx new --worktree ~/dev/thing` work, and the
    /// error that has to be readable when there is nothing to add a worktree to.
    #[test]
    fn a_local_checkout_can_be_named_directly() {
        let tmp = std::env::temp_dir().join("sbx-test-checkout");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        let b = backend(&tmp);

        let mut s = session("alpha", &tmp);
        s.repo = tmp.display().to_string();
        assert_eq!(b.source_checkout(&s).unwrap(), tmp);

        s.repo = "https://github.com/example/thing.git".into();
        let err = b.source_checkout(&s).unwrap_err().to_string();
        assert!(err.contains("needs a checkout on the server"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Re-seeding is how a half-created session is repaired, so neither the
    /// worktree nor the branch may be a second create's problem.
    #[test]
    fn the_fetch_script_is_idempotent() {
        let tmp = std::env::temp_dir().join("sbx-test-fetch");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        let b = backend(&tmp);
        let mut s = session("alpha", &tmp.join("wt"));
        s.repo = tmp.display().to_string();

        let script = b.fetch_script(&s);
        assert!(script.contains("git checkout"), "{script}");
        assert!(script.contains("worktree add -b"), "{script}");
        assert!(
            script.contains("show-ref --verify --quiet refs/heads/"),
            "an existing branch has to be reused rather than re-created: {script}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

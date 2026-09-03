# Sessions without a sandbox

Every session up to here has been a sandbox: a container the gateway made, a
clone inside it, and a policy on everything that leaves. This page is about the
other kind — a session that is a `git worktree` on the machine running `sbxd`,
with no sandbox around it at all.

It exists because a clone is expensive and a worktree is not, and because there
are jobs the image cannot do. It is not the default, and it never will be: the
isolation is the product.

## What you give up

**Everything.** The agent runs as an ordinary process under the server's own
account. It reads that account's files, uses its git credentials, and reaches
whatever the network allows it to reach. There is no gateway in the path, so:

* **no policy.** `sbx policy <name>` says so rather than printing rules, and the
  window's policy pane says so where the rules would be.
* **no allow/deny feed.** Nothing is deciding anything, so there is nothing to
  report. `sbx events` and the events pane say that too.
* **no credential swap.** A provider is a secret the *gateway* substitutes into
  an outgoing request, which is what keeps a token off the sandbox filesystem.
  A worktree session pushes with the server's own git credentials, and its agent
  can read them.

The last one is the sharp edge, because the button is the same. `publish` on a
sandboxed session pushes from *inside* the sandbox with a header the gateway
fills in; on a worktree session it is a plain `git push` as the server's user.
Same outcome, materially different guarantee.

Everything that shows a session says which kind it is: a `worktree` badge in the
window's tree and beside the branch in the TUI, a `KIND` column in `sbx ls`, and
an `isolation` row in the facts pane.

## What you get

* **Seconds instead of minutes.** A worktree shares the checkout's object store.
  Nothing is fetched that the machine already has, and a repository whose clone
  takes four minutes is ready as fast as git can write the index.
* **The machine's own toolchains.** No image variant to build, no registry to
  open in a policy. If it builds in your shell, it builds in the worktree.
* **Unpushed history.** A sandbox clones `origin`, so a branch that has never
  been pushed cannot be its base. A worktree can start from any local ref.
* **Anything the sandbox cannot do**: a daemon, a device, a licensed SDK, a
  language server that needs the real filesystem.

What it does *not* share is the working copy. Uncommitted changes in the
checkout stay there; the worktree gets a clean tree at the base commit.

## Starting one

From the window: the **where it runs** choice at the top of the new-worktree
form, which spells out what picking it means and hides the fields — policy,
toolchains, credentials — that are instructions to a gateway that will not be
involved.

From the terminal:

```sh
sbx new --worktree --repo ~/dev/thing --task "add the changelog"
```

`--repo` has to be a checkout **on the machine that will run the session**,
because that is the repository the worktree is added to. A clone URL alone
cannot answer it: the point is to share an object store that already exists, and
nothing here will clone one to make that true. Started from a project in the
window, the project's own path is that checkout.

`--policy`, `--provider` and `--toolchain` are refused rather than ignored. A
session created with a policy flag that did nothing would be one whose owner
believes it is isolated.

## Where things are

| | |
| --- | --- |
| the working copy | `~/.local/share/sbx/worktrees/<name>`, or `worktree_root` in the config file |
| the record | `~/.local/state/sbx/worktrees/<name>/meta.json` |
| the agent | a tmux session named `sbx-<name>` on the server |
| its shells | `sbx-<name>-shell-1`, `-2`, … beside it |

**The record is deliberately not in the working copy.** A sandboxed session
keeps `meta.json` inside its own sandbox, which is what lets it survive losing
the local cache. A worktree has nowhere equivalent: `.sbx/` in the working copy
would appear in every `git status` the agent runs, in every diff you review, and
one `git clean -fdx` from being deleted. So it lives beside the server's other
state, and adoption after a lost cache is that directory reconciled against the
worktrees still on disk.

**The tmux names are the session's, not `agent`.** Every sandbox has a tmux
server to itself and can call the agent's session whatever it likes. Here they
share one with each other and with whatever you are running yourself, so a name
that was not the session's would mean two sessions attaching to one agent — and
your own tmux sessions showing up as a session's shells.

## Ending one

`sbx rm <name>`, or the window. It kills the agent and its shells, runs
`git worktree remove --force`, prunes, and drops the record.

`--force`, because the point of removing a session is removing it: git refuses a
worktree with modifications, and a worktree with modifications is what every
session that did any work is. **The branch is left alone.** It is where the
commits are, and this is not the command for deleting work.

A session whose record the cache has lost cannot be removed this way — unlike a
sandbox, whose name is derived from the session's, a worktree's directory is not
recoverable from the name once a root has been reconfigured. `sbx rm` drops the
record and says so; the directory is yours to remove.

## What is missing

* **No hook-driven status.** The image bakes in `sbx-status`, which is what
  makes a sandboxed session's `waiting` state exact. A worktree session's state
  comes from reading the agent's screen, which is what the terminal has always
  fallen back to and is good but not perfect.
* **Skills and MCP servers are not carried.** The agent is the server's own, so
  it reads that user's `~/.claude`. Nothing is copied in, and the session record
  says it was given none rather than claiming a copy it never had.
* **A project needs a remote.** `projects::add` refuses a checkout with no
  origin, because a project is also what a *sandboxed* session clones. A
  local-only repository can still host worktree sessions from the command line.

---

[← Documentation](README.md) · [README](../README.md)

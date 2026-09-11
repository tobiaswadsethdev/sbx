# Architecture

A tour of the code, for anyone about to change some of it. [PLAN.md](../PLAN.md)
has the decisions and the increments that got here; this is the map.

## The shape of it

```
      +-------------------------+     +---------------------------+
      |   sbxd (the CLI)        |     |  apps/desktop (webview)   |
      | ls | diff | policy      |     |  projects | tabs | dock   |
      |    | events | attach   |     +-------------+-------------+
      +-----------+-------------+                   | tauri commands
                  |                   +-------------+-------------+
                  |                   |  sbx-client (pinned TLS)  |
                  |                   +-------------+-------------+
                  |                                 | https + wss
                  |                   +-------------+-------------+
                  |                   |  sbxd  (/rpc, /ws)        |
                  |                   +-------------+-------------+
                  |                                 |
                +-+---------------------------------+-+
                |             sbx-core                |   nothing here draws
                |  ops, sessions, policy, events,     |
                |  seed, publish, git, files, projects|
                +-----------------+-------------------+
                                  |
                    +-------------+-------------+
                    |     Backend (a trait)     |   where a session runs
                    +------+-------------+------+
                           |             |
                   Sandboxed          Worktree
                           |             |
   SessionStore     openshell-client   git worktree + tmux
   (~/.config/sbx/  (CLI subprocess)   (on the server itself,
    sessions.json)         |            no isolation)
                    openshell gateway (docker driver)
                           |
        +------------------+-------------------+
        |                  |                   |
    sandbox A          sandbox B           sandbox C
   clone+agent        clone+agent         clone+agent
```

Five crates, and an application:

| | |
| --- | --- |
| `crates/openshell-client` | everything the rest of the tool knows about OpenShell, behind one trait. OpenShell is a fast-moving `0.0.x` project, so version churn lands in one file -- and the trait is what lets the gRPC API replace the subprocess later without touching callers |
| `crates/sbx-core` | everything `sbx` *does*: sessions, policy, events, seeding, publishing. No renderer may appear in it, which is what lets something other than a terminal sit on top |
| `crates/sbx-proto` | one definition of every message on the wire, so a server and a client cannot drift. Built on `sbx-core`, because the types it carries are the core's own rather than a second set kept in step by hand |
| `crates/sbxd` | the whole Linux side: the clap CLI, and the server behind TLS, one token check and `/rpc`. Async only where it has to be -- everything it calls is blocking and goes to `spawn_blocking`. `crates/sbx` carried the CLI and a ratatui TUI beside it until v0.4.0, when two front ends onto one set of sessions stopped earning their keep |
| `crates/sbx-client` | the client half: paired servers, and one certificate-pinned connection to each. Its own crate because the CLI and the desktop application both need it, and a webview cannot pin a certificate for itself |
| `apps/desktop` | Tauri v2 and React. Deliberately *not* a workspace member: `cargo build --workspace` would otherwise need a GUI toolkit installed to check that a session store reconciles |

## Where things live

The module docs at the top of each file are the real documentation; this is the
index into them.

Everything is in `sbx-core` unless the second column says otherwise.

| Module | What it owns |
| --- | --- |
| `main.rs` | *(sbx)* the clap CLI, and dispatch into `ops` |
| `ops.rs` | the operations the CLI and the window both need, so neither reimplements the other. Everything here takes a `Backend` rather than a gateway client |
| `backend.rs` | *where* a session runs, as a trait: the sandboxed one and the worktree one. `backend/sandboxed.rs` is what `ops` used to do directly; `backend/worktree.rs` is a `git worktree` on the server with no isolation at all, and says so. See [worktrees.md](worktrees.md) |
| `session.rs` | what a session *is*: identity, the derived branch and sandbox names, and the metadata record written inside the sandbox |
| `store.rs` | the local cache and its reconciliation against the gateway; every write is locked |
| `removed.rs` | the names of destroyed sessions, kept until the sandbox or worktree behind them has actually gone. Deletion is asynchronous, and a sandbox still listed with its record already dropped is the exact shape of an orphan worth adopting -- so without this a removed session came back on the next refresh, and the name could not be used again |
| `seed.rs` | the detached script that clones, cuts the branch, writes the record and starts the agent |
| `status.rs` | what the agent is doing, from hooks and from its screen |
| `policy.rs` | the templates, the mid-run widen/tighten, and `View`: the policy pane as facts, which each renderer words for itself |
| `endpoints.rs` | the global allow and block lists applied to every new session |
| `events.rs` | the allow/deny feed, merged and kept on disk per session |
| `forge.rs` | which git host a session works against, derived from the repo URL |
| `tracker.rs` | the task inbox: GitHub, Azure DevOps and Jira over REST, and the comment and transition a publish writes back. The parsers are pure and tested against captured answers, because reading somebody else's JSON is the part that is easy to get wrong quietly |
| `publish.rs` | push and open a pull request, both from inside the sandbox -- or, for a worktree session, with the server's own git credentials |
| `image.rs` | the sandbox image, with its whole build context embedded in the binary |
| `toolchain.rs` | the toolchains, their image variants, and the registry each one opens |
| `skills.rs` | packing host skills into a session, and the server-side library a client pushes its own into |
| `mcp.rs` | MCP servers on the host, and the endpoints granted for them. `mcp/managed.rs` is the half `sbxd` runs itself: an image and a port instead of a url, the container's lifecycle, and what Docker says about it |
| `secrets.rs` | the values a managed MCP container is given. In one way only: `get` is `pub(crate)`, so no request handler can reach a value |
| `integrations.rs` | the MCP catalog, the secret names and the skill library as one answer, which every action on that screen returns |
| `repos.rs` | the git repositories on the machine `sbxd` runs on -- the only module that reads that host's filesystem |
| `projects.rs` | the repositories someone has said they are working on, which is what worktrees are grouped under. A decision, not a discovery: stored rather than derived from the sessions that exist |
| `git.rs` | the working copy inside a sandbox as git describes it, and the operations on it. The status parser is pure, because git's output is the part that is easy to get subtly wrong and impossible to notice |
| `files.rs` | reading a worktree's files from outside it, one directory at a time. Read-only: the agent owns the working copy |
| `comments.rs` | review comments on a diff, kept per session until they go to the agent as one message |
| `config.rs` | `~/.config/sbx/config.toml`, and which default wins |
| `doctor.rs` | the preflight checks, each carrying its fix |
| `update.rs` | fetching, verifying and replacing the `sbxd` binary itself |
| `ansi.rs` | one tokenizer for captured screens, into style types of its own, shared by everything that shows them and the matcher that reads them |
| `pane.rs` | the markup the text panes share, so styling stays in one place |
| `attach.rs` | *(sbxd)* raw mode, and handing this terminal to the agent |
| `lib.rs` | *(sbx-client)* the servers this machine is paired with, pairing with one, and one request against one. `pair` is shared: `sbxd connect` and the desktop application's connect dialog are both it |
| `pin.rs` | *(sbx-client)* judging a server by its certificate's fingerprint and nothing else |
| `http.rs` | *(sbx-client)* enough HTTP/1.1 to ask an `sbxd` a question |
| `state.rs` | where secrets live: keys, tokens, and saved connections |
| `lib.rs` | *(sbx-proto)* every message on the wire, carrying the core's own types |
| `stream.rs` | *(sbx-proto)* the multiplexed websocket: the channels, and the frames both ends speak |
| `ws.rs` | *(sbx-client)* the streaming half, and the pty a terminal channel needs on this side of it |
| `rpc.rs` | *(sbxd)* one request in, one outcome out; every arm a call into `ops` |
| `serve.rs` | *(sbxd)* the routes, the token check, and keeping blocking work off the runtime |
| `stream.rs` | *(sbxd)* the channels a client subscribes to, and the pty behind a terminal |
| `App.tsx` | *(desktop)* the workspace: the project tree, the tabs and the dock |
| `charSize.ts` | *(desktop)* the font metrics WebKit gets wrong, and the probe that corrects them |
| `panes/` | *(desktop)* the terminal, a file in Monaco, and a file's diff with the review on it |

## Three rules worth knowing before you change anything

**Nothing in `sbx-core` may depend on a renderer.** It is the invariant the crate
split exists to hold, and it is load-bearing rather than tidy: a core that knows
about a renderer cannot be linked into a server. It is also easy to breach by
accident, because reaching for a colour type in a module that builds a pane body
is the obvious thing to do -- `ansi.rs` tokenizes into a `Style` of its own for
exactly that reason, and raw-mode attaching lives in `attach.rs` in the binary
rather than in the core.

The rule used to be tested by a ratatui TUI and a headless server both linking
the core. The TUI went in v0.4.0, so what enforces it now is the webview: a
renderer that cannot link Rust at all, and reaches the core only through
`sbx-proto`. That is a stricter test than the one it replaced.


**Nothing that paints does I/O.** Every gateway call is a subprocess round trip
costing hundreds of milliseconds, so none of them happen on a path that draws.
The window reaches them over `/rpc`, and `sbxd` runs each on `spawn_blocking`. A
feature that needs to ask the gateway something adds an op, not a call in a
handler that renders.

**The sandbox is the source of truth.** Seeding writes
`/sandbox/.sbx/meta.json`, so a session describes itself and survives losing the
local cache. (A worktree session has nowhere equivalent and keeps its record in
the server's state directory instead -- the one place this rule bends, and
[worktrees.md](worktrees.md) says why.) Labels carry identity only -- the gateway restricts label values to
Kubernetes rules, at most 63 characters of `[A-Za-z0-9._-]`, which cannot hold a
repo URL or a branch name with a `/` in it.

The local cache is disposable: each session's record lives inside its own
sandbox, so deleting `~/.config/sbx/sessions.json` and running `sbxd ls`
re-adopts everything still running. Every write to it is a locked
read-modify-write, because more than one writer is the normal case -- a window
reconciling the list every second while `sbxd new` in another terminal walks a
session from `seeding` to `ready`, which on a large repository takes minutes.

**Seeding runs inside the sandbox, detached from the command that asks for it.**
`sbxd new` writes a script into the sandbox, starts it with `setsid`, and then
only *watches* it: the clone, the work branch, the metadata record and the agent
all happen in there, and they finish whether or not the tool that started them is
still running. Killing `sbxd new` with `SIGKILL` two seconds into a clone leaves a
session that comes up complete on its own.

The seeder reports each step into `/sandbox/.sbx/seed.state` and its output to
`seed.log`, which is what makes the difference between the three things that used
to look identical from outside: still cloning, finished while nobody was looking,
and stopped. The first refresh of any `sbxd` command reads that file for a session
whose record still says `creating` or `seeding` and catches the record up --
`seed-kill: seeding -> ready (seeding finished)` -- or marks it failed with the
reason git gave. A seeder still running is left alone, however long it takes.

## Tests

The suite is hermetic on purpose, so a contributor with no gateway can still
change almost anything:

* pane classification runs against real captures in `crates/sbx-core/tests/panes/`,
  included at compile time by `status.rs`;

What that cannot cover is the gateway contract, which lives in ignored tests in
`crates/openshell-client/tests/live.rs` and needs a live gateway and Docker.
They create and delete real sandboxes labelled `sbx.test`, one at a time:

```sh
cargo test -p openshell-client -- --ignored --test-threads=1
```

[CONTRIBUTING.md](../CONTRIBUTING.md) has the rest of the development loop.

---

[← Documentation](README.md) · [README](../README.md)
